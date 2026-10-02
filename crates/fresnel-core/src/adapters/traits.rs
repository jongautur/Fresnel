use std::time::Duration;

use async_trait::async_trait;

use crate::error::{Result, WifiError};
use crate::wifi::models::{
    Adapter, AdapterCapabilities, AdapterId, ConnectionInfo, ScanRequest, ScanResult,
};

/// A source of Wi-Fi adapters and measurements.
///
/// Implementations: NetworkManager (D-Bus) today; nl80211, monitor-mode/pcap
/// and external USB probes later. Everything above this trait (scanner,
/// survey, database, UI) must stay ignorant of which implementation produced
/// a measurement.
///
/// Implementations must never panic on hardware/service conditions (service
/// down, adapter removed, radio off, permission denied); return a
/// [`WifiError`] instead.
#[async_trait]
pub trait WifiAdapterProvider: Send + Sync + 'static {
    /// Stable provider identifier, e.g. `networkmanager`.
    fn provider_id(&self) -> &'static str;

    /// All adapters this provider can currently serve.
    async fn list_adapters(&self) -> Result<Vec<Adapter>>;

    async fn get_adapter(&self, id: &AdapterId) -> Result<Adapter> {
        self.list_adapters()
            .await?
            .into_iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| WifiError::AdapterNotFound(id.to_string()))
    }

    async fn get_capabilities(&self, id: &AdapterId) -> Result<AdapterCapabilities> {
        Ok(self.get_adapter(id).await?.capabilities)
    }

    /// Minimum time from the end of one triggered scan on `id` to the start
    /// of the next. [`Scanner`](crate::wifi::scanner::Scanner) waits this
    /// long between back-to-back scans, because some service/driver
    /// combinations return incomplete results when scans come too quickly.
    /// Per adapter, since drivers (USB dongles in particular) differ.
    /// Should be measured on the hardware; the default, zero, adds no wait.
    fn min_scan_interval(&self, _id: &AdapterId) -> Duration {
        Duration::ZERO
    }

    /// Scan for access points. With `request.trigger == false` returns the
    /// provider's cached results without touching the radio.
    async fn scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult>;

    /// The adapter's current association, or `None` if not connected.
    async fn get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>>;
}
