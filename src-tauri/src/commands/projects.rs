use fresnel_core::database::projects::{NewProject, Project};
use fresnel_core::WifiError;
use tauri::State;

use super::survey::collect_plan_garbage;
use super::with_db;
use crate::state::AppState;

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
    let deleted = with_db(&state, move |db| db.delete_project(id)).await?;
    collect_plan_garbage(&state).await;
    Ok(deleted)
}
