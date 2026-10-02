//! Active tests per point: settings, run (after Measure Here), cancel, list.

use fresnel_core::nettools::settings::TestSettings;
use fresnel_core::survey::models::PointTest;
use fresnel_core::survey::point_tests;
use fresnel_core::WifiError;
use tauri::State;

use super::with_db;
use crate::state::AppState;

#[tauri::command]
pub async fn get_test_settings(state: State<'_, AppState>) -> Result<TestSettings, WifiError> {
    let dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || TestSettings::load(&dir))
        .await
        .map_err(|e| WifiError::Backend(format!("settings task failed: {e}")))
}

#[tauri::command]
pub async fn save_test_settings(
    state: State<'_, AppState>,
    settings: TestSettings,
) -> Result<TestSettings, WifiError> {
    let dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || settings.save(&dir))
        .await
        .map_err(|e| WifiError::Backend(format!("settings task failed: {e}")))?
}

/// Run the configured tests for a point just measured. `test_id` (chosen
/// by the UI) is what `cancel_active_test` takes.
#[tauri::command]
pub async fn run_point_tests(
    state: State<'_, AppState>,
    point_id: i64,
    test_id: String,
) -> Result<Vec<PointTest>, WifiError> {
    let db = state.db()?;
    let dir = state.data_dir.clone();
    let settings = tokio::task::spawn_blocking(move || TestSettings::load(&dir))
        .await
        .map_err(|e| WifiError::Backend(format!("settings task failed: {e}")))?;
    let cancel = state.begin_test(&test_id)?;
    let result =
        point_tests::run_point_tests(&state.scanner, db, point_id, settings, &cancel).await;
    state.finish_test(&test_id);
    result.inspect_err(|e| tracing::warn!(kind = e.kind(), error = %e, "point tests failed"))
}

#[tauri::command]
pub fn cancel_active_test(state: State<'_, AppState>, test_id: String) -> bool {
    state.cancel_test(&test_id)
}

#[tauri::command]
pub async fn list_floor_point_tests(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<Vec<PointTest>, WifiError> {
    with_db(&state, move |db| db.list_floor_point_tests(floor_id)).await
}
