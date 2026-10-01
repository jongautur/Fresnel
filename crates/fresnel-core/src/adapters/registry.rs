use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::traits::WifiAdapterProvider;
use crate::error::{Result, WifiError};
use crate::wifi::models::{Adapter, AdapterId};

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
}

impl AdapterRegistry {
    pub fn new(providers: Vec<Arc<dyn WifiAdapterProvider>>) -> Self {
        Self {
            providers,
            routes: RwLock::new(HashMap::new()),
        }
    }

    pub async fn list_adapters(&self) -> AdapterListing {
        let mut adapters: Vec<Adapter> = Vec::new();
        let mut issues = Vec::new();
        let mut routes = HashMap::new();

        for (idx, provider) in self.providers.iter().enumerate() {
            match provider.list_adapters().await {
                Ok(found) => {
                    debug!(
                        provider = provider.provider_id(),
                        count = found.len(),
                        "listed adapters"
                    );
                    for adapter in found {
                        if routes.contains_key(&adapter.id) {
                            continue;
                        }
                        routes.insert(adapter.id.clone(), idx);
                        adapters.push(adapter);
                    }
                }
                Err(error) => {
                    warn!(provider = provider.provider_id(), %error, "provider failed to list adapters");
                    issues.push(ProviderIssue {
                        provider: provider.provider_id().to_string(),
                        error,
                    });
                }
            }
        }

        adapters.sort_by(|a, b| a.id.cmp(&b.id));
        *self.routes.write().await = routes;
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
