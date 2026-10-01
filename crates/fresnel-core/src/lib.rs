//! Fresnel core: hardware providers, normalised Wi-Fi models, scanning
//! and local storage. Deliberately independent of Tauri.

pub mod adapters;
pub mod database;
pub mod error;
pub mod survey;
pub mod wifi;

pub use error::{Result, WifiError};

use std::sync::Arc;

use adapters::networkmanager::NetworkManagerProvider;
use adapters::{AdapterRegistry, WifiAdapterProvider};

/// Registry with all providers available on this platform, in priority order.
pub fn default_registry() -> AdapterRegistry {
    let providers: Vec<Arc<dyn WifiAdapterProvider>> =
        vec![Arc::new(NetworkManagerProvider::new())];
    AdapterRegistry::new(providers)
}
