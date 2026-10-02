//! Test-only [`WifiAdapterProvider`] that replays recorded scans and injects
//! faults: no hardware, no services.
//!
//! Scans are scripted at two levels:
//!
//! * Per adapter ([`FakeProvider::script`]): each `scan` on that adapter
//!   plays the next [`ScanStep`]: a recorded scan (fresh or from the
//!   backend's cache, optionally slow), a [`Fault`] that strikes mid-scan
//!   and persists (adapter unplugged, radio switched off, service stopped),
//!   or a one-off error.
//! * Provider-wide ([`FakeProvider::push_scan`]): a `scan` on an adapter with
//!   no script of its own plays the next [`Step`]; so does each
//!   `get_current_connection` ([`FakeProvider::push_connection`]).
//!
//! With the scripts used up a call answers successfully with nothing heard /
//! not connected. Faults can also be injected between calls
//! ([`FakeProvider::inject`]) and adapters hot-plugged ([`FakeProvider::plug`]).
//!
//! Timestamps come from tokio's clock, so scans and their windows line up
//! with paused-time tests. Recordings are `ScanResult` JSON as written by
//! `cargo run --example probe -- --scan --record <file>`, kept in
//! `fixtures/` next to this file and loaded with [`recording`].

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::time::Instant;

use super::traits::WifiAdapterProvider;
use crate::error::{Result, WifiError};
use crate::wifi::models::{
    AccessPointObservation, Adapter, AdapterCapabilities, AdapterId, AdapterStatus, ConnectionInfo,
    ScanRequest, ScanResult,
};

pub const PROVIDER_ID: &str = "fake";

/// What one scripted call does.
#[derive(Debug, Clone)]
pub enum Step<T> {
    /// Answer with this value.
    Return(T),
    /// Fail with this error.
    Fail(WifiError),
    /// Never answer, like a wedged service or driver.
    Hang,
}

impl<T> Step<T> {
    async fn play(self) -> Result<T> {
        match self {
            Self::Return(v) => Ok(v),
            Self::Fail(e) => Err(e),
            Self::Hang => std::future::pending().await,
        }
    }
}

/// What one scripted `scan` on one adapter does.
#[derive(Debug, Clone)]
pub enum ScanStep {
    /// The hardware scanned for `took`, then the backend listed `aps`.
    /// A cached-results request (`trigger: false`) gets the list without
    /// waiting.
    Fresh {
        took: Duration,
        aps: Vec<AccessPointObservation>,
    },
    /// The backend declined to scan (rate limit, scan in progress) and
    /// answered from its cache: `scan_triggered` is false.
    Cached {
        aps: Vec<AccessPointObservation>,
        notice: Option<String>,
    },
    /// `fault` strikes `after` into the scan, which fails with the error the
    /// fault causes. The fault persists for later calls.
    Fault { after: Duration, fault: Fault },
    /// Fail with this error; nothing persists.
    Fail(WifiError),
}

impl ScanStep {
    /// Replay a recorded scan: as long as it took and with the APs it
    /// listed, or as cached results if the backend didn't scan. Adapter IDs
    /// and timestamps are rewritten when played; the BSS ages are kept.
    pub fn replay(recording: ScanResult) -> Self {
        if recording.scan_triggered {
            Self::Fresh {
                took: (recording.completed_at - recording.started_at)
                    .to_std()
                    .unwrap_or_default(),
                aps: recording.access_points,
            }
        } else {
            Self::Cached {
                aps: recording.access_points,
                notice: recording.notice,
            }
        }
    }
}

/// A lasting failure, injected between calls or mid-scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The adapter is unplugged: gone from listings, and calls on it fail
    /// with `AdapterNotFound`.
    Unplug,
    /// The radio is switched off (rfkill): the adapter is listed as
    /// `RadioOff` and scans fail with `RadioDisabled`.
    RadioOff,
    /// The whole backend stops (service crashed or stopped): every call on
    /// every adapter, listing included, fails with `ServiceUnavailable`.
    ServiceDown,
}

