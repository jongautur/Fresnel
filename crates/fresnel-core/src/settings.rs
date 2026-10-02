//! App settings that belong to the user, not to a project: report branding
//! (technician, company, logo).
//!
//! A small JSON file in the app data directory, replaced atomically, plus
//! the logo as a file of its own next to it. The logo's format is sniffed
//! from its content whenever it is read, so the two files can't disagree.
//! The logo is PNG or JPEG only: an SVG can carry script and links, and the
//! report must stay inert.

use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{Result, WifiError};
use crate::survey::filestore::replace_file;

pub const SETTINGS_FILE: &str = "settings.json";
pub const LOGO_FILE: &str = "branding-logo";
pub const MAX_LOGO_BYTES: usize = 2 * 1024 * 1024;
/// Per side; a logo is drawn a few centimetres wide.
pub const MAX_LOGO_SIDE: u32 = 8192;
pub const MAX_NAME_CHARS: usize = 120;

/// What the user typed (validated by [`SettingsStore::set_branding`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Branding {
    pub technician_name: Option<String>,
    pub company_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoInfo {
    pub mime: &'static str,
    pub bytes: usize,
    pub width: u32,
    pub height: u32,
}

/// Branding as stored, with the logo's details (its bytes are fetched apart).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandingInfo {
    pub technician_name: Option<String>,
    pub company_name: Option<String>,
    pub logo: Option<LogoInfo>,
}

/// The file's layout. `version` is for future changes.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SettingsFile {
    version: u32,
    branding: Branding,
}

const FILE_VERSION: u32 = 1;

pub struct SettingsStore {
    dir: PathBuf,
    /// Serialises read-modify-write of the files.
    lock: Mutex<()>,
}

impl SettingsStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            lock: Mutex::new(()),
        }
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn branding(&self) -> Result<BrandingInfo> {
        let _guard = self.guard();
        self.info()
    }

    /// Store the names (trimmed; blank clears one).
    pub fn set_branding(&self, branding: Branding) -> Result<BrandingInfo> {
        let branding = Branding {
            technician_name: clean_name("technician name", branding.technician_name)?,
            company_name: clean_name("company name", branding.company_name)?,
        };
        let _guard = self.guard();
        let file = SettingsFile {
            version: FILE_VERSION,
            branding,
        };
        let json = serde_json::to_vec_pretty(&file)
            .map_err(|e| WifiError::Backend(format!("cannot encode settings: {e}")))?;
        fs::create_dir_all(&self.dir).map_err(|e| io_error("create", &self.dir, e))?;
        let path = self.dir.join(SETTINGS_FILE);
        replace_file(&path, &json).map_err(|e| io_error("save", &path, e))?;
        self.info()
    }

    /// The logo's bytes and MIME type, if one is set.
    pub fn logo(&self) -> Result<Option<(Vec<u8>, &'static str)>> {
        let _guard = self.guard();
        Ok(self.read_logo()?.map(|(bytes, info)| (bytes, info.mime)))
    }

    /// Replace the logo: PNG or JPEG, at most [`MAX_LOGO_BYTES`].
    pub fn set_logo(&self, bytes: &[u8]) -> Result<BrandingInfo> {
        check_logo(bytes)?;
        let _guard = self.guard();
        fs::create_dir_all(&self.dir).map_err(|e| io_error("create", &self.dir, e))?;
        let path = self.dir.join(LOGO_FILE);
        replace_file(&path, bytes).map_err(|e| io_error("save", &path, e))?;
        self.info()
    }

    pub fn clear_logo(&self) -> Result<BrandingInfo> {
        let _guard = self.guard();
        let path = self.dir.join(LOGO_FILE);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error("delete", &path, e)),
        }
        self.info()
    }

    fn info(&self) -> Result<BrandingInfo> {
        let branding = self.read_branding();
        Ok(BrandingInfo {
            technician_name: branding.technician_name,
            company_name: branding.company_name,
            logo: self.read_logo()?.map(|(_, info)| info),
        })
    }

    /// Missing file: defaults. A damaged file or invalid values are logged
    /// and ignored (the next save replaces the file); settings are never a
    /// reason to fail.
    fn read_branding(&self) -> Branding {
        let path = self.dir.join(SETTINGS_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Branding::default(),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot read settings; using defaults");
                return Branding::default();
            }
        };
        let file: SettingsFile = match serde_json::from_slice(&bytes) {
            Ok(f) => f,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "settings file is damaged; using defaults");
                return Branding::default();
            }
        };
        let keep = |what: &str, v: Option<String>| {
            clean_name(what, v).unwrap_or_else(|e| {
                warn!(error = %e, "ignoring a stored setting");
                None
            })
        };
        Branding {
            technician_name: keep("technician name", file.branding.technician_name),
            company_name: keep("company name", file.branding.company_name),
        }
    }

    /// A stored logo that no longer passes the checks is ignored (logged).
    fn read_logo(&self) -> Result<Option<(Vec<u8>, LogoInfo)>> {
        let path = self.dir.join(LOGO_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error("read", &path, e)),
        };
        match check_logo(&bytes) {
            Ok(info) => Ok(Some((bytes, info))),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "ignoring the stored logo");
                Ok(None)
            }
        }
    }
}

