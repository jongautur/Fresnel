//! Error type shared by providers, scanner, database and the IPC layer.
//!
//! Errors cross the Tauri boundary as `{ "kind": "...", "message": "..." }` so
//! the UI can react to the kind (e.g. show a "Wi-Fi is off" banner) without
//! parsing English text.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

pub type Result<T, E = WifiError> = std::result::Result<T, E>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum WifiError {
    #[error("{0}")]
    ServiceUnavailable(String),

    #[error("No Wi-Fi adapters were found")]
    NoAdapters,

    #[error("Adapter '{0}' was not found — it may have been unplugged or renamed")]
    AdapterNotFound(String),

    #[error("Adapter '{id}' is unavailable: {reason}")]
    AdapterUnavailable { id: String, reason: String },

    #[error("Wi-Fi radio is off: {0}")]
    RadioDisabled(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Scan request rejected: {0}")]
    ScanRejected(String),

    #[error("Timed out: {0}")]
    Timeout(String),

    #[error("Not supported: {0}")]
    Unsupported(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// A survey measurement would mix readings from different adapters on
    /// one floor. The caller may retry with explicit confirmation.
    #[error("{0}")]
    AdapterMismatch(String),

    #[error("{0}")]
    Backend(String),
}

impl WifiError {
    /// Stable machine-readable identifier, mirrored in `src/types/wifi.ts`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ServiceUnavailable(_) => "service_unavailable",
            Self::NoAdapters => "no_adapters",
            Self::AdapterNotFound(_) => "adapter_not_found",
            Self::AdapterUnavailable { .. } => "adapter_unavailable",
            Self::RadioDisabled(_) => "radio_disabled",
            Self::PermissionDenied(_) => "permission_denied",
            Self::ScanRejected(_) => "scan_rejected",
            Self::Timeout(_) => "timeout",
            Self::Unsupported(_) => "unsupported",
            Self::Database(_) => "database",
            Self::InvalidInput(_) => "invalid_input",
            Self::AdapterMismatch(_) => "adapter_mismatch",
            Self::Backend(_) => "backend",
        }
    }
}

impl Serialize for WifiError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("WifiError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

impl From<rusqlite::Error> for WifiError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<rusqlite_migration::Error> for WifiError {
    fn from(e: rusqlite_migration::Error) -> Self {
        Self::Database(format!("migration failed: {e}"))
    }
}
