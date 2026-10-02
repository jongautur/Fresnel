use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::traits::WifiAdapterProvider;
use crate::error::{Result, WifiError};
use crate::wifi::models::{Adapter, AdapterId};

/// [`ProviderIssue::provider`] when there is no provider at all for this OS.
pub const NO_PROVIDER: &str = "none";

/// A provider that failed while listing adapters. Reported alongside the
/// adapters other providers did return, so one broken backend (e.g.
/// NetworkManager stopped) doesn't hide hardware served by another.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIssue {
    pub provider: String,
    pub error: WifiError,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterListing {
    pub adapters: Vec<Adapter>,
    pub issues: Vec<ProviderIssue>,
}

/// Aggregates providers and routes adapter IDs to the provider serving them.
///
/// Providers are registered in priority order: when two providers report the
/// same adapter ID (e.g. NetworkManager and a future nl80211 provider both
/// seeing `linux:wlan0`), the first registered wins.
pub struct AdapterRegistry {
    providers: Vec<Arc<dyn WifiAdapterProvider>>,
    routes: RwLock<HashMap<AdapterId, usize>>,
    /// Adapters whose provider (by index) failed at the last listing, with
    /// that error. A provider that can't list doesn't prove its adapters are
    /// gone, so asking for one reports the outage rather than "unplugged".
    unreachable: RwLock<HashMap<AdapterId, (usize, WifiError)>>,
}

impl AdapterRegistry {
    pub fn new(providers: Vec<Arc<dyn WifiAdapterProvider>>) -> Self {
        Self {
            providers,
            routes: RwLock::new(HashMap::new()),
            unreachable: RwLock::new(HashMap::new()),
        }
    }

    pub async fn list_adapters(&self) -> AdapterListing {
        let mut adapters: Vec<Adapter> = Vec::new();
        let mut issues = Vec::new();
        let mut routes = HashMap::new();
        let mut failed: HashMap<usize, WifiError> = HashMap::new();

        // Not "no adapters": there may well be some, Fresnel just can't
        // talk to them on this OS yet.
        if self.providers.is_empty() {
            issues.push(ProviderIssue {
                provider: NO_PROVIDER.into(),
                error: WifiError::Unsupported(format!(
                    "Fresnel has no Wi-Fi provider for this operating system ({}) yet",
                    std::env::consts::OS
                )),
            });
        }

        for (idx, provider) in self.providers.iter().enumerate() {
            match provider.list_adapters().await {
                Ok(found) => {
                    debug!(
                        provider = provider.provider_id(),
                        count = found.len(),
                        "listed adapters"
                    );
                    for mut adapter in found {
                        if routes.contains_key(&adapter.id) {
                            continue;
                        }
                        let spacing = provider.min_scan_interval(&adapter.id);
                        adapter.scan_spacing_ms =
                            Some(u64::try_from(spacing.as_millis()).unwrap_or(u64::MAX));
                        routes.insert(adapter.id.clone(), idx);
                        adapters.push(adapter);
                    }
                }
                Err(error) => {
                    warn!(provider = provider.provider_id(), %error, "provider failed to list adapters");
                    failed.insert(idx, error.clone());
                    issues.push(ProviderIssue {
                        provider: provider.provider_id().to_string(),
                        error,
                    });
                }
            }
        }

        adapters.sort_by(|a, b| a.id.cmp(&b.id));
        let mut old_routes = self.routes.write().await;
        let mut unreachable = self.unreachable.write().await;
        // Unreachable until their provider lists again (then they are either
        // routed or really gone) or another provider serves them.
        unreachable.retain(|id, (idx, _)| failed.contains_key(idx) && !routes.contains_key(id));
        for (id, idx) in old_routes.iter() {
            if let Some(error) = failed.get(idx) {
                if !routes.contains_key(id) {
                    unreachable.insert(id.clone(), (*idx, error.clone()));
                }
            }
        }
        for (idx, error) in &failed {
            for entry in unreachable.values_mut().filter(|(i, _)| i == idx) {
                entry.1 = error.clone();
            }
        }
        *old_routes = routes;
        AdapterListing { adapters, issues }
    }

