//! Tauri IPC commands. Thin wrappers: validation and orchestration live in
//! `fresnel-core`. Errors serialise as `{ kind, message }`.

pub mod adapters;
pub mod app;
pub mod diagnostics;
pub mod findings;
pub mod notes;
pub mod projects;
pub mod report;
pub mod requirements;
pub mod settings;
pub mod survey;
pub mod wifi;

use fresnel_core::database::Database;
use fresnel_core::WifiError;

use crate::state::AppState;

/// Run a blocking DB operation off the async runtime.
pub(crate) async fn with_db<T, F>(state: &AppState, f: F) -> Result<T, WifiError>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T, WifiError> + Send + 'static,
{
    let db = state.db()?;
    tokio::task::spawn_blocking(move || f(&db))
        .await
        .map_err(|e| WifiError::Backend(format!("database task failed: {e}")))?
}
