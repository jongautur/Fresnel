//! Floor plan image files.
//!
//! Imported plans are copied into one directory owned by the app (the
//! project must not break when the original file moves); the database only
//! stores the file name. Files no floor references are removed by
//! [`PlanStore::collect_garbage`].
//!
//! A new plan file exists on disk before the database references it, so an
//! import (store, then reference) and garbage collection (read the referenced
//! set, then sweep the directory) run under one lock in the store: otherwise
//! a collection running between the two steps of an import deletes the plan
//! being imported. The public API only offers both as whole operations.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::Utc;
use tracing::{info, warn};

use super::models::FloorPlan;
use crate::error::{Result, WifiError};

pub const MAX_PLAN_BYTES: usize = 64 * 1024 * 1024;
/// Upper bound for a plan's side in px (sanity check, not a format limit).
const MAX_PLAN_SIDE: f64 = 50_000.0;
const PREFIX: &str = "plan-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFormat {
    Png,
    Jpeg,
    Svg,
}

impl PlanFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Svg => "image/svg+xml",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Svg => "svg",
        }
    }

    /// Identify the format from the content, never from the file name.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Self::Jpeg);
        }
        let head = &bytes[..bytes.len().min(4096)];
        let text = String::from_utf8_lossy(head);
        let text = text.trim_start_matches('\u{feff}').trim_start();
        if (text.starts_with("<?xml") || text.starts_with("<svg") || text.starts_with("<!--"))
            && text.contains("<svg")
        {
            return Some(Self::Svg);
        }
        None
    }
}

pub struct PlanStore {
    dir: PathBuf,
    /// Serialises "write a file, then reference it" against garbage
    /// collection. Guards no data, only the window in between.
    lock: Mutex<()>,
}

impl PlanStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Validate and store an image, then let `reference` record it (e.g.
    /// attach it to a floor), all without garbage collection in between.
    /// `reference` returns its result and the plan file the new one replaces,
    /// if any; that file is deleted. If `reference` fails, the new file is.
    ///
    /// `width`/`height` are the image's natural size as the UI renders it
    /// (which accounts for e.g. JPEG EXIF rotation and SVG viewBoxes); that
    /// rendered size is the survey coordinate space.
    pub fn import<T>(
        &self,
        bytes: &[u8],
        width: f64,
        height: f64,
        reference: impl FnOnce(&FloorPlan) -> Result<(T, Option<String>)>,
    ) -> Result<T> {
        let _guard = self.lock();
        let plan = self.save(bytes, width, height)?;
        match reference(&plan) {
            Ok((value, previous)) => {
                if let Some(old) = previous.filter(|old| *old != plan.file) {
                    self.remove(&old);
                }
                Ok(value)
            }
            Err(e) => {
                self.remove(&plan.file);
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
    fn save(&self, bytes: &[u8], width: f64, height: f64) -> Result<FloorPlan> {
        if bytes.is_empty() {
            return Err(WifiError::InvalidInput(
                "the floor plan file is empty".into(),
            ));
        }
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(WifiError::InvalidInput(format!(
                "the floor plan is {} MB; the limit is {} MB",
                bytes.len() / (1024 * 1024),
                MAX_PLAN_BYTES / (1024 * 1024)
            )));
        }
        let format = PlanFormat::sniff(bytes).ok_or_else(|| {
            WifiError::InvalidInput(
                "unsupported floor plan format; use PNG, JPEG or SVG (export PDFs to PNG first)"
                    .into(),
            )
        })?;
        let valid = |v: f64| v.is_finite() && (1.0..=MAX_PLAN_SIDE).contains(&v);
        if !valid(width) || !valid(height) {
            return Err(WifiError::InvalidInput(format!(
                "the floor plan's size ({width} × {height} px) is not usable"
            )));
        }

        fs::create_dir_all(&self.dir).map_err(|e| io_error("create", &self.dir, e))?;
        let stamp = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let (name, path) = (0..100)
            .map(|n| {
                let name = format!("{PREFIX}{stamp}-{n}.{}", format.extension());
                let path = self.dir.join(&name);
                (name, path)
            })
            .find(|(_, p)| !p.exists())
            .ok_or_else(|| WifiError::Backend("could not pick a floor plan file name".into()))?;

        // Write then rename, so a crash never leaves a half-written plan.
        let tmp = path.with_extension("tmp");
        let write = || -> std::io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            fs::rename(&tmp, &path)?;
            // Make the rename itself durable. Windows can't open a directory
            // as a file to sync it; NTFS journals the rename.
            #[cfg(unix)]
            fs::File::open(&self.dir)?.sync_all()?;
            Ok(())
        };
        if let Err(e) = write() {
            let _ = fs::remove_file(&tmp);
            let _ = fs::remove_file(&path);
            return Err(io_error("write", &path, e));
        }
        info!(file = %name, bytes = bytes.len(), ?format, "floor plan stored");
        Ok(FloorPlan {
            file: name,
            mime: format.mime().into(),
            width,
            height,
        })
    }

    pub fn read(&self, file: &str) -> Result<Vec<u8>> {
        let path = self.path_of(file)?;
        fs::read(&path).map_err(|e| io_error("read", &path, e))
    }

    fn remove(&self, file: &str) {
        if let Ok(path) = self.path_of(file) {
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(path = %path.display(), error = %e, "could not delete floor plan");
                }
            }
        }
    }

    /// Delete plan files (and leftover temp files) no floor references.
    /// `referenced` reads the set of referenced file names; it runs under
    /// the store's lock, so no import can slip in between reading the set
    /// and sweeping the directory. Only touches files this store created.
    /// Returns how many were removed.
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
            if name.starts_with(PREFIX) && !referenced.contains(&name) {
                match fs::remove_file(entry.path()) {
                    Ok(()) => removed += 1,
                    Err(e) => warn!(file = %name, error = %e, "could not delete unused floor plan"),
                }
            }
        }
        if removed > 0 {
            info!(removed, "removed unused floor plan files");
        }
        Ok(removed)
    }

    /// Resolve a stored file name, refusing anything that isn't a bare
    /// name this store could have produced.
    fn path_of(&self, file: &str) -> Result<PathBuf> {
        let ok = file.starts_with(PREFIX)
            && !file.contains(['/', '\\'])
            && !file.contains("..")
            && file.len() < 128;
        if !ok {
            return Err(WifiError::InvalidInput(format!(
                "invalid floor plan reference '{file}'"
            )));
        }
        Ok(self.dir.join(file))
    }
}

