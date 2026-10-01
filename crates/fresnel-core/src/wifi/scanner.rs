//! Provider-agnostic scan orchestration.
//!
//! The UI's Live view and the future survey "Measure Here" action both go
//! through [`Scanner`]; neither knows which provider produced the data.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use super::models::{AdapterId, ConnectionInfo, ScanRequest, ScanResult};
use crate::adapters::AdapterRegistry;
use crate::error::Result;

pub struct Scanner {
    registry: Arc<AdapterRegistry>,
    /// One lock per adapter: concurrent scans on the same radio are serialised
    /// (the second caller gets fresh results instead of a rejected request),
    /// while different adapters scan in parallel.
    locks: Mutex<HashMap<AdapterId, Arc<Mutex<()>>>>,
    last: Mutex<HashMap<AdapterId, ScanResult>>,
}

impl Scanner {
    pub fn new(registry: Arc<AdapterRegistry>) -> Self {
        Self {
            registry,
            locks: Mutex::new(HashMap::new()),
            last: Mutex::new(HashMap::new()),
        }
    }

    pub fn registry(&self) -> &Arc<AdapterRegistry> {
        &self.registry
    }

    async fn adapter_lock(&self, id: &AdapterId) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .await
            .entry(id.clone())
            .or_default()
            .clone()
    }

    /// Scan with `adapter`. Results are ordered strongest-first.
    pub async fn scan(&self, adapter: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
        let lock = self.adapter_lock(adapter).await;
        let _guard = lock.lock().await;

        let provider = self.registry.provider_for(adapter).await?;
        let mut result = provider.scan(adapter, request).await?;
        result.access_points.sort_by(|a, b| {
            b.signal
                .sort_key()
                .cmp(&a.signal.sort_key())
                .then_with(|| a.bssid.cmp(&b.bssid))
        });

        info!(
            adapter = %adapter,
            provider = provider.provider_id(),
            count = result.access_points.len(),
            elapsed_ms = (result.completed_at - result.started_at).num_milliseconds(),
            "scan finished"
        );
        self.last
            .lock()
            .await
            .insert(adapter.clone(), result.clone());
        Ok(result)
    }

    /// Most recent result obtained through this scanner for `adapter`.
    pub async fn last_result(&self, adapter: &AdapterId) -> Option<ScanResult> {
        self.last.lock().await.get(adapter).cloned()
    }

    pub async fn current_connection(&self, adapter: &AdapterId) -> Result<Option<ConnectionInfo>> {
        self.registry
            .provider_for(adapter)
            .await?
            .get_current_connection(adapter)
            .await
    }
}
