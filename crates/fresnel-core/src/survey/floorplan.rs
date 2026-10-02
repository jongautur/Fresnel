//! Floor plan image files.
//!
//! Imported plans are copied into one directory owned by the app (the
//! project must not break when the original file moves); the database only
//! stores the file name. Files no floor references are removed by
//! [`PlanStore::collect_garbage`].
//!
//! The files themselves live in a [`FileStore`] (prefix `plan-`), which runs
//! an import (store, then reference) and garbage collection (read the
//! referenced set, then sweep the directory) under one lock, so a collection
//! can't delete a plan that is being imported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::filestore::{FileFormat, FileStore, NewFile, StoreSpec};
use super::models::FloorPlan;
use crate::error::{Result, WifiError};

pub const MAX_PLAN_BYTES: usize = 64 * 1024 * 1024;
/// Upper bound for a plan's side in px (sanity check, not a format limit).
const MAX_PLAN_SIDE: f64 = 50_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFormat {
    Png,
    Jpeg,
    Svg,
}

impl PlanFormat {
    pub fn mime(self) -> &'static str {
        self.file_format().mime
    }

    fn file_format(self) -> &'static FileFormat {
        match self {
            Self::Png => &PNG,
            Self::Jpeg => &JPEG,
            Self::Svg => &SVG,
        }
    }

    /// Identify the format from the content, never from the file name.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        [Self::Png, Self::Jpeg, Self::Svg]
            .into_iter()
            .find(|f| (f.file_format().sniff)(bytes))
    }
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    (text.starts_with("<?xml") || text.starts_with("<svg") || text.starts_with("<!--"))
        && text.contains("<svg")
}

/// Shared with the photo store.
pub(crate) static PNG: FileFormat = FileFormat {
    mime: "image/png",
    extension: "png",
    sniff: is_png,
};
pub(crate) static JPEG: FileFormat = FileFormat {
    mime: "image/jpeg",
    extension: "jpg",
    sniff: is_jpeg,
};
static SVG: FileFormat = FileFormat {
    mime: "image/svg+xml",
    extension: "svg",
    sniff: is_svg,
};

const SPEC: StoreSpec = StoreSpec {
    prefix: "plan-",
    max_bytes: MAX_PLAN_BYTES,
    formats: &[&PNG, &JPEG, &SVG],
    label: "floor plan",
    unsupported: "unsupported floor plan format; use PNG, JPEG or SVG (export PDFs to PNG first)",
};

pub struct PlanStore {
    files: FileStore,
}

impl PlanStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            files: FileStore::new(dir, SPEC),
        }
    }

    pub fn dir(&self) -> &Path {
        self.files.dir()
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
        self.files.check(bytes)?;
        let valid = |v: f64| v.is_finite() && (1.0..=MAX_PLAN_SIDE).contains(&v);
        if !valid(width) || !valid(height) {
            return Err(WifiError::InvalidInput(format!(
                "the floor plan's size ({width} × {height} px) is not usable"
            )));
        }
        let file = NewFile { suffix: "", bytes };
        self.files.import(&[file], |stored| {
            let plan = FloorPlan {
                file: stored[0].name.clone(),
                mime: stored[0].format.mime.into(),
                width,
                height,
            };
            let (value, previous) = reference(&plan)?;
            Ok((value, previous.into_iter().collect()))
        })
    }

    pub fn read(&self, file: &str) -> Result<Vec<u8>> {
        self.files.read(file)
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
        self.files.collect_garbage(referenced)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

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
