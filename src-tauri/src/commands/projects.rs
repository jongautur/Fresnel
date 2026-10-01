use fresnel_core::database::projects::{NewProject, Project};
use fresnel_core::database::Database;
use fresnel_core::WifiError;
use tauri::State;

use crate::state::AppState;

/// Run a blocking DB operation off the async runtime.
async fn with_db<T, F>(state: &AppState, f: F) -> Result<T, WifiError>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T, WifiError> + Send + 'static,
{
    let db = state.db()?;
    tokio::task::spawn_blocking(move || f(&db))
        .await
        .map_err(|e| WifiError::Backend(format!("database task failed: {e}")))?
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>, WifiError> {
    with_db(&state, |db| db.list_projects()).await
}

#[tauri::command]
pub async fn create_project(
    state: State<'_, AppState>,
    project: NewProject,
) -> Result<Project, WifiError> {
    with_db(&state, move |db| db.create_project(&project)).await
}

#[tauri::command]
pub async fn delete_project(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    with_db(&state, move |db| db.delete_project(id)).await
}
