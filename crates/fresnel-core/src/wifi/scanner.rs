//! Provider-agnostic scan orchestration.
//!
//! The UI's Live view and the future survey "Measure Here" action both go
//! through [`Scanner`]; neither knows which provider produced the data.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::models::{AdapterId, ConnectionInfo, ScanRequest, ScanResult};
use crate::adapters::AdapterRegistry;
use crate::error::{Result, WifiError};

/// Minimum time between the end of one hardware scan and the start of the
/// next on the same adapter. Measured on NetworkManager + iwlwifi: scans
/// started < 5 s after the previous one often come back having heard only
/// the associated AP (4 of 6 at 4 s, 0 of 16 at ≥ 5 s).
const MIN_SCAN_GAP: Duration = Duration::from_millis(5500);

/// Overall limit for one provider call, resolving the provider included.
/// Providers bound their own steps (NetworkManager: 10 s per D-Bus call,
/// 15 s for the scan to finish, 5 s per nl80211 dump), but a run of slow
/// steps, or a provider without such limits, must still not hold the
/// adapter's lock forever: Tauri commands can't be cancelled from the UI,
/// so one hung call would block that adapter until restart. A healthy NM
/// scan takes 3–8 s, and one that falls back to cached results after its
/// 15 s wait still finishes well within this.
const PROVIDER_DEADLINE: Duration = Duration::from_secs(30);

pub struct Scanner {
    registry: Arc<AdapterRegistry>,
    /// One lock per adapter: concurrent scans on the same radio are serialised
    /// (the second caller gets fresh results instead of a rejected request),
    /// while different adapters scan in parallel.
    locks: Mutex<HashMap<AdapterId, Arc<Mutex<()>>>>,
    last: Mutex<HashMap<AdapterId, ScanResult>>,
    /// When each adapter's last hardware scan finished.
    last_triggered: Mutex<HashMap<AdapterId, Instant>>,
}

impl Scanner {
    pub fn new(registry: Arc<AdapterRegistry>) -> Self {
        Self {
            registry,
            locks: Mutex::new(HashMap::new()),
            last: Mutex::new(HashMap::new()),
            last_triggered: Mutex::new(HashMap::new()),
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

        if request.trigger {
            let previous = self.last_triggered.lock().await.get(adapter).copied();
            if let Some(wait) = previous.and_then(|t| MIN_SCAN_GAP.checked_sub(t.elapsed())) {
                debug!(adapter = %adapter, ?wait, "spacing out back-to-back scans");
                tokio::time::sleep(wait).await;
            }
        }

        let (provider, mut result) = within_deadline(adapter, "scan", async {
            let provider = self.registry.provider_for(adapter).await?;
            let result = provider.scan(adapter, request).await?;
            Ok((provider, result))
        })
        .await?;
        if result.scan_triggered {
            self.last_triggered
                .lock()
                .await
                .insert(adapter.clone(), Instant::now());
        }
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
        within_deadline(adapter, "reading the current connection", async {
            self.registry
                .provider_for(adapter)
                .await?
                .get_current_connection(adapter)
                .await
        })
        .await
    }
}

/// Run `call` under [`PROVIDER_DEADLINE`]. On expiry the call is dropped,
/// releasing whatever it holds, and `Timeout` is returned.
async fn within_deadline<T>(
    adapter: &AdapterId,
    what: &str,
    call: impl Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(PROVIDER_DEADLINE, call).await {
        Ok(r) => r,
        Err(_) => {
            warn!(%adapter, what, "provider did not finish within {PROVIDER_DEADLINE:?}");
            Err(WifiError::Timeout(format!(
                "{what} on {adapter} did not finish within {} s; the Wi-Fi service or \
                 driver may be stuck",
                PROVIDER_DEADLINE.as_secs()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::fake::{FakeProvider, Step};

    fn setup() -> (Arc<FakeProvider>, Scanner, AdapterId) {
        let fake = Arc::new(FakeProvider::new(&["wlan0"]));
        let registry = AdapterRegistry::new(vec![fake.clone()]);
        let scanner = Scanner::new(Arc::new(registry));
        (fake, scanner, FakeProvider::adapter_id("wlan0"))
    }

    fn triggered() -> ScanRequest {
        ScanRequest {
            trigger: true,
            ssids: vec![],
        }
    }

    fn assert_timeout<T: std::fmt::Debug>(r: Result<T>) {
        match r {
            Err(WifiError::Timeout(_)) => {}
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hung_scan_times_out_and_the_next_scan_runs() {
        let (fake, scanner, id) = setup();
        fake.push_scan(Step::Hang);

        let started = Instant::now();
        assert_timeout(scanner.scan(&id, &triggered()).await);
        let waited = started.elapsed();
        assert!(waited >= PROVIDER_DEADLINE && waited < PROVIDER_DEADLINE + Duration::from_secs(1));

        // The adapter is not left locked: the next scan goes straight through.
        let next = tokio::time::timeout(Duration::from_secs(1), scanner.scan(&id, &triggered()))
            .await
            .expect("second scan was blocked")
            .expect("second scan failed");
        assert!(next.scan_triggered);
        assert_eq!(fake.scan_calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn scan_waiting_behind_a_hung_one_runs_after_the_deadline() {
        let (fake, scanner, id) = setup();
        fake.push_scan(Step::Hang);

        let request = triggered();
        let started = Instant::now();
        // `join!` polls the first scan first, so it takes the adapter lock and
        // hangs; the second waits for that lock.
        let (first, second) =
            tokio::join!(scanner.scan(&id, &request), scanner.scan(&id, &request));
        assert_timeout(first);
        assert!(second.expect("second scan failed").scan_triggered);
        assert!(started.elapsed() < PROVIDER_DEADLINE + Duration::from_secs(1));
        assert_eq!(fake.scan_calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn errors_pass_through_and_release_the_adapter() {
        let (fake, scanner, id) = setup();
        fake.push_scan(Step::Fail(WifiError::ScanRejected("busy".into())));
        assert!(matches!(
            scanner.scan(&id, &triggered()).await,
            Err(WifiError::ScanRejected(_))
        ));
        scanner.scan(&id, &triggered()).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn triggered_scans_stay_spaced_apart() {
        let (_fake, scanner, id) = setup();
        scanner.scan(&id, &triggered()).await.unwrap();
        let started = Instant::now();
        scanner.scan(&id, &triggered()).await.unwrap();
        assert!(started.elapsed() >= MIN_SCAN_GAP);

        // Cached reads don't wait.
        let started = Instant::now();
        let cached = ScanRequest::default();
        scanner.scan(&id, &cached).await.unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn hung_connection_query_times_out() {
        let (fake, scanner, id) = setup();
        fake.push_connection(Step::Hang);
        assert_timeout(scanner.current_connection(&id).await);
        assert_eq!(scanner.current_connection(&id).await.unwrap(), None);
    }
}
