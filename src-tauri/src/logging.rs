//! Logging: stdout from the first line, plus a daily file in the app log dir
//! once Tauri can say where that is (the file layer is swapped in through a
//! reload handle in `setup`). Release builds have no console, so the file is
//! what users can send us.
//!
//! Files are `fresnel.YYYY-MM-DD.log` (UTC date); the newest
//! [`MAX_LOG_FILES`] are kept. Writes are synchronous so that nothing is lost
//! when the process dies right after logging a panic.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::format::{DefaultFields, Format};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

const DEFAULT_FILTER: &str = "info,fresnel_core=info,fresnel_lib=info,zbus=warn";
const LOG_PREFIX: &str = "fresnel";
const LOG_SUFFIX: &str = "log";
const MAX_LOG_FILES: usize = 14;
/// How much of the log file's tail [`recent_lines`] reads at most.
const TAIL_BYTES: u64 = 512 * 1024;

type FileLayer = fmt::Layer<Registry, DefaultFields, Format, RollingFileAppender>;

static FILE_HANDLE: OnceLock<reload::Handle<Option<FileLayer>, Registry>> = OnceLock::new();
/// Where file logging ended up: the directory, or why there is none.
static FILE_STATUS: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Log to stdout. Override the filter with e.g. `RUST_LOG=fresnel_core=debug`.
pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let (file_layer, handle) = reload::Layer::new(None::<FileLayer>);
    let installed = tracing_subscriber::registry()
        .with(file_layer)
        .with(filter)
        .with(fmt::layer().with_target(true))
        .try_init()
        .is_ok();
    if installed {
        let _ = FILE_HANDLE.set(handle);
    }
}

/// Also log to a daily file in `dir`. On failure, keep logging to stdout
/// only and say so.
pub fn attach_file(dir: Result<PathBuf, String>) {
    let status = dir.and_then(|dir| open_file_layer(&dir).map(|()| dir));
    match &status {
        Ok(dir) => tracing::info!(dir = %dir.display(), "logging to file"),
        Err(e) => tracing::warn!("not logging to a file ({e}); logging to stdout only"),
    }
    let _ = FILE_STATUS.set(status);
}

fn open_file_layer(dir: &Path) -> Result<(), String> {
    let handle = FILE_HANDLE
        .get()
        .ok_or("another logger was installed first")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
        .map_err(|e| format!("cannot open a log file in {}: {e}", dir.display()))?;
    let layer = fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(appender);
    handle
        .reload(Some(layer))
        .map_err(|e| format!("cannot attach the log file: {e}"))
}

/// The log directory in use, or why there is none.
pub fn file_status() -> Result<PathBuf, String> {
    FILE_STATUS
        .get()
        .cloned()
        .unwrap_or_else(|| Err("file logging not started".into()))
}

/// The last `n` lines of the newest log file, and that file's path.
pub fn recent_lines(n: usize) -> Result<(PathBuf, Vec<String>), String> {
    let dir = file_status()?;
    let path = newest_log_file(&dir)?;
    let lines = tail_lines(&path, n).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok((path, lines))
}

fn newest_log_file(dir: &Path) -> Result<PathBuf, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| is_log_file_name(name))
        // The date in the name sorts chronologically.
        .max()
        .map(|name| dir.join(name))
        .ok_or_else(|| format!("no log files in {}", dir.display()))
}

fn is_log_file_name(name: &str) -> bool {
    name.strip_prefix(LOG_PREFIX)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(LOG_SUFFIX))
        .and_then(|rest| rest.strip_suffix('.'))
        .is_some_and(|date| !date.is_empty())
}

fn tail_lines(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut buf)?;
    Ok(last_lines(&String::from_utf8_lossy(&buf), n, start > 0))
}

/// The last `n` lines of `text`. If `text` starts mid-file, its first line
/// is probably partial and is dropped.
fn last_lines(text: &str, n: usize, starts_mid_file: bool) -> Vec<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    if starts_mid_file && !lines.is_empty() {
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| l.to_string()).collect()
}

/// Log panics (message, location, thread, backtrace) through `tracing`, so
/// they reach the log file, then run the default hook as before.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(non-string panic payload)");
        let location = info
            .location()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "unknown location".into());
        let thread = std::thread::current();
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            thread = thread.name().unwrap_or("<unnamed>"),
            %location,
            "panic: {message}\nbacktrace:\n{backtrace}"
        );
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only test that installs the global subscriber.
    #[test]
    fn file_layer_is_attached_after_startup() {
        let dir = std::env::temp_dir().join(format!("fresnel-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        init();
        tracing::info!("before the file layer");
        attach_file(Ok(dir.clone()));
        tracing::info!("after the file layer");

        assert_eq!(file_status().unwrap(), dir);
        let (path, lines) = recent_lines(10).unwrap();
        assert!(is_log_file_name(
            &path.file_name().unwrap().to_string_lossy()
        ));
        assert!(
            lines.iter().any(|l| l.contains("after the file layer")),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("before the file layer")));
        assert!(
            !lines.iter().any(|l| l.contains('\u{1b}')),
            "no ANSI colours in the file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn log_file_names() {
        assert!(is_log_file_name("fresnel.2026-10-02.log"));
        assert!(!is_log_file_name("fresnel.log"));
        assert!(!is_log_file_name("fresnel.2026-10-02.log.bak"));
        assert!(!is_log_file_name("other.2026-10-02.log"));
    }

    #[test]
    fn last_lines_drops_a_partial_first_line() {
        let text = "tial line\nb\nc\nd\n";
        assert_eq!(last_lines(text, 2, true), ["c", "d"]);
        assert_eq!(last_lines(text, 10, true), ["b", "c", "d"]);
        assert_eq!(last_lines(text, 10, false), ["tial line", "b", "c", "d"]);
        assert!(last_lines("", 5, true).is_empty());
    }
}
