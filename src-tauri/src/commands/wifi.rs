use fresnel_core::wifi::models::{AdapterId, ConnectionInfo, ScanRequest, ScanResult};
use fresnel_core::WifiError;
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub async fn scan(
    state: State<'_, AppState>,
    adapter_id: AdapterId,
    request: Option<ScanRequest>,
) -> Result<ScanResult, WifiError> {
    let request = request.unwrap_or(ScanRequest {
        trigger: true,
        ssids: vec![],
    });
    state
        .scanner
        .scan(&adapter_id, &request)
        .await
        .inspect_err(|e| {
            tracing::warn!(adapter = %adapter_id, kind = e.kind(), error = %e, "scan failed");
        })
}

#[tauri::command]
pub async fn last_scan(
    state: State<'_, AppState>,
    adapter_id: AdapterId,
) -> Result<Option<ScanResult>, WifiError> {
    Ok(state.scanner.last_result(&adapter_id).await)
}

#[tauri::command]
pub async fn get_current_connection(
    state: State<'_, AppState>,
    adapter_id: AdapterId,
) -> Result<Option<ConnectionInfo>, WifiError> {
    state.scanner.current_connection(&adapter_id).await
}