    /// Provider currently serving `id`. Refreshes the routing table once if
    /// the ID is unknown (adapter hot-plugged since the last listing).
    pub async fn provider_for(&self, id: &AdapterId) -> Result<Arc<dyn WifiAdapterProvider>> {
        if let Some(p) = self.lookup(id).await {
            return Ok(p);
        }
        let listing = self.list_adapters().await;
        if let Some(p) = self.lookup(id).await {
            return Ok(p);
        }
        if let Some((_, error)) = self.unreachable.read().await.get(id) {
            return Err(error.clone());
        }
        // If the only reason we can't find it is that every provider is down,
        // surface that instead of a misleading "not found".
        if listing.adapters.is_empty() && !listing.issues.is_empty() {
            return Err(listing.issues.into_iter().next().unwrap().error);
        }
        Err(WifiError::AdapterNotFound(id.to_string()))
    }

    async fn lookup(&self, id: &AdapterId) -> Option<Arc<dyn WifiAdapterProvider>> {
        let idx = *self.routes.read().await.get(id)?;
        self.providers.get(idx).cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::adapters::fake::{self, FakeProvider, Fault};
    use crate::wifi::models::ScanRequest;

    fn registry(providers: &[&Arc<FakeProvider>]) -> AdapterRegistry {
        AdapterRegistry::new(
            providers
                .iter()
                .map(|&p| p.clone() as Arc<dyn WifiAdapterProvider>)
                .collect(),
        )
    }

    fn ids(listing: &AdapterListing) -> Vec<&str> {
        listing.adapters.iter().map(|a| a.id.as_str()).collect()
    }

    #[tokio::test]
    async fn a_failing_provider_does_not_hide_the_others() {
        let down = Arc::new(FakeProvider::new(&["wlan9"]));
        down.inject("wlan9", Fault::ServiceDown);
        let up = Arc::new(FakeProvider::new(&["wlan1", "wlan0"]));
        let registry = registry(&[&down, &up]);

        let listing = registry.list_adapters().await;
        assert_eq!(ids(&listing), ["fake:wlan0", "fake:wlan1"]);
        assert_eq!(listing.issues.len(), 1);
        assert_eq!(listing.issues[0].provider, fake::PROVIDER_ID);
        assert_eq!(listing.issues[0].error.kind(), "service_unavailable");

        let id = FakeProvider::adapter_id("wlan0");
        let provider = registry.provider_for(&id).await.unwrap();
        provider.scan(&id, &ScanRequest::default()).await.unwrap();
        assert_eq!((down.scan_calls(), up.scan_calls()), (0, 1));
    }

    #[tokio::test]
    async fn first_registered_provider_wins_a_shared_adapter() {
        let first = Arc::new(FakeProvider::new(&["wlan0"]));
        let second = Arc::new(FakeProvider::new(&["wlan0", "wlan1"]));
        let registry = registry(&[&first, &second]);

        assert_eq!(
            ids(&registry.list_adapters().await),
            ["fake:wlan0", "fake:wlan1"]
        );
        for interface in ["wlan0", "wlan1"] {
            let id = FakeProvider::adapter_id(interface);
            let provider = registry.provider_for(&id).await.unwrap();
            provider.scan(&id, &ScanRequest::default()).await.unwrap();
        }
        assert_eq!((first.scan_calls(), second.scan_calls()), (1, 1));
    }

    #[tokio::test]
    async fn provider_for_finds_a_hot_plugged_adapter() {
        let fake = Arc::new(FakeProvider::new(&["wlan0"]));
        let registry = registry(&[&fake]);
        assert_eq!(ids(&registry.list_adapters().await), ["fake:wlan0"]);

        // Plugged in after the last listing: found by the one refresh.
        fake.plug(fake::adapter("wlan1"));
        let id = FakeProvider::adapter_id("wlan1");
        let provider = registry.provider_for(&id).await.unwrap();
        provider.scan(&id, &ScanRequest::default()).await.unwrap();

        // Unplugged: still routed until the next listing, and the provider
        // says it's gone; after a listing the registry says so itself.
        fake.inject("wlan1", Fault::Unplug);
        let provider = registry.provider_for(&id).await.unwrap();
        let err = provider.get_adapter(&id).await.unwrap_err();
        assert_eq!(err.kind(), "adapter_not_found");
        assert_eq!(ids(&registry.list_adapters().await), ["fake:wlan0"]);
        let err = registry.provider_for(&id).await.err().unwrap();
        assert_eq!(err.kind(), "adapter_not_found");

        let err = registry
            .provider_for(&FakeProvider::adapter_id("wlan7"))
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), "adapter_not_found");
    }

    #[tokio::test]
    async fn every_provider_down_reports_the_providers_error() {
        let a = Arc::new(FakeProvider::new(&["wlan0"]));
        let b = Arc::new(FakeProvider::new(&["wlan1"]));
        a.inject("wlan0", Fault::ServiceDown);
        b.inject("wlan1", Fault::ServiceDown);
        let registry = registry(&[&a, &b]);

        let listing = registry.list_adapters().await;
        assert!(listing.adapters.is_empty());
        assert_eq!(listing.issues.len(), 2);
        let err = registry
            .provider_for(&FakeProvider::adapter_id("wlan0"))
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), "service_unavailable");
    }

    #[tokio::test]
    async fn no_adapters_anywhere_is_not_found_without_issues() {
        let fake = Arc::new(FakeProvider::new(&[]));
        let registry = registry(&[&fake]);

        let listing = registry.list_adapters().await;
        assert!(listing.adapters.is_empty() && listing.issues.is_empty());
        let err = registry
            .provider_for(&FakeProvider::adapter_id("wlan0"))
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), "adapter_not_found");
    }

    #[tokio::test]
    async fn adapter_of_a_down_provider_reports_the_outage() {
        let down = Arc::new(FakeProvider::new(&["wlan9"]));
        let up = Arc::new(FakeProvider::new(&["wlan0"]));
        let registry = registry(&[&down, &up]);
        registry.list_adapters().await;

        down.inject("wlan9", Fault::ServiceDown);
        // E.g. the UI refreshing its adapter list while the service is down.
        registry.list_adapters().await;
        let err = registry
            .provider_for(&FakeProvider::adapter_id("wlan9"))
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), "service_unavailable");

        // Back up, but the adapter is really gone: "not found" again.
        down.inject("wlan9", Fault::Unplug);
        down.restore_service();
        registry.list_adapters().await;
        let err = registry
            .provider_for(&FakeProvider::adapter_id("wlan9"))
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), "adapter_not_found");
    }

    #[tokio::test]
    async fn no_providers_is_reported_as_unsupported() {
        let registry = AdapterRegistry::new(vec![]);
        let listing = registry.list_adapters().await;
        assert!(listing.adapters.is_empty());
        assert_eq!(listing.issues.len(), 1);
        assert_eq!(listing.issues[0].provider, NO_PROVIDER);
        assert!(matches!(listing.issues[0].error, WifiError::Unsupported(_)));

        // Routing says the same instead of "adapter not found".
        let id = FakeProvider::adapter_id("wlan0");
        assert!(matches!(
            registry.provider_for(&id).await,
            Err(WifiError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn listing_carries_the_providers_scan_spacing() {
        let fake = Arc::new(FakeProvider::new(&["wlan0"]));
        fake.set_min_scan_interval(Duration::from_millis(1500));
        let registry = AdapterRegistry::new(vec![fake]);
        let listing = registry.list_adapters().await;
        assert!(listing.issues.is_empty());
        assert_eq!(listing.adapters[0].scan_spacing_ms, Some(1500));
    }
}
