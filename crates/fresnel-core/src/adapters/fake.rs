//! Test-only [`WifiAdapterProvider`] with scripted behaviour: no hardware,
//! no services.
//!
//! Each `scan` / `get_current_connection` call plays the next [`Step`] of its
//! script; with the script used up it answers successfully with nothing
//! heard / not connected. Meant to grow into the replay and fault-injection
//! provider (recorded scans, adapter vanishing mid-scan, stale entries).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

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

pub struct FakeProvider {
    adapters: Vec<Adapter>,
    scans: Mutex<VecDeque<Step<Vec<AccessPointObservation>>>>,
    connections: Mutex<VecDeque<Step<Option<ConnectionInfo>>>>,
    scan_calls: AtomicUsize,
}

impl FakeProvider {
    /// A provider serving one disconnected adapter per interface name.
    pub fn new(interfaces: &[&str]) -> Self {
        Self {
            adapters: interfaces.iter().map(|i| adapter(i)).collect(),
            scans: Mutex::default(),
            connections: Mutex::default(),
            scan_calls: AtomicUsize::new(0),
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

    /// How many times `scan` has been called (including ones still running).
    pub fn scan_calls(&self) -> usize {
        self.scan_calls.load(Ordering::SeqCst)
    }

    fn check_adapter(&self, id: &AdapterId) -> Result<()> {
        if self.adapters.iter().any(|a| &a.id == id) {
            Ok(())
        } else {
            Err(WifiError::AdapterNotFound(id.to_string()))
        }
    }
}

#[async_trait]
impl WifiAdapterProvider for FakeProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    async fn list_adapters(&self) -> Result<Vec<Adapter>> {
        Ok(self.adapters.clone())
    }

    async fn scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
        self.scan_calls.fetch_add(1, Ordering::SeqCst);
        self.check_adapter(id)?;
        let started_at = Utc::now();
        let step = self.scans.lock().unwrap().pop_front();
        let access_points = step.unwrap_or(Step::Return(vec![])).play().await?;
        Ok(ScanResult {
            adapter_id: id.clone(),
            provider: PROVIDER_ID.into(),
            started_at,
            completed_at: Utc::now(),
            scan_triggered: request.trigger,
            notice: None,
            access_points,
        })
    }

    async fn get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
        self.check_adapter(id)?;
        let step = self.connections.lock().unwrap().pop_front();
        step.unwrap_or(Step::Return(None)).play().await
    }
}

fn adapter(interface: &str) -> Adapter {
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
    }
}
