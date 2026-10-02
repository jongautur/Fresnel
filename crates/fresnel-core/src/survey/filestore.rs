//! Validated files in one directory owned by the app (floor plans, photos).
//!
//! The database only stores file names; a [`FileStore`] owns the files.
//! Content is identified by sniffing, never by the name the user gave it.
//! Files no row references are removed by [`FileStore::collect_garbage`].
//!
//! New files exist on disk before the database references them, so an
//! import (write, then reference) and garbage collection (read the
//! referenced set, then sweep the directory) run under one lock: otherwise a
//! collection running between the two steps of an import deletes the files
//! being imported. The public API only offers both as whole operations.
//!
//! On Windows, antivirus and indexers briefly open new files, which makes a
//! rename or delete fail with a sharing violation; those are retried a few
//! times with a short backoff.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::Utc;
use tracing::{info, warn};

use crate::error::{Result, WifiError};

/// A content type a store accepts.
#[derive(Debug)]
pub struct FileFormat {
    pub mime: &'static str,
    /// Without the dot.
    pub extension: &'static str,
    /// Does `bytes` look like this format? Checks content, not names.
    pub sniff: fn(&[u8]) -> bool,
}

/// What a store holds and accepts.
#[derive(Debug, Clone, Copy)]
pub struct StoreSpec {
    /// Every file name starts with this; garbage collection never touches
    /// anything else in the directory.
    pub prefix: &'static str,
    /// Per file.
    pub max_bytes: usize,
    pub formats: &'static [&'static FileFormat],
    /// What is stored, for messages ("floor plan", "photo").
    pub label: &'static str,
    /// The message when no format matches.
    pub unsupported: &'static str,
}

/// One file of a group imported together. `suffix` tells the files of a
/// group apart: "" for the main one, else `-` and lowercase letters
/// (e.g. "-thumb").
#[derive(Debug, Clone, Copy)]
pub struct NewFile<'a> {
    pub suffix: &'static str,
    pub bytes: &'a [u8],
}

/// A file written by [`FileStore::import`].
#[derive(Debug, Clone)]
pub struct StoredFile {
    /// File name only (never a path).
    pub name: String,
    pub format: &'static FileFormat,
}

pub struct FileStore {
    dir: PathBuf,
    spec: StoreSpec,
    /// Serialises "write files, then reference them" against garbage
    /// collection. Guards no data, only the window in between.
    lock: Mutex<()>,
}

