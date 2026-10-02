//! Support commands: the "Save diagnostics…" report and the frontend's
//! error forwarding into the log file.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use fresnel_core::database::Database;
use fresnel_core::WifiError;
use tauri::State;

use super::with_db;
use crate::state::AppState;
use crate::{env_fix, logging};

/// Log lines included at the end of the report.
const REPORT_LOG_LINES: usize = 200;
/// Listing adapters talks to NetworkManager; don't let a wedged service
/// hold up the report.
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(15);

const MAX_MESSAGE_CHARS: usize = 2_000;
const MAX_STACK_CHARS: usize = 8_000;
const MAX_SOURCE_CHARS: usize = 200;
/// Backstop for a frontend stuck in an error loop (it rate-limits too).
const FRONTEND_ERRORS_PER_MINUTE: u32 = 30;

/// A plain-text report for bug reports: versions, platform, adapters,
/// database state and the tail of the log.
#[tauri::command]
pub async fn diagnostics_report(state: State<'_, AppState>) -> Result<String, WifiError> {
    let mut out = vec![
        "Fresnel diagnostics report".to_string(),
        format!("Generated: {}", chrono::Utc::now().to_rfc3339()),
        String::new(),
        "== App ==".into(),
        format!(
            "Version: {} ({} build, Tauri {})",
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            tauri::VERSION
        ),
        format!("OS: {}", os_info::get()),
    ];
    if let Some(kernel) = kernel_release() {
        out.push(format!("Kernel: {kernel}"));
    }
    out.push(format!("Arch: {}", std::env::consts::ARCH));
    out.push(match tauri::webview_version() {
        Ok(v) => format!("Webview: {v}"),
        Err(e) => format!("Webview: unknown ({e})"),
    });
    match env_fix::changes() {
        [] => out.push("Environment fixes: none".into()),
        changes => {
            out.push("Environment fixes:".into());
            out.extend(changes.iter().map(|c| format!("  - {c}")));
        }
    }

    out.push(String::new());
    out.push("== Paths ==".into());
    out.push(format!("Data dir: {}", state.data_dir.display()));
    out.push(format!("Database: {}", state.db_path.display()));
    out.push(match logging::file_status() {
        Ok(dir) => format!("Log dir: {}", dir.display()),
        Err(e) => format!("Log dir: none ({e})"),
    });

    out.push(String::new());
    out.push("== Database ==".into());
    let latest = Database::latest_schema_version();
    out.push(match with_db(&state, |db| db.schema_version()).await {
        Ok(v) => format!("Schema version: v{v} (this build supports up to v{latest})"),
        Err(e) => format!("Error: {e}"),
    });
    if let Some(notice) = state.db_notice() {
        out.push(format!("Notice: {notice}"));
    }

    out.push(String::new());
    out.push("== Adapters ==".into());
    let listing = tokio::time::timeout(ADAPTER_TIMEOUT, state.scanner.registry().list_adapters());
    match listing.await {
        Err(_) => out.push(format!(
            "Listing adapters timed out after {} s",
            ADAPTER_TIMEOUT.as_secs()
        )),
        Ok(listing) => {
            if listing.adapters.is_empty() {
                out.push("No adapters found".into());
            }
            for a in &listing.adapters {
                out.push(format!(
                    "- {} \"{}\": provider {} (data from {}), driver {}, status {:?}{}",
                    a.id.0,
                    a.display_name,
                    a.provider,
                    a.data_sources.join(", "),
                    a.driver.as_deref().unwrap_or("unknown"),
                    a.status,
                    a.status_detail
                        .as_deref()
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default(),
                ));
                if let Some(bus) = &a.bus {
                    out.push(format!(
                        "    bus {:?} {}:{} {} {}",
                        bus.kind,
                        bus.vendor_id.as_deref().unwrap_or("?"),
                        bus.product_id.as_deref().unwrap_or("?"),
                        bus.vendor_name.as_deref().unwrap_or(""),
                        bus.product_name.as_deref().unwrap_or(""),
                    ));
                }
                out.push(format!("    {:?}", a.capabilities));
            }
            for issue in &listing.issues {
                out.push(format!(
                    "! provider {}: {} ({})",
                    issue.provider,
                    issue.error,
                    issue.error.kind()
                ));
            }
        }
    }

    out.push(String::new());
    match logging::recent_lines(REPORT_LOG_LINES) {
        Ok((path, lines)) => {
            out.push(format!(
                "== Log (last {} lines of {}) ==",
                lines.len(),
                path.display()
            ));
            out.extend(lines);
        }
        Err(e) => {
            out.push("== Log ==".into());
            out.push(format!("Not available: {e}"));
        }
    }
    out.push(String::new());
    Ok(out.join("\n"))
}

#[cfg(target_os = "linux")]
fn kernel_release() -> Option<String> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    Some(release.trim().to_string())
}

#[cfg(not(target_os = "linux"))]
fn kernel_release() -> Option<String> {
    None
}

/// Record an uncaught frontend error (window `error` / `unhandledrejection`)
/// in the log. Never fails; oversized fields are truncated.
#[tauri::command]
pub async fn log_frontend_error(message: String, stack: Option<String>, source: String) {
    if !frontend_error_allowed(Instant::now()) {
        return;
    }
    let message = clean(&message, MAX_MESSAGE_CHARS, false);
    let source = clean(&source, MAX_SOURCE_CHARS, false);
    match stack.map(|s| clean(&s, MAX_STACK_CHARS, true)) {
        Some(stack) if !stack.trim().is_empty() => tracing::error!(
            target: "fresnel_lib::frontend",
            %source,
            "{message}\n{stack}"
        ),
        _ => tracing::error!(target: "fresnel_lib::frontend", %source, "{message}"),
    }
}

struct RateWindow {
    started: Instant,
    count: u32,
    dropped: u32,
}

static FRONTEND_ERRORS: Mutex<Option<RateWindow>> = Mutex::new(None);

fn frontend_error_allowed(now: Instant) -> bool {
    let mut guard = FRONTEND_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    let window = guard.get_or_insert(RateWindow {
        started: now,
        count: 0,
        dropped: 0,
    });
    if now.duration_since(window.started) >= Duration::from_secs(60) {
        if window.dropped > 0 {
            tracing::warn!(
                dropped = window.dropped,
                "frontend errors were dropped by the rate limit"
            );
        }
        *window = RateWindow {
            started: now,
            count: 0,
            dropped: 0,
        };
    }
    if window.count < FRONTEND_ERRORS_PER_MINUTE {
        window.count += 1;
        true
    } else {
        window.dropped += 1;
        false
    }
}

/// Cap at `max` characters and replace control characters (all of them,
/// or all but newlines and tabs when `multiline`), so one frontend error
/// can neither flood the log nor forge log lines.
fn clean(s: &str, max: usize, multiline: bool) -> String {
    let mut out: String = s
        .chars()
        .take(max)
        .map(|c| match c {
            '\n' | '\t' if multiline => c,
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    if s.chars().nth(max).is_some() {
        out.push_str("… [truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_truncates_and_strips_control_characters() {
        assert_eq!(clean("a\nb\u{1b}[31mc", 100, false), "a b [31mc");
        assert_eq!(clean("a\nb\tc\r", 100, true), "a\nb\tc ");
        assert_eq!(clean("héllo wörld", 5, false), "héllo… [truncated]");
        assert_eq!(clean("exact", 5, false), "exact");
    }
}
