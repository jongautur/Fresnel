use fresnel_core::survey::measure::{self, MeasureRequest};
use fresnel_core::survey::models::{
    Building, Floor, FloorScale, NewBuilding, NewFloor, PlacedAp, PlacedApInput, SurveyPoint,
};
use fresnel_core::WifiError;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::State;

use super::with_db;
use crate::state::AppState;

/// Remove plan files no floor references any more (after cascading deletes).
/// Best effort: a failure only leaves an orphaned file for the next run.
pub(crate) async fn collect_plan_garbage(state: &AppState) {
    let plans = state.plans.clone();
    let _ = with_db(state, move |db| {
        let referenced = db.referenced_plan_files()?;
        plans.collect_garbage(&referenced);
        Ok(())
    })
    .await;
}

// --- Buildings ------------------------------------------------------------

#[tauri::command]
pub async fn list_buildings(
    state: State<'_, AppState>,
    project_id: i64,
) -> Result<Vec<Building>, WifiError> {
    with_db(&state, move |db| db.list_buildings(project_id)).await
}

#[tauri::command]
pub async fn create_building(
    state: State<'_, AppState>,
    building: NewBuilding,
) -> Result<Building, WifiError> {
    with_db(&state, move |db| db.create_building(&building)).await
}

#[tauri::command]
pub async fn delete_building(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    let deleted = with_db(&state, move |db| db.delete_building(id)).await?;
    collect_plan_garbage(&state).await;
    Ok(deleted)
}

// --- Floors ---------------------------------------------------------------

#[tauri::command]
pub async fn list_floors(
    state: State<'_, AppState>,
    building_id: i64,
) -> Result<Vec<Floor>, WifiError> {
    with_db(&state, move |db| db.list_floors(building_id)).await
}

#[tauri::command]
pub async fn create_floor(state: State<'_, AppState>, floor: NewFloor) -> Result<Floor, WifiError> {
    with_db(&state, move |db| db.create_floor(&floor)).await
}

#[tauri::command]
pub async fn delete_floor(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    let deleted = with_db(&state, move |db| db.delete_floor(id)).await?;
    collect_plan_garbage(&state).await;
    Ok(deleted)
}

/// Import a floor plan image. The image travels as the raw IPC body (no
/// base64/JSON overhead); metadata comes in headers:
/// `x-floor-id`, and `x-plan-width` / `x-plan-height` (natural size as the
/// webview renders it).
#[tauri::command]
pub async fn import_floor_plan(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<Floor, WifiError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(WifiError::InvalidInput(
            "expected the floor plan image as a binary body".into(),
        ));
    };
    let header = |name: &str| -> Result<&str, WifiError> {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| WifiError::InvalidInput(format!("missing {name} header")))
    };
    let number = |name: &str| -> Result<f64, WifiError> {
        header(name)?
            .parse::<f64>()
            .map_err(|_| WifiError::InvalidInput(format!("{name} is not a number")))
    };
    let floor_id: i64 = header("x-floor-id")?
        .parse()
        .map_err(|_| WifiError::InvalidInput("x-floor-id is not an id".into()))?;
    let (width, height) = (number("x-plan-width")?, number("x-plan-height")?);
    let bytes = bytes.clone();

    let plans = state.plans.clone();
    with_db(&state, move |db| {
        let plan = plans.save(&bytes, width, height)?;
        match db.set_floor_plan(floor_id, &plan) {
            Ok((floor, previous)) => {
                if let Some(old) = previous {
                    plans.remove(&old);
                }
                Ok(floor)
            }
            Err(e) => {
                plans.remove(&plan.file);
                Err(e)
            }
        }
    })
    .await
}

/// The floor's plan image as raw bytes (the UI knows the MIME type from
/// `Floor.plan.mime`).
#[tauri::command]
pub async fn floor_plan_image(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<Response, WifiError> {
    let plans = state.plans.clone();
    let bytes = with_db(&state, move |db| {
        let floor = db
            .get_floor(floor_id)?
            .ok_or_else(|| WifiError::InvalidInput(format!("floor {floor_id} no longer exists")))?;
        let plan = floor
            .plan
            .ok_or_else(|| WifiError::InvalidInput("this floor has no plan".into()))?;
        plans.read(&plan.file)
    })
    .await?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn set_floor_scale(
    state: State<'_, AppState>,
    floor_id: i64,
    scale: Option<FloorScale>,
) -> Result<Floor, WifiError> {
    with_db(&state, move |db| {
        db.set_floor_scale(floor_id, scale.as_ref())
    })
    .await
}

// --- Points ---------------------------------------------------------------

#[tauri::command]
pub async fn list_survey_points(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<Vec<SurveyPoint>, WifiError> {
    with_db(&state, move |db| db.list_survey_points(floor_id)).await
}

#[tauri::command]
pub async fn measure_here(
    state: State<'_, AppState>,
    request: MeasureRequest,
) -> Result<SurveyPoint, WifiError> {
    let db = state.db()?;
    measure::measure_here(&state.scanner, db, request)
        .await
        .inspect_err(|e| tracing::warn!(kind = e.kind(), error = %e, "measurement failed"))
}

#[tauri::command]
pub async fn delete_survey_point(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    with_db(&state, move |db| db.delete_survey_point(id)).await
}

// --- Placed access points -------------------------------------------------

#[tauri::command]
pub async fn list_building_aps(
    state: State<'_, AppState>,
    building_id: i64,
) -> Result<Vec<PlacedAp>, WifiError> {
    with_db(&state, move |db| db.list_building_aps(building_id)).await
}

#[tauri::command]
pub async fn create_placed_ap(
    state: State<'_, AppState>,
    floor_id: i64,
    ap: PlacedApInput,
) -> Result<PlacedAp, WifiError> {
    with_db(&state, move |db| db.create_placed_ap(floor_id, &ap)).await
}

#[tauri::command]
pub async fn update_placed_ap(
    state: State<'_, AppState>,
    id: i64,
    ap: PlacedApInput,
) -> Result<PlacedAp, WifiError> {
    with_db(&state, move |db| db.update_placed_ap(id, &ap)).await
}

#[tauri::command]
pub async fn delete_placed_ap(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    with_db(&state, move |db| db.delete_placed_ap(id)).await
}