fn clean_name(what: &str, value: Option<String>) -> Result<Option<String>> {
    let Some(v) = value else { return Ok(None) };
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    if v.chars().count() > MAX_NAME_CHARS {
        return Err(WifiError::InvalidInput(format!(
            "the {what} can be at most {MAX_NAME_CHARS} characters"
        )));
    }
    if v.chars().any(char::is_control) {
        return Err(WifiError::InvalidInput(format!(
            "the {what} can't contain line breaks or control characters"
        )));
    }
    Ok(Some(v.to_string()))
}

/// Content checks: format by magic bytes, size, and a header that decodes
/// to sane dimensions.
pub fn check_logo(bytes: &[u8]) -> Result<LogoInfo> {
    if bytes.is_empty() {
        return Err(WifiError::InvalidInput("the logo file is empty".into()));
    }
    if bytes.len() > MAX_LOGO_BYTES {
        return Err(WifiError::InvalidInput(format!(
            "the logo is {:.1} MB; the limit is {} MB. Export it smaller.",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_LOGO_BYTES / (1024 * 1024)
        )));
    }
    let (format, mime) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        (ImageFormat::Png, "image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        (ImageFormat::Jpeg, "image/jpeg")
    } else if looks_like_svg(bytes) {
        return Err(WifiError::InvalidInput(
            "SVG logos aren't accepted (an SVG can contain scripts and links, and the report must not). Export the logo as PNG."
                .into(),
        ));
    } else {
        return Err(WifiError::InvalidInput(
            "the logo must be a PNG or JPEG image".into(),
        ));
    };
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| WifiError::InvalidInput(format!("the logo could not be read: {e}")))?;
    if width == 0 || height == 0 || width > MAX_LOGO_SIDE || height > MAX_LOGO_SIDE {
        return Err(WifiError::InvalidInput(format!(
            "the logo is {width} × {height} px; at most {MAX_LOGO_SIDE} px a side"
        )));
    }
    Ok(LogoInfo {
        mime,
        bytes: bytes.len(),
        width,
        height,
    })
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    head.contains("<svg")
        || head
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with("<?xml")
}

fn io_error(action: &str, path: &Path, e: io::Error) -> WifiError {
    WifiError::Backend(format!("cannot {action} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("fresnel-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::new(w, h)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn branding_round_trip_and_validation() {
        let dir = temp_dir("names");
        let store = SettingsStore::new(&dir);
        assert_eq!(
            store.branding().unwrap(),
            BrandingInfo {
                technician_name: None,
                company_name: None,
                logo: None
            }
        );
        let info = store
            .set_branding(Branding {
                technician_name: Some("  Ada  ".into()),
                company_name: Some("   ".into()),
            })
            .unwrap();
        assert_eq!(info.technician_name.as_deref(), Some("Ada"));
        assert_eq!(info.company_name, None);
        // A new store reads what the old one wrote.
        assert_eq!(SettingsStore::new(&dir).branding().unwrap(), info);

        for bad in ["a\nb".to_string(), "x".repeat(MAX_NAME_CHARS + 1)] {
            let err = store
                .set_branding(Branding {
                    technician_name: None,
                    company_name: Some(bad),
                })
                .unwrap_err();
            assert!(matches!(err, WifiError::InvalidInput(_)));
        }
        // A refused save leaves the stored values alone.
        assert_eq!(store.branding().unwrap(), info);

        // A damaged file reads as defaults rather than failing.
        fs::write(dir.join(SETTINGS_FILE), b"{ not json").unwrap();
        assert_eq!(store.branding().unwrap().technician_name, None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn logo_is_sniffed_and_svg_refused() {
        let dir = temp_dir("logo");
        let store = SettingsStore::new(&dir);
        let bytes = png(40, 20);
        let info = store.set_logo(&bytes).unwrap();
        let logo = info.logo.unwrap();
        assert_eq!((logo.mime, logo.width, logo.height), ("image/png", 40, 20));
        assert_eq!(store.logo().unwrap().unwrap().0, bytes);

        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let err = store.set_logo(svg).unwrap_err();
        assert!(err.to_string().contains("SVG"));
        assert!(store.set_logo(b"GIF89a....").is_err());
        assert!(store.set_logo(&vec![0x89; MAX_LOGO_BYTES + 1]).is_err());
        // A PNG signature with garbage after it doesn't pass either.
        assert!(store.set_logo(b"\x89PNG\r\n\x1a\nnope").is_err());
        // Refused uploads keep the previous logo.
        assert_eq!(store.logo().unwrap().unwrap().0, bytes);

        assert_eq!(store.clear_logo().unwrap().logo, None);
        assert_eq!(store.clear_logo().unwrap().logo, None);
        let _ = fs::remove_dir_all(dir);
    }
}
