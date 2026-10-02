use fresnel_core::database::requirements::TargetOptions;
use fresnel_core::survey::requirements::{
    self, FloorRequirements, PresetInfo, RequirementProfile, RequirementProfileInput,
    RequirementTarget,
};
use fresnel_core::WifiError;
use tauri::State;

use super::with_db;
use crate::state::AppState;

#[tauri::command]
pub fn requirement_presets() -> Vec<PresetInfo> {
    requirements::presets()
}

#[tauri::command]
pub async fn list_requirement_profiles(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<RequirementProfile>, WifiError> {
    with_db(&state, move |db| db.list_requirement_profiles(project_id)).await
}

#[tauri::command]
pub async fn create_requirement_profile(
    state: State<'_, AppState>,
    project_id: i64,
    profile: RequirementProfileInput,
) -> Result<RequirementProfile, WifiError> {
    with_db(&state, move |db| {
        db.create_requirement_profile(project_id, &profile)
    })
    .await
}

#[tauri::command]
pub async fn update_requirement_profile(
    state: State<'_, AppState>,
    id: i64,
    profile: RequirementProfileInput,
) -> Result<RequirementProfile, WifiError> {
    with_db(&state, move |db| {
        db.update_requirement_profile(id, &profile)
    })
    .await
}

#[tauri::command]
pub async fn delete_requirement_profile(
    state: State<'_, AppState>,
    id: i64,
) -> Result<bool, WifiError> {
    with_db(&state, move |db| db.delete_requirement_profile(id)).await
}

#[tauri::command]
pub async fn set_requirement_targets(
    state: State<'_, AppState>,
    profile_id: i64,
    targets: Vec<RequirementTarget>,
) -> Result<RequirementProfile, WifiError> {
    with_db(&state, move |db| {
        db.set_requirement_targets(profile_id, &targets)
    })
    .await
}

/// SSIDs heard on the project's floors and its placed APs.
#[tauri::command]
pub async fn requirement_target_options(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<TargetOptions, WifiError> {
    with_db(&state, move |db| db.requirement_target_options(project_id)).await
}

/// Set the floor's override (`None`: the project default) and return the
/// floor's re-evaluated requirements.
#[tauri::command]
pub async fn set_floor_requirement_profile(
    state: State<'_, AppState>,
    floor_id: i64,
    profile_id: Option<i64>,
) -> Result<FloorRequirements, WifiError> {
    with_db(&state, move |db| {
        db.set_floor_requirement_profile(floor_id, profile_id)?;
        db.floor_requirements(floor_id)
    })
    .await
}

#[tauri::command]
pub async fn evaluate_floor_requirements(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<FloorRequirements, WifiError> {
    with_db(&state, move |db| db.floor_requirements(floor_id)).await
}
