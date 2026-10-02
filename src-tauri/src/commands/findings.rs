//! Rogue / evil-twin findings and BSSID marks.

use fresnel_core::survey::findings::{BssidMark, FindingScope, Findings, LinkOptions, MarkStatus};
use fresnel_core::WifiError;
use tauri::State;

use super::with_db;
use crate::state::AppState;

#[tauri::command]
pub async fn survey_findings(
    state: State<'_, AppState>,
    scope: FindingScope,
) -> Result<Findings, WifiError> {
    with_db(&state, move |db| db.findings(scope)).await
}

#[tauri::command]
pub async fn set_bssid_mark(
    state: State<'_, AppState>,
    project_id: i64,
    bssid: String,
    status: MarkStatus,
    note: Option<String>,
) -> Result<BssidMark, WifiError> {
    with_db(&state, move |db| {
        db.set_bssid_mark(project_id, &bssid, status, note)
    })
    .await
}

#[tauri::command]
pub async fn clear_bssid_mark(
    state: State<'_, AppState>,
    project_id: i64,
    bssid: String,
) -> Result<bool, WifiError> {
    with_db(&state, move |db| db.clear_bssid_mark(project_id, &bssid)).await
}

/// "This is ours": what the UI needs to place or link the BSSID. The link
/// itself goes through `create_placed_ap` / `update_placed_ap`.
#[tauri::command]
pub async fn bssid_link_options(
    state: State<'_, AppState>,
    project_id: i64,
    bssid: String,
) -> Result<LinkOptions, WifiError> {
    with_db(&state, move |db| db.bssid_link_options(project_id, &bssid)).await
}
