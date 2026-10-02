//! Report export IPC. The webview renders the report (HTML, CSV, JSON) and
//! hands over the bytes; choosing a file and writing it stays in Rust, so the
//! webview has no filesystem access.
use super::with_db;
use crate::state::AppState;
use fresnel_core::survey::filestore::replace_file;
use fresnel_core::{database::notes::FloorAnnotations, WifiError};
use std::path::{Path, PathBuf};
use tauri::{
    ipc::{InvokeBody, Request},
    AppHandle, Runtime, State,
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
pub async fn floor_annotations(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<FloorAnnotations, WifiError> {
    with_db(&state, move |db| db.floor_annotations(floor_id)).await
}

/// Written through a sibling temporary file and a rename, so a partial
/// report is never left under the chosen name (or over an older report).
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WifiError> {
    replace_file(path, bytes)
        .map_err(|e| WifiError::Backend(format!("could not save {}: {e}", path.display())))
}

/// Header values are ASCII, so the UI percent-encodes the suggested name.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A suggested file name from the UI (e.g. the project name), reduced to
/// characters every file system accepts.
fn file_stem(suggested: Option<&str>) -> String {
    let stem: String = percent_decode(suggested.unwrap_or(""))
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(80)
        .collect();
    let stem = stem.trim_matches(|c: char| c == '.' || c == ' ' || c == '-');
    if stem.is_empty() {
        "fresnel-survey".into()
    } else {
        stem.into()
    }
}

/// GTK's dialog doesn't add the extension the filter implies.
fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    let has = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if has {
        path
    } else {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{ext}"));
        path.with_file_name(name)
    }
}

/// Ask where to save, then write the raw body there. Returns the path
/// written, or null when the user cancelled.
#[tauri::command]
pub async fn save_export<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<Option<String>, WifiError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(WifiError::InvalidInput(
            "expected export as a binary body".into(),
        ));
    };
    let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok());
    let (label, ext) = match header("x-export-kind").unwrap_or("html") {
        "html" => ("HTML report", "html"),
        "csv" => ("CSV data", "csv"),
        "json" => ("JSON data", "json"),
        _ => return Err(WifiError::InvalidInput("unknown export format".into())),
    };
    let name = format!("{}.{ext}", file_stem(header("x-export-name")));
    let dialog = app.clone();
    let path = tokio::task::spawn_blocking(move || {
        dialog
            .dialog()
            .file()
            .add_filter(label, &[ext])
            .set_file_name(name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| WifiError::Backend(format!("save dialog failed: {e}")))?;
    let Some(path) = path else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|_| WifiError::InvalidInput("the selected location is not a local file".into()))?;
    let path = with_extension(path, ext);
    let save_path = path.clone();
    let bytes = bytes.clone();
    tokio::task::spawn_blocking(move || atomic_write(&save_path, &bytes))
        .await
        .map_err(|e| WifiError::Backend(format!("export task failed: {e}")))??;
    tracing::info!(path = %path.display(), "export saved");
    let shown = path.display().to_string();
    *state.last_export.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
    Ok(Some(shown))
}

/// Open the last saved export with the system's default app (the browser,
/// for a report). Only that file: the webview can't name another path.
#[tauri::command]
pub async fn open_export<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
) -> Result<(), WifiError> {
    let path = state
        .last_export
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .ok_or_else(|| WifiError::InvalidInput("nothing has been exported yet".into()))?;
    app.opener()
        .open_path(path.to_string_lossy().into_owned(), None::<String>)
        .map_err(|e| {
            WifiError::Backend(format!(
                "could not open {}: {e}. Open it from your file manager.",
                path.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn atomic_save_replaces_the_file() {
        let d = std::env::temp_dir().join(format!("fresnel-report-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let p = d.join("x.html");
        fs::write(&p, b"old").unwrap();
        atomic_write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn suggested_names_are_sanitised() {
        assert_eq!(file_stem(Some("Acme / HQ: survey")), "Acme - HQ- survey");
        assert_eq!(file_stem(Some("Caf%C3%A9%20Nord")), "Café Nord");
        assert_eq!(file_stem(Some("100%")), "100");
        assert_eq!(file_stem(Some("../..")), "fresnel-survey");
        assert_eq!(file_stem(None), "fresnel-survey");
        assert_eq!(
            with_extension(PathBuf::from("/a/report"), "html"),
            PathBuf::from("/a/report.html")
        );
        assert_eq!(
            with_extension(PathBuf::from("/a/report.HTML"), "html"),
            PathBuf::from("/a/report.HTML")
        );
    }
}
