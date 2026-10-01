use fresnel_core::adapters::AdapterListing;
use fresnel_core::wifi::models::{Adapter, AdapterId};
use fresnel_core::WifiError;
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub async fn list_adapters(state: State<'_, AppState>) -> Result<AdapterListing, WifiError> {
    Ok(state.scanner.registry().list_adapters().await)
}

#[tauri::command]
pub async fn get_adapter(
    state: State<'_, AppState>,
    adapter_id: AdapterId,
) -> Result<Adapter, WifiError> {
    let provider = state.scanner.registry().provider_for(&adapter_id).await?;
    provider.get_adapter(&adapter_id).await
}