pub struct FakeProvider {
    adapters: Mutex<Vec<Adapter>>,
    scans: Mutex<VecDeque<Step<Vec<AccessPointObservation>>>>,
    connections: Mutex<VecDeque<Step<Option<ConnectionInfo>>>>,
    scan_calls: AtomicUsize,
    scripts: Mutex<HashMap<AdapterId, VecDeque<ScanStep>>>,
    /// Set while the backend is down ([`Fault::ServiceDown`]).
    outage: Mutex<Option<String>>,
    /// Wall-clock time at `epoch`; [`Self::now`] advances with tokio's clock.
    epoch: (Instant, DateTime<Utc>),
    min_scan_interval: Mutex<Duration>,
}

impl FakeProvider {
    /// A provider serving one disconnected adapter per interface name.
    pub fn new(interfaces: &[&str]) -> Self {
        Self {
            adapters: Mutex::new(interfaces.iter().map(|i| adapter(i)).collect()),
            scans: Mutex::default(),
            connections: Mutex::default(),
            scan_calls: AtomicUsize::new(0),
            scripts: Mutex::default(),
            outage: Mutex::default(),
            epoch: (Instant::now(), Utc::now()),
            min_scan_interval: Mutex::new(Duration::ZERO),
        }
    }

    pub fn adapter_id(interface: &str) -> AdapterId {
        AdapterId(format!("{PROVIDER_ID}:{interface}"))
    }

    /// Queue the behaviour of the next unscripted `scan` call (any adapter).
    pub fn push_scan(&self, step: Step<Vec<AccessPointObservation>>) {
        self.scans.lock().unwrap().push_back(step);
    }

    /// Queue the behaviour of the next unscripted `get_current_connection`.
    pub fn push_connection(&self, step: Step<Option<ConnectionInfo>>) {
        self.connections.lock().unwrap().push_back(step);
    }

    /// Append to the script of scans on `interface`.
    pub fn script(&self, interface: &str, steps: impl IntoIterator<Item = ScanStep>) {
        self.scripts
            .lock()
            .unwrap()
            .entry(Self::adapter_id(interface))
            .or_default()
            .extend(steps);
    }

    /// Make `fault` happen now, on `interface` (or provider-wide).
    pub fn inject(&self, interface: &str, fault: Fault) {
        self.apply(&Self::adapter_id(interface), fault);
    }

    /// End a [`Fault::ServiceDown`] outage.
    pub fn restore_service(&self) {
        *self.outage.lock().unwrap() = None;
    }

    /// Hot-plug `adapter`, replacing any adapter with the same ID (e.g. a
    /// different USB stick that came up under the same interface name).
    pub fn plug(&self, adapter: Adapter) {
        let mut adapters = self.adapters.lock().unwrap();
        adapters.retain(|a| a.id != adapter.id);
        adapters.push(adapter);
    }

    /// What [`WifiAdapterProvider::min_scan_interval`] answers (default zero).
    pub fn set_min_scan_interval(&self, interval: Duration) {
        *self.min_scan_interval.lock().unwrap() = interval;
    }

    /// How many times `scan` has been called (including ones still running).
    pub fn scan_calls(&self) -> usize {
        self.scan_calls.load(Ordering::SeqCst)
    }

    /// Wall-clock time that follows tokio's (possibly paused) clock.
    fn now(&self) -> DateTime<Utc> {
        let (instant, wall) = self.epoch;
        wall + chrono::Duration::from_std(instant.elapsed()).unwrap_or_default()
    }

    /// Put `fault` into effect and return the error the interrupted call
    /// fails with.
    fn apply(&self, id: &AdapterId, fault: Fault) -> WifiError {
        match fault {
            Fault::Unplug => {
                self.adapters.lock().unwrap().retain(|a| &a.id != id);
                WifiError::AdapterNotFound(id.to_string())
            }
            Fault::RadioOff => {
                for a in self.adapters.lock().unwrap().iter_mut() {
                    if &a.id == id {
                        a.status = AdapterStatus::RadioOff;
                        a.status_detail = Some("software rfkill".into());
                        a.connected_ssid = None;
                    }
                }
                WifiError::RadioDisabled("software rfkill".into())
            }
            Fault::ServiceDown => {
                let message = "the fake Wi-Fi service stopped".to_string();
                *self.outage.lock().unwrap() = Some(message.clone());
                WifiError::ServiceUnavailable(message)
            }
        }
    }

    fn check_service(&self) -> Result<()> {
        match &*self.outage.lock().unwrap() {
            Some(message) => Err(WifiError::ServiceUnavailable(message.clone())),
            None => Ok(()),
        }
    }

