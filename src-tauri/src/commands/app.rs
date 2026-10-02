use fresnel_core::WifiError;
use serde::Serialize;
use tauri::State;

use super::with_db;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: &'static str,
    database_path: String,
    database_error: Option<String>,
    /// Something the user should be told about the database (e.g. a damaged
    /// file was set aside and a new one started).
    database_notice: Option<String>,
    schema_version: Option<u32>,
}

/// Async so that a database retry (see `AppState::db`) never blocks the
/// main thread.
#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, WifiError> {
    let (schema_version, database_error) = match with_db(&state, |db| db.schema_version()).await {
        Ok(version) => (Some(version), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        database_path: state.db_path.display().to_string(),
        database_error,
        database_notice: state.db_notice(),
        schema_version,
    })
}
