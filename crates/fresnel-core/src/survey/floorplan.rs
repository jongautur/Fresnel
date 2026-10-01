//! Floor plan image files.
//!
//! Imported plans are copied into one directory owned by the app (the
//! project must not break when the original file moves); the database only
//! stores the file name. Files no floor references are removed by
//! [`PlanStore::collect_garbage`].

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
}

impl PlanStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Validate and store an image. `width`/`height` are the image's natural
    /// size as the UI renders it (which accounts for e.g. JPEG EXIF rotation
    /// and SVG viewBoxes); that rendered size is the survey coordinate space.
    pub fn save(&self, bytes: &[u8], width: f64, height: f64) -> Result<FloorPlan> {
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
            fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            let _ = fs::remove_file(&tmp);
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

    pub fn remove(&self, file: &str) {
        if let Ok(path) = self.path_of(file) {
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(path = %path.display(), error = %e, "could not delete floor plan");
                }
            }
        }
    }

    /// Delete plan files (and leftover temp files) no floor references.
    /// Only touches files this store created. Returns how many were removed.
    pub fn collect_garbage(&self, referenced: &HashSet<String>) -> usize {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return 0;
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
        removed
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

    #[test]
    fn save_read_and_collect() {
        let dir = std::env::temp_dir().join(format!("fresnel-plans-{}", std::process::id()));
        let store = PlanStore::new(&dir);
        let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let plan = store.save(&png, 800.0, 600.0).unwrap();
        assert_eq!(plan.mime, "image/png");
        assert_eq!(store.read(&plan.file).unwrap(), png);

        assert!(store.read("../etc/passwd").is_err());
        assert!(store.save(b"%PDF", 10.0, 10.0).is_err());
        assert!(store.save(&png, 0.0, 10.0).is_err());

        let keep: HashSet<String> = [plan.file.clone()].into();
        let other = store.save(&png, 10.0, 10.0).unwrap();
        fs::write(dir.join("unrelated.txt"), b"x").unwrap();
        assert_eq!(store.collect_garbage(&keep), 1);
        assert!(store.read(&other.file).is_err());
        assert!(store.read(&plan.file).is_ok());
        assert!(dir.join("unrelated.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