    fn check_adapter(&self, id: &AdapterId) -> Result<Adapter> {
        self.check_service()?;
        self.adapters
            .lock()
            .unwrap()
            .iter()
            .find(|a| &a.id == id)
            .cloned()
            .ok_or_else(|| WifiError::AdapterNotFound(id.to_string()))
    }

    /// Play `id`'s next scripted step: `(scan_triggered, notice, aps)`, with
    /// the time spent scanning already waited out.
    async fn play_script(
        &self,
        id: &AdapterId,
        step: ScanStep,
        request: &ScanRequest,
    ) -> Result<(bool, Option<String>, Vec<AccessPointObservation>)> {
        match step {
            ScanStep::Fresh { took, aps } => {
                if request.trigger {
                    tokio::time::sleep(took).await;
                }
                Ok((request.trigger, None, aps))
            }
            ScanStep::Cached { aps, notice } => Ok((false, notice, aps)),
            ScanStep::Fault { after, fault } => {
                tokio::time::sleep(after).await;
                Err(self.apply(id, fault))
            }
            ScanStep::Fail(e) => Err(e),
        }
    }
}

#[async_trait]
impl WifiAdapterProvider for FakeProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    async fn list_adapters(&self) -> Result<Vec<Adapter>> {
        self.check_service()?;
        Ok(self.adapters.lock().unwrap().clone())
    }

    fn min_scan_interval(&self, _id: &AdapterId) -> Duration {
        *self.min_scan_interval.lock().unwrap()
    }

    async fn scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
        self.scan_calls.fetch_add(1, Ordering::SeqCst);
        if self.check_adapter(id)?.status == AdapterStatus::RadioOff {
            return Err(WifiError::RadioDisabled("software rfkill".into()));
        }
        let started_at = self.now();
        let scripted = self
            .scripts
            .lock()
            .unwrap()
            .get_mut(id)
            .and_then(VecDeque::pop_front);
        let (scan_triggered, notice, mut access_points) = match scripted {
            Some(step) => self.play_script(id, step, request).await?,
            None => {
                let step = self.scans.lock().unwrap().pop_front();
                let aps = step.unwrap_or(Step::Return(vec![])).play().await?;
                (request.trigger, None, aps)
            }
        };
        let completed_at = self.now();
        for ap in &mut access_points {
            ap.adapter_id = id.clone();
            ap.timestamp = completed_at;
        }
        Ok(ScanResult {
            adapter_id: id.clone(),
            provider: PROVIDER_ID.into(),
            started_at,
            completed_at,
            scan_triggered,
            notice,
            access_points,
        })
    }

    async fn get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
        self.check_adapter(id)?;
        let step = self.connections.lock().unwrap().pop_front();
        step.unwrap_or(Step::Return(None)).play().await
    }
}

/// A disconnected adapter as [`FakeProvider::new`] serves it; tests adjust
/// it (bus, status) before [`FakeProvider::plug`].
pub fn adapter(interface: &str) -> Adapter {
    Adapter {
        id: FakeProvider::adapter_id(interface),
        provider: PROVIDER_ID.into(),
        data_sources: vec![PROVIDER_ID.into()],
        interface_name: Some(interface.into()),
        display_name: format!("Fake adapter {interface}"),
        driver: None,
        hw_address: None,
        permanent_hw_address: None,
        bus: None,
        capabilities: AdapterCapabilities::default(),
        status: AdapterStatus::Disconnected,
        status_detail: None,
        connected_ssid: None,
        scan_spacing_ms: None,
    }
}

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("adapters")
        .join("fixtures")
}