/// Tries for a rename or delete that fails with a (probably) transient error.
const IO_ATTEMPTS: u32 = 5;
const IO_FIRST_BACKOFF: Duration = Duration::from_millis(25);

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>, spec: StoreSpec) -> Self {
        Self {
            dir: dir.into(),
            spec,
            lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn spec(&self) -> &StoreSpec {
        &self.spec
    }

    /// The accepted format `bytes` are in, if any.
    pub fn sniff(&self, bytes: &[u8]) -> Option<&'static FileFormat> {
        self.spec.formats.iter().copied().find(|f| (f.sniff)(bytes))
    }

    /// Size and format checks every stored file passes.
    pub fn check(&self, bytes: &[u8]) -> Result<&'static FileFormat> {
        let label = self.spec.label;
        if bytes.is_empty() {
            return Err(WifiError::InvalidInput(format!(
                "the {label} file is empty"
            )));
        }
        if bytes.len() > self.spec.max_bytes {
            return Err(WifiError::InvalidInput(format!(
                "the {label} is {} MB; the limit is {} MB",
                bytes.len().div_ceil(1024 * 1024),
                self.spec.max_bytes / (1024 * 1024)
            )));
        }
        self.sniff(bytes)
            .ok_or_else(|| WifiError::InvalidInput(self.spec.unsupported.into()))
    }

    /// Validate and store a group of files, then let `reference` record
    /// them (e.g. in the database), all without garbage collection in
    /// between. `reference` gets the stored files in the order given and
    /// returns its result plus the names of files the new ones replace;
    /// those are deleted. If anything fails, all new files are.
    pub fn import<T>(
        &self,
        files: &[NewFile<'_>],
        reference: impl FnOnce(&[StoredFile]) -> Result<(T, Vec<String>)>,
    ) -> Result<T> {
        let formats = files
            .iter()
            .map(|f| self.check(f.bytes))
            .collect::<Result<Vec<_>>>()?;
        if files.is_empty()
            || files.iter().any(|f| {
                !(f.suffix.is_empty()
                    || f.suffix.len() > 1
                        && f.suffix.starts_with('-')
                        && f.suffix[1..].bytes().all(|b| b.is_ascii_lowercase()))
            })
        {
            return Err(WifiError::Backend(format!(
                "invalid {} file group",
                self.spec.label
            )));
        }

        let _guard = self.lock();
        let stored = self.write_group(files, &formats)?;
        let names: Vec<String> = stored.iter().map(|s| s.name.clone()).collect();
        match reference(&stored) {
            Ok((value, replaced)) => {
                for old in replaced.iter().filter(|old| !names.contains(old)) {
                    self.remove(old);
                }
                Ok(value)
            }
            Err(e) => {
                for name in &names {
                    self.remove(name);
                }
                Err(e)
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // The lock guards no data, so a panic while holding it leaves
        // nothing inconsistent.
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Only call with the lock held (see [`Self::import`]).
    fn write_group(
        &self,
        files: &[NewFile<'_>],
        formats: &[&'static FileFormat],
    ) -> Result<Vec<StoredFile>> {
        fs::create_dir_all(&self.dir).map_err(|e| io_error("create", &self.dir, e))?;
        let stamp = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let prefix = self.spec.prefix;
        let names = (0..100)
            .map(|n| {
                files
                    .iter()
                    .zip(formats)
                    .map(|(f, fmt)| format!("{prefix}{stamp}-{n}{}.{}", f.suffix, fmt.extension))
                    .collect::<Vec<_>>()
            })
            .find(|names| names.iter().all(|name| !self.dir.join(name).exists()))
            .ok_or_else(|| {
                WifiError::Backend(format!("could not pick a {} file name", self.spec.label))
            })?;

        let mut written: Vec<String> = Vec::new();
        for ((file, name), format) in files.iter().zip(&names).zip(formats) {
            let path = self.dir.join(name);
            if let Err(e) = write_atomically(&path, file.bytes) {
                for done in &written {
                    self.remove(done);
                }
                return Err(io_error("write", &path, e));
            }
            written.push(name.clone());
            info!(file = %name, bytes = file.bytes.len(), mime = format.mime, "{} stored", self.spec.label);
        }
        // Make the renames themselves durable. Windows can't open a
        // directory as a file to sync it; NTFS journals the rename.
        #[cfg(unix)]
        if let Err(e) = fs::File::open(&self.dir).and_then(|d| d.sync_all()) {
            for done in &written {
                self.remove(done);
            }
            return Err(io_error("sync", &self.dir, e));
        }
        Ok(names
            .into_iter()
            .zip(formats)
            .map(|(name, format)| StoredFile { name, format })
            .collect())
    }

    pub fn read(&self, file: &str) -> Result<Vec<u8>> {
        let path = self.path_of(file)?;
        fs::read(&path).map_err(|e| io_error("read", &path, e))
    }

    fn remove(&self, file: &str) {
        if let Ok(path) = self.path_of(file) {
            if let Err(e) = retry(|| fs::remove_file(&path)) {
                if e.kind() != io::ErrorKind::NotFound {
                    warn!(path = %path.display(), error = %e, "could not delete {}", self.spec.label);
                }
            }
        }
    }

    /// Delete files (and leftover temp files) no row references.
    /// `referenced` reads the set of referenced file names; it runs under
    /// the store's lock, so no import can slip in between reading the set
    /// and sweeping the directory. Only touches files with this store's
    /// prefix. Returns how many were removed.
    pub fn collect_garbage(
        &self,
        referenced: impl FnOnce() -> Result<HashSet<String>>,
    ) -> Result<usize> {
        let _guard = self.lock();
        let referenced = referenced()?;
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(self.spec.prefix) && !referenced.contains(&name) {
                match retry(|| fs::remove_file(entry.path())) {
                    Ok(()) => removed += 1,
                    Err(e) => {
                        warn!(file = %name, error = %e, "could not delete unused {}", self.spec.label)
                    }
                }
            }
        }
        if removed > 0 {
            info!(removed, "removed unused {} files", self.spec.label);
        }
        Ok(removed)
    }

    /// Resolve a stored file name, refusing anything that isn't a bare
    /// name this store could have produced.
    fn path_of(&self, file: &str) -> Result<PathBuf> {
        let ok = file.starts_with(self.spec.prefix)
            && !file.contains(['/', '\\', ':'])
            && !file.contains("..")
            && file.len() < 128;
        if !ok {
            return Err(WifiError::InvalidInput(format!(
                "invalid {} reference '{file}'",
                self.spec.label
            )));
        }
        Ok(self.dir.join(file))
    }
}

/// Write then rename, so a crash never leaves a half-written file under the
/// final name.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let write = || -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        retry(|| fs::rename(&tmp, path))
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
        let _ = fs::remove_file(path);
    })
}

/// Errors that may pass on their own: on Windows another process (antivirus,
/// search indexer) holding the file open.
fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy
    ) || (cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33))) // sharing / lock violation
}

fn retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut backoff = IO_FIRST_BACKOFF;
    for _ in 1..IO_ATTEMPTS {
        match op() {
            Err(e) if is_transient(&e) => {
                std::thread::sleep(backoff);
                backoff *= 2;
            }
            result => return result,
        }
    }
    op()
}

