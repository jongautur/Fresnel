//! Report branding stored as app settings (`fresnel_core::settings`).
use fresnel_core::settings::{Branding, BrandingInfo};
use fresnel_core::WifiError;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::State;

use crate::state::AppState;

/// Settings files are tiny; still, keep disk access off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, WifiError> + Send + 'static,
) -> Result<T, WifiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WifiError::Backend(format!("settings task failed: {e}")))?
}

#[tauri::command]
pub async fn get_branding(state: State<'_, AppState>) -> Result<BrandingInfo, WifiError> {
    let settings = state.settings.clone();
    blocking(move || settings.branding()).await
}

#[tauri::command]
pub async fn set_branding(
    state: State<'_, AppState>,
    branding: Branding,
) -> Result<BrandingInfo, WifiError> {
    let settings = state.settings.clone();
    blocking(move || settings.set_branding(branding)).await
}

/// The logo's bytes as a raw response; empty when there is none.
#[tauri::command]
pub async fn branding_logo(state: State<'_, AppState>) -> Result<Response, WifiError> {
    let settings = state.settings.clone();
    let logo = blocking(move || settings.logo()).await?;
    Ok(Response::new(
        logo.map(|(bytes, _)| bytes).unwrap_or_default(),
    ))
}

/// The logo arrives as a raw body (PNG or JPEG, checked here).
#[tauri::command]
pub async fn set_branding_logo(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<BrandingInfo, WifiError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(WifiError::InvalidInput(
            "expected the logo as a binary body".into(),
        ));
    };
    let bytes = bytes.clone();
    let settings = state.settings.clone();
    blocking(move || settings.set_logo(&bytes)).await
}

#[tauri::command]
pub async fn clear_branding_logo(state: State<'_, AppState>) -> Result<BrandingInfo, WifiError> {
    let settings = state.settings.clone();
    blocking(move || settings.clear_logo()).await
}
