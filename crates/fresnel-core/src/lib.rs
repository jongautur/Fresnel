//! Fresnel core: hardware providers, normalised Wi-Fi models, scanning
//! and local storage. Deliberately independent of Tauri.

pub mod adapters;
pub mod database;
pub mod error;
pub mod nettools;
pub mod settings;
pub mod survey;
pub mod tools;
pub mod wifi;

pub use error::{Result, WifiError};

use std::sync::Arc;

use adapters::{AdapterRegistry, WifiAdapterProvider};

/// Registry with all providers available on this platform, in priority order.
/// Empty where Fresnel has no provider yet; the registry then reports that
/// instead of "no adapters".
pub fn default_registry() -> AdapterRegistry {
    AdapterRegistry::new(platform_providers())
}

#[cfg(target_os = "linux")]
fn platform_providers() -> Vec<Arc<dyn WifiAdapterProvider>> {
    vec![Arc::new(
        adapters::networkmanager::NetworkManagerProvider::new(),
    )]
}

#[cfg(target_os = "windows")]
fn platform_providers() -> Vec<Arc<dyn WifiAdapterProvider>> {
    vec![Arc::new(adapters::windows::WindowsProvider::new())]
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn platform_providers() -> Vec<Arc<dyn WifiAdapterProvider>> {
    Vec::new()
}