fn io_error(action: &str, path: &Path, e: std::io::Error) -> WifiError {
    WifiError::Backend(format!("cannot {action} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_formats() {
        assert_eq!(
            PlanFormat::sniff(b"\x89PNG\r\n\x1a\n...."),
            Some(PlanFormat::Png)
        );
        assert_eq!(
            PlanFormat::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(PlanFormat::Jpeg)
        );
        assert_eq!(
            PlanFormat::sniff(b"\xef\xbb\xbf  <?xml version=\"1.0\"?>\n<svg xmlns=..."),
            Some(PlanFormat::Svg)
        );
        assert_eq!(
            PlanFormat::sniff(b"<svg viewBox='0 0 1 1'/>"),
            Some(PlanFormat::Svg)
        );
        assert_eq!(PlanFormat::sniff(b"%PDF-1.7"), None);
        assert_eq!(PlanFormat::sniff(b"<html><svg/></html>"), None);
        assert_eq!(PlanFormat::sniff(b""), None);
    }

    fn temp_store(name: &str) -> (PathBuf, PlanStore) {
        let dir = std::env::temp_dir().join(format!("fresnel-plans-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (dir.clone(), PlanStore::new(dir))
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";

    /// Import without a database: the reference step just reports the plan.
    fn import(store: &PlanStore, bytes: &[u8], w: f64, h: f64) -> Result<FloorPlan> {
        store.import(bytes, w, h, |plan| Ok((plan.clone(), None)))
    }

    #[test]
    fn import_read_and_collect() {
        let (dir, store) = temp_store("collect");
        let plan = import(&store, PNG, 800.0, 600.0).unwrap();
        assert_eq!(plan.mime, "image/png");
        assert_eq!(store.read(&plan.file).unwrap(), PNG);

        assert!(store.read("../etc/passwd").is_err());
        assert!(import(&store, b"%PDF", 10.0, 10.0).is_err());
        assert!(import(&store, PNG, 0.0, 10.0).is_err());

        let keep: HashSet<String> = [plan.file.clone()].into();
        let other = import(&store, PNG, 10.0, 10.0).unwrap();
        fs::write(dir.join("unrelated.txt"), b"x").unwrap();
        assert_eq!(store.collect_garbage(|| Ok(keep)).unwrap(), 1);
        assert!(store.read(&other.file).is_err());
        assert!(store.read(&plan.file).is_ok());
        assert!(dir.join("unrelated.txt").exists());

        // A failing reference read sweeps nothing.
        let err = store.collect_garbage(|| Err(WifiError::Database("busy".into())));
        assert!(err.is_err());
        assert!(store.read(&plan.file).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_replaces_or_rolls_back() {
        let (dir, store) = temp_store("replace");
        let first = import(&store, PNG, 10.0, 10.0).unwrap();
        // The reference step reports the plan it replaced: that file goes.
        let second = store
            .import(PNG, 10.0, 10.0, |plan| {
                Ok((plan.clone(), Some(first.file.clone())))
            })
            .unwrap();
        assert!(store.read(&first.file).is_err());
        assert!(store.read(&second.file).is_ok());
        // The reference step fails: the new file goes, the old one stays.
        let mut attempted = None;
        let err = store.import(PNG, 10.0, 10.0, |plan| -> Result<((), _)> {
            attempted = Some(plan.file.clone());
            Err(WifiError::InvalidInput("floor has points".into()))
        });
        assert!(err.is_err());
        assert!(store.read(&attempted.unwrap()).is_err());
        assert!(store.read(&second.file).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collection_waits_for_an_import_in_progress() {
        let (dir, store) = temp_store("race");
        // Stands in for the database: what floors reference.
        let referenced = Mutex::new(HashSet::new());
        let plan = std::thread::scope(|scope| {
            store
                .import(PNG, 10.0, 10.0, |plan| {
                    // The file is on disk but not yet referenced. A collection
                    // started now must wait rather than see it as garbage.
                    let gc = scope
                        .spawn(|| store.collect_garbage(|| Ok(referenced.lock().unwrap().clone())));
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    assert!(!gc.is_finished(), "collection ran during an import");
                    referenced.lock().unwrap().insert(plan.file.clone());
                    Ok((plan.clone(), None))
                })
                .unwrap()
        });
        assert_eq!(store.read(&plan.file).unwrap(), PNG);
        let _ = fs::remove_dir_all(&dir);
    }
}
