use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: &'static str,
    database_path: String,
    database_error: Option<String>,
    schema_version: Option<u32>,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    let (schema_version, database_error) = match &state.db {
        Ok(db) => (db.schema_version().ok(), None),
        Err(e) => (None, Some(e.to_string())),
    };
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        database_path: state.db_path.display().to_string(),
        database_error,
        schema_version,
    }
}