/// The recorded scan `fixtures/<name>.json`.
pub fn recording(name: &str) -> ScanResult {
    let path = fixtures_dir().join(format!("{name}.json"));
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triggered() -> ScanRequest {
        ScanRequest {
            trigger: true,
            ssids: vec![],
        }
    }

    #[test]
    fn every_recording_parses() {
        let mut names = vec![];
        for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                let scan = recording(&name);
                assert!(scan.completed_at >= scan.started_at, "{name}");
                assert!(!scan.access_points.is_empty(), "{name}");
                names.push(name);
            }
        }
        names.sort();
        assert_eq!(
            names,
            ["connected_only", "hidden_duplicate", "office", "stale"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn replay_takes_as_long_as_the_recording_and_restamps_it() {
        let fake = FakeProvider::new(&["wlan0"]);
        let id = FakeProvider::adapter_id("wlan0");
        let office = recording("office");
        fake.script("wlan0", [ScanStep::replay(office.clone())]);

        let started = Instant::now();
        let scan = fake.scan(&id, &triggered()).await.unwrap();
        assert_eq!(started.elapsed(), Duration::from_millis(3150));
        assert_eq!(
            scan.completed_at - scan.started_at,
            office.completed_at - office.started_at
        );
        assert!(scan.scan_triggered);
        assert_eq!(scan.access_points.len(), office.access_points.len());
        for (played, recorded) in scan.access_points.iter().zip(&office.access_points) {
            assert_eq!(played.adapter_id, id);
            assert_eq!(played.timestamp, scan.completed_at);
            assert_eq!(played.last_seen_age_ms, recorded.last_seen_age_ms);
        }

        // Script used up: nothing heard.
        let rest = fake.scan(&id, &triggered()).await.unwrap();
        assert!(rest.access_points.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn faults_strike_mid_scan_and_persist() {
        let fake = FakeProvider::new(&["wlan0", "wlan1", "wlan2"]);
        let id = |i| FakeProvider::adapter_id(i);
        let after = Duration::from_secs(2);
        fake.script(
            "wlan0",
            [ScanStep::Fault {
                after,
                fault: Fault::Unplug,
            }],
        );
        fake.script(
            "wlan1",
            [ScanStep::Fault {
                after,
                fault: Fault::RadioOff,
            }],
        );

        let started = Instant::now();
        let err = fake.scan(&id("wlan0"), &triggered()).await.unwrap_err();
        assert_eq!(err.kind(), "adapter_not_found");
        assert_eq!(started.elapsed(), after);
        let err = fake.scan(&id("wlan1"), &triggered()).await.unwrap_err();
        assert_eq!(err.kind(), "radio_disabled");

        let listed = fake.list_adapters().await.unwrap();
        let ids: Vec<_> = listed.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["fake:wlan1", "fake:wlan2"]);
        assert_eq!(listed[0].status, AdapterStatus::RadioOff);
        let err = fake.scan(&id("wlan0"), &triggered()).await.unwrap_err();
        assert_eq!(err.kind(), "adapter_not_found");
        let err = fake.scan(&id("wlan1"), &triggered()).await.unwrap_err();
        assert_eq!(err.kind(), "radio_disabled");
        fake.scan(&id("wlan2"), &triggered()).await.unwrap();

        // Another stick comes up under the unplugged name.
        fake.plug(adapter("wlan0"));
        fake.scan(&id("wlan0"), &triggered()).await.unwrap();

        fake.inject("wlan2", Fault::ServiceDown);
        assert_eq!(
            fake.list_adapters().await.unwrap_err().kind(),
            "service_unavailable"
        );
        let err = fake.scan(&id("wlan0"), &triggered()).await.unwrap_err();
        assert_eq!(err.kind(), "service_unavailable");
        let err = fake.get_current_connection(&id("wlan2")).await.unwrap_err();
        assert_eq!(err.kind(), "service_unavailable");
    }

    #[tokio::test(start_paused = true)]
    async fn cached_steps_and_cached_requests_dont_scan() {
        let fake = FakeProvider::new(&["wlan0"]);
        let id = FakeProvider::adapter_id("wlan0");
        let office = recording("office");
        fake.script(
            "wlan0",
            [
                ScanStep::Cached {
                    aps: office.access_points.clone(),
                    notice: Some("scan already in progress".into()),
                },
                ScanStep::replay(office),
            ],
        );
        let started = Instant::now();
        let first = fake.scan(&id, &triggered()).await.unwrap();
        assert!(!first.scan_triggered);
        assert_eq!(first.notice.as_deref(), Some("scan already in progress"));
        // A recorded fresh scan played to a cached-results request.
        let second = fake.scan(&id, &ScanRequest::default()).await.unwrap();
        assert!(!second.scan_triggered);
        assert_eq!(second.access_points.len(), 6);
        assert_eq!(started.elapsed(), Duration::ZERO);
    }
}