fn io_error(action: &str, path: &Path, e: io::Error) -> WifiError {
    WifiError::Backend(format!("cannot {action} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEXT: FileFormat = FileFormat {
        mime: "text/plain",
        extension: "txt",
        sniff: |b| b.starts_with(b"T:"),
    };
    static BIN: FileFormat = FileFormat {
        mime: "application/octet-stream",
        extension: "bin",
        sniff: |b| b.starts_with(b"B:"),
    };
    const SPEC: StoreSpec = StoreSpec {
        prefix: "item-",
        max_bytes: 16,
        formats: &[&TEXT, &BIN],
        label: "item",
        unsupported: "unsupported item",
    };

    fn temp_store(name: &str) -> (PathBuf, FileStore) {
        let dir = std::env::temp_dir().join(format!("fresnel-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (dir.clone(), FileStore::new(dir, SPEC))
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn checks_size_and_format() {
        let (_, store) = temp_store("check");
        assert_eq!(store.check(b"T:hello").unwrap().extension, "txt");
        assert_eq!(store.check(b"B:\x00").unwrap().extension, "bin");
        let msg = |b: &[u8]| store.check(b).unwrap_err().to_string();
        assert!(msg(b"").contains("empty"));
        assert!(msg(b"T:0123456789abcdef").contains("limit"));
        assert!(msg(b"X:").contains("unsupported item"));
    }

    #[test]
    fn group_import_names_formats_and_rollback() {
        let (dir, store) = temp_store("group");
        let files = [
            NewFile {
                suffix: "",
                bytes: b"B:orig",
            },
            NewFile {
                suffix: "-thumb",
                bytes: b"T:small",
            },
        ];
        let stored = store
            .import(&files, |s| Ok((s.to_vec(), Vec::new())))
            .unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored[0].name.starts_with("item-") && stored[0].name.ends_with(".bin"));
        let stem = stored[0].name.trim_end_matches(".bin");
        assert_eq!(stored[1].name, format!("{stem}-thumb.txt"));
        assert_eq!(store.read(&stored[1].name).unwrap(), b"T:small");
        assert!(store.read("item-../x").is_err());
        assert!(store.read("other.txt").is_err());

        // One bad file refuses the whole group, before anything is written.
        let bad = [
            files[0],
            NewFile {
                suffix: "-x",
                bytes: b"??",
            },
        ];
        assert!(store.import(&bad, |_| Ok(((), Vec::new()))).is_err());
        let bad_suffix = [NewFile {
            suffix: "_x",
            bytes: b"T:",
        }];
        assert!(store.import(&bad_suffix, |_| Ok(((), Vec::new()))).is_err());
        assert_eq!(names(&dir).len(), 2);

        // A failing reference removes every new file.
        let err = store.import(&files, |_| -> Result<((), _)> {
            Err(WifiError::Database("busy".into()))
        });
        assert!(err.is_err());
        assert_eq!(names(&dir).len(), 2);

        // Replaced files go; the new ones are never deleted as "replaced".
        let next = store
            .import(&files[..1], |s| {
                Ok((
                    s[0].name.clone(),
                    vec![stored[0].name.clone(), s[0].name.clone()],
                ))
            })
            .unwrap();
        assert!(store.read(&stored[0].name).is_err());
        assert!(store.read(&next).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_collection_keeps_referenced_and_foreign_files() {
        let (dir, store) = temp_store("gc");
        let files = [
            NewFile {
                suffix: "",
                bytes: b"T:a",
            },
            NewFile {
                suffix: "-report",
                bytes: b"T:b",
            },
        ];
        let keep = store
            .import(&files, |s| Ok((s.to_vec(), Vec::new())))
            .unwrap();
        let drop_ = store
            .import(&files, |s| Ok((s.to_vec(), Vec::new())))
            .unwrap();
        fs::write(dir.join("item-leftover.tmp"), b"x").unwrap();
        fs::write(dir.join("unrelated.txt"), b"x").unwrap();

        let referenced: HashSet<String> = keep.iter().map(|s| s.name.clone()).collect();
        assert_eq!(store.collect_garbage(|| Ok(referenced)).unwrap(), 3);
        for s in &keep {
            assert!(store.read(&s.name).is_ok());
        }
        for s in &drop_ {
            assert!(store.read(&s.name).is_err());
        }
        assert!(dir.join("unrelated.txt").exists());
        assert!(store
            .collect_garbage(|| Err(WifiError::Database("busy".into())))
            .is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_gives_up_on_permanent_errors_at_once() {
        let mut calls = 0;
        let r: io::Result<()> = retry(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);

        let mut calls = 0;
        let r = retry(|| {
            calls += 1;
            if calls < 3 {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(r.unwrap(), 3);
    }
}
