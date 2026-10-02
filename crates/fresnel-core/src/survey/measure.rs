//! "Measure Here": run a fresh scan and store what was heard at a position.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tracing::{info, warn};

use super::models::{MeasuringAdapter, NewSurveyPoint, Sample, SurveyPoint};
use crate::database::Database;
use crate::error::{Result, WifiError};
use crate::wifi::models::{Adapter, AdapterId, BusKind, ScanRequest, ScanResult};
use crate::wifi::scanner::Scanner;

/// Attempts at getting a scan the backend actually ran (it may decline one
/// requested right after another, e.g. after a Live-view auto-scan).
const SCAN_ATTEMPTS: u32 = 4;
const RETRY_DELAY: Duration = Duration::from_millis(2500);
/// Slack when deciding whether a BSS was heard during the scan window (the
/// BSS ages are read a few ms after the scan's completion time is taken).
const FRESH_MARGIN_MS: i64 = 250;
/// "Other networks were in range moments ago": used to spot scans that
/// heard nothing but the associated AP.
const RECENTLY_HEARD_MS: u64 = 15_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureRequest {
    pub floor_id: i64,
    pub x: f64,
    pub y: f64,
    pub adapter_id: AdapterId,
    /// Proceed even though other adapters have already measured this floor.
    #[serde(default)]
    pub allow_adapter_change: bool,
}

pub async fn measure_here(
    scanner: &Scanner,
    db: Arc<Database>,
    req: MeasureRequest,
) -> Result<SurveyPoint> {
    let adapter = scanner
        .registry()
        .provider_for(&req.adapter_id)
        .await?
        .get_adapter(&req.adapter_id)
        .await?;
    let measuring = measuring_adapter(&adapter);

    // Validate before spending seconds on a scan.
    {
        let (db, req, measuring) = (db.clone(), req.clone(), measuring.clone());
        blocking(move || preflight(&db, &req, &measuring)).await?;
    }

    let (scan, samples) = fresh_scan(scanner, &req.adapter_id).await?;
    let window_ms = (scan.completed_at - scan.started_at).num_milliseconds();
    info!(
        floor = req.floor_id,
        adapter = %req.adapter_id,
        heard = samples.len(),
        listed = scan.access_points.len(),
        "measured survey point"
    );

    let new = NewSurveyPoint {
        floor_id: req.floor_id,
        x: req.x,
        y: req.y,
        measured_at: scan.completed_at,
        scan_duration_ms: window_ms,
        adapter: measuring,
        samples,
    };
    blocking(move || db.insert_survey_point(&new)).await
}

fn preflight(db: &Database, req: &MeasureRequest, measuring: &MeasuringAdapter) -> Result<()> {
    let floor = db.get_floor(req.floor_id)?.ok_or_else(|| {
        WifiError::InvalidInput(format!("floor {} no longer exists", req.floor_id))
    })?;
    let plan = floor
        .plan
        .ok_or_else(|| WifiError::InvalidInput("import a floor plan before measuring".into()))?;
    if !(req.x.is_finite() && req.y.is_finite())
        || req.x < 0.0
        || req.y < 0.0
        || req.x > plan.width
        || req.y > plan.height
    {
        return Err(WifiError::InvalidInput(
            "that position is outside the floor plan".into(),
        ));
    }
    if req.allow_adapter_change {
        return Ok(());
    }
    let others: Vec<_> = db
        .floor_adapters(req.floor_id)?
        .into_iter()
        .filter(|a| !same_radio(a, measuring))
        .collect();
    if let Some(first) = others.first() {
        return Err(WifiError::AdapterMismatch(format!(
            "'{}' was surveyed with {}; you are measuring with {}. \
             Different cards report different dBm for the same signal, so \
             mixing them on one floor skews the results.",
            floor.name,
            describe(first),
            describe(measuring)
        )));
    }
    Ok(())
}

/// Same adapter = same ID and, when both are known, the same hardware
/// (a different USB stick can come up under the same interface name).
fn same_radio(a: &MeasuringAdapter, b: &MeasuringAdapter) -> bool {
    a.id == b.id
        && match (&a.hw_id, &b.hw_id) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        }
}

fn describe(a: &MeasuringAdapter) -> String {
    let iface =
        a.id.as_str()
            .split_once(':')
            .map_or(a.id.as_str(), |(_, i)| i);
    match &a.model {
        Some(m) => format!("{m} ({iface})"),
        None => iface.to_string(),
    }
}

fn measuring_adapter(a: &Adapter) -> MeasuringAdapter {
    let bus = a.bus.as_ref();
    let hw_id = bus.and_then(|b| {
        let kind = match b.kind {
            BusKind::Pci => "pci",
            BusKind::Usb => "usb",
            BusKind::Sdio => "sdio",
            BusKind::Other => "other",
        };
        Some(format!(
            "{kind}:{}:{}",
            b.vendor_id.as_ref()?,
            b.product_id.as_ref()?
        ))
    });
    MeasuringAdapter {
        id: a.id.clone(),
        provider: a.provider.clone(),
        model: bus
            .and_then(|b| b.product_name.clone())
            .or_else(|| Some(a.display_name.clone()).filter(|s| !s.is_empty())),
        driver: a.driver.clone(),
        hw_id,
    }
}

/// A scan the hardware actually performed (never the backend's cache) that
/// heard a plausible set of networks, plus the samples heard during it.
async fn fresh_scan(scanner: &Scanner, adapter: &AdapterId) -> Result<(ScanResult, Vec<Sample>)> {
    let request = ScanRequest {
        trigger: true,
        ssids: vec![],
    };
    let mut problem = String::new();
    for attempt in 1..=SCAN_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(RETRY_DELAY).await;
        }
        let scan = scanner.scan(adapter, &request).await?;
        if !scan.scan_triggered {
            warn!(%adapter, attempt, notice = ?scan.notice, "measurement scan not performed; retrying");
            problem = scan
                .notice
                .unwrap_or_else(|| "the scan was not performed".into());
            continue;
        }
        let window_ms = (scan.completed_at - scan.started_at).num_milliseconds();
        let samples = fresh_samples(&scan, window_ms)?;
        if looks_incomplete(&scan, &samples) {
            warn!(%adapter, attempt, heard = samples.len(), "scan heard only the associated AP; retrying");
            problem = "the scan heard only the connected network although others were in range moments ago".into();
            continue;
        }
        return Ok((scan, samples));
    }
    Err(WifiError::ScanRejected(format!(
        "no usable scan after {SCAN_ATTEMPTS} attempts ({problem}). Nothing was saved; try again."
    )))
}

/// Signs that a scan "completed" without really covering the air:
///
/// * Heard nothing but the associated AP while other BSSes were heard
///   moments earlier: far more likely a cut-short scan than a dead zone (in
///   a real dead zone those entries age out after a few retries and the
///   point is saved).
/// * Some fresh BSSes have dBm and others don't: the dBm source (the kernel's
///   scan table) lost entries the backend still reports as just heard.
fn looks_incomplete(scan: &ScanResult, fresh: &[Sample]) -> bool {
    let with_dbm = fresh.iter().filter(|s| s.signal.dbm.is_some()).count();
    if with_dbm > 0 && with_dbm < fresh.len() {
        return true;
    }
    if fresh.iter().any(|s| !s.is_connected) {
        return false;
    }
    scan.access_points
        .iter()
        .any(|ap| !ap.is_connected && ap.last_seen_age_ms.is_some_and(|a| a <= RECENTLY_HEARD_MS))
}

/// Keep only BSSes heard during the scan window. Backends remember BSSes for
/// a while after they go quiet; those readings belong to wherever the
/// surveyor was earlier, not to this point.
fn fresh_samples(scan: &ScanResult, window_ms: i64) -> Result<Vec<Sample>> {
    let aps = &scan.access_points;
    if !aps.is_empty() && aps.iter().all(|ap| ap.last_seen_age_ms.is_none()) {
        return Err(WifiError::Unsupported(
            "this adapter does not report when each network was last heard, so fresh \
             readings can't be told apart from remembered ones"
                .into(),
        ));
    }
    let limit = window_ms.max(0) + FRESH_MARGIN_MS;
    Ok(one_per_bssid(
        aps.iter()
            .filter(|ap| {
                ap.last_seen_age_ms
                    .is_some_and(|age| i64::try_from(age).unwrap_or(i64::MAX) <= limit)
            })
            .map(Sample::from),
    ))
}

/// A point stores one sample per BSSID, but a scan can list a BSSID twice:
/// a hidden network seen via its beacon (no SSID) and a probe response (real
/// SSID), or one BSSID heard on two channels. Keep the most informative
/// reading deliberately (see [`better_reading`]) and log the rest; order of
/// first appearance is kept.
fn one_per_bssid(samples: impl IntoIterator<Item = Sample>) -> Vec<Sample> {
    let mut kept: Vec<Sample> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for sample in samples {
        let Some(&i) = index.get(&sample.bssid) else {
            index.insert(sample.bssid.clone(), kept.len());
            kept.push(sample);
            continue;
        };
        let dropped = if better_reading(&sample, &kept[i]) == Ordering::Greater {
            std::mem::replace(&mut kept[i], sample)
        } else {
            sample
        };
        let winner = &kept[i];
        if dropped.frequency_mhz != winner.frequency_mhz {
            // TODO(plan 2c): a BSSID on two frequencies in one scan is a
            // rogue-detection finding, not just a log line.
            warn!(
                bssid = %winner.bssid,
                kept_mhz = winner.frequency_mhz,
                dropped_mhz = dropped.frequency_mhz,
                "BSSID heard on two frequencies in one scan; keeping one reading"
            );
        } else {
            info!(
                bssid = %winner.bssid,
                kept_ssid = ?winner.ssid,
                dropped_ssid = ?dropped.ssid,
                "BSSID listed twice in one scan; keeping one reading"
            );
        }
    }
    kept
}

/// Which of two readings of one BSSID to keep: a known SSID over a hidden
/// one, then the more recently heard, then the stronger (dBm, else %).
/// Unknown values rank below known ones.
fn better_reading(a: &Sample, b: &Sample) -> Ordering {
    let named = |s: &Sample| s.ssid.as_deref().is_some_and(|n| !n.is_empty());
    // Smaller age = fresher; `Reverse` so unknown (None) ranks lowest.
    let fresh = |s: &Sample| s.last_seen_age_ms.map(std::cmp::Reverse);
    let dbm = |s: &Sample| s.signal.dbm.unwrap_or(f32::NEG_INFINITY);
    named(a)
        .cmp(&named(b))
        .then_with(|| fresh(a).cmp(&fresh(b)))
        .then_with(|| dbm(a).total_cmp(&dbm(b)))
        .then_with(|| a.signal.quality_percent.cmp(&b.signal.quality_percent))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WifiError::Backend(format!("database task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::models::*;
    use chrono::{Duration as ChronoDuration, Utc};

    fn ap(bssid: &str, age: Option<u64>) -> AccessPointObservation {
        AccessPointObservation {
            timestamp: Utc::now(),
            adapter_id: AdapterId::linux("wlan0"),
            bssid: bssid.into(),
            ssid: Some("x".into()),
            ssid_raw: b"x".to_vec(),
            hidden: false,
            frequency_mhz: 2412,
            channel: Some(1),
            band: Band::Band2_4GHz,
            channel_width_mhz: Some(20),
            channel_center_mhz: Some(2412),
            signal: Signal::from_dbm(-60.0),
            security: Security {
                kind: SecurityKind::Open,
                akms: vec![],
                pairwise_ciphers: vec![],
                group_ciphers: vec![],
                privacy: false,
                wpa: false,
                rsn: false,
            },
            mode: WifiMode::Infrastructure,
            max_bitrate_kbps: None,
            last_seen_age_ms: age,
            is_connected: false,
            noise_dbm: None,
            snr_db: None,
            channel_utilization_pct: None,
            station_count: None,
            beacon_interval_tu: None,
            phy_type: None,
            wifi_generation: None,
        }
    }

    fn scan(aps: Vec<AccessPointObservation>) -> ScanResult {
        let now = Utc::now();
        ScanResult {
            adapter_id: AdapterId::linux("wlan0"),
            provider: "test".into(),
            started_at: now - ChronoDuration::milliseconds(3000),
            completed_at: now,
            scan_triggered: true,
            notice: None,
            access_points: aps,
        }
    }

    #[test]
    fn drops_bsses_not_heard_during_the_scan() {
        let s = scan(vec![
            ap("A", Some(200)),
            ap("B", Some(3200)),
            ap("C", Some(3300)),
            ap("D", Some(25_000)),
            ap("E", None),
        ]);
        let kept: Vec<_> = fresh_samples(&s, 3000)
            .unwrap()
            .into_iter()
            .map(|s| s.bssid)
            .collect();
        assert_eq!(kept, ["A", "B"]);
    }

    #[test]
    fn empty_scan_is_a_valid_dead_zone() {
        let s = scan(vec![]);
        let fresh = fresh_samples(&s, 3000).unwrap();
        assert!(fresh.is_empty());
        assert!(!looks_incomplete(&s, &fresh));
    }

    #[test]
    fn flags_scans_that_heard_only_the_associated_ap() {
        let mut connected = ap("C", Some(50));
        connected.is_connected = true;
        // Observed on iwlwifi: back-to-back scan, kernel list flushed, only the
        // associated BSS came back; NM still remembers the rest from 4 s ago.
        let s = scan(vec![
            connected.clone(),
            ap("X", Some(4139)),
            ap("Y", Some(5139)),
        ]);
        let fresh = fresh_samples(&s, 1800).unwrap();
        assert_eq!(fresh.len(), 1);
        assert!(looks_incomplete(&s, &fresh));
        // Others last heard long ago: a genuine dead zone (apart from the link).
        let s = scan(vec![connected, ap("X", Some(40_000))]);
        let fresh = fresh_samples(&s, 1800).unwrap();
        assert!(!looks_incomplete(&s, &fresh));
        // Heard others: fine.
        let s = scan(vec![ap("X", Some(100))]);
        assert!(!looks_incomplete(&s, &fresh_samples(&s, 1800).unwrap()));
    }

    #[test]
    fn flags_fresh_bsses_missing_dbm() {
        let mut no_dbm = ap("Y", Some(300));
        no_dbm.signal = Signal::from_quality(60);
        let s = scan(vec![ap("X", Some(100)), no_dbm.clone()]);
        assert!(looks_incomplete(&s, &fresh_samples(&s, 1800).unwrap()));
        // A %-only adapter is consistent, not incomplete.
        let s = scan(vec![no_dbm]);
        assert!(!looks_incomplete(&s, &fresh_samples(&s, 1800).unwrap()));
    }

    #[test]
    fn keeps_one_reading_per_bssid() {
        // Hidden network: beacon without SSID (fresher, stronger) and probe
        // response with the real SSID. The named reading wins.
        let mut beacon = ap("H", Some(100));
        beacon.ssid = None;
        beacon.ssid_raw = vec![];
        beacon.hidden = true;
        beacon.signal = Signal::from_dbm(-50.0);
        let mut probe = ap("H", Some(900));
        probe.ssid = Some("Corp".into());
        probe.signal = Signal::from_dbm(-55.0);
        // One BSSID on two channels: the fresher wins over the stronger.
        let mut old_ch = ap("T", Some(1500));
        old_ch.signal = Signal::from_dbm(-40.0);
        let mut new_ch = ap("T", Some(200));
        new_ch.frequency_mhz = 2437;
        new_ch.channel = Some(6);
        new_ch.signal = Signal::from_dbm(-70.0);
        // Same age: the stronger wins.
        let mut weak = ap("S", Some(300));
        weak.signal = Signal::from_dbm(-80.0);
        let mut strong = ap("S", Some(300));
        strong.signal = Signal::from_dbm(-60.0);

        let s = scan(vec![
            beacon,
            ap("A", Some(100)),
            old_ch,
            probe,
            weak,
            new_ch,
            strong,
        ]);
        let kept = fresh_samples(&s, 3000).unwrap();
        let summary: Vec<_> = kept
            .iter()
            .map(|s| {
                (
                    s.bssid.as_str(),
                    s.ssid.as_deref(),
                    s.frequency_mhz,
                    s.signal.dbm,
                )
            })
            .collect();
        // First-appearance order.
        assert_eq!(
            summary,
            [
                ("H", Some("Corp"), 2412, Some(-55.0)),
                ("A", Some("x"), 2412, Some(-60.0)),
                ("T", Some("x"), 2437, Some(-70.0)),
                ("S", Some("x"), 2412, Some(-60.0)),
            ]
        );
    }

    #[test]
    fn unknown_values_rank_below_known_ones() {
        let mut no_age = Sample::from(&ap("X", None));
        let aged = Sample::from(&ap("X", Some(5000)));
        assert_eq!(better_reading(&aged, &no_age), Ordering::Greater);
        no_age.last_seen_age_ms = Some(5000);
        no_age.signal = Signal::from_quality(90);
        assert_eq!(better_reading(&aged, &no_age), Ordering::Greater);
        let mut empty_name = aged.clone();
        empty_name.ssid = Some(String::new());
        assert_eq!(better_reading(&empty_name, &aged), Ordering::Less);
    }

    #[test]
    fn refuses_when_freshness_is_unknowable() {
        let err = fresh_samples(&scan(vec![ap("A", None)]), 3000).unwrap_err();
        assert_eq!(err.kind(), "unsupported");
    }

    #[test]
    fn same_radio_needs_matching_hardware_when_known() {
        let a = MeasuringAdapter {
            id: AdapterId::linux("wlan1"),
            provider: "networkmanager".into(),
            model: Some("RTL8812AU".into()),
            driver: None,
            hw_id: Some("usb:0bda:8812".into()),
        };
        let other_stick = MeasuringAdapter {
            hw_id: Some("usb:0e8d:7961".into()),
            ..a.clone()
        };
        let unknown = MeasuringAdapter {
            hw_id: None,
            ..a.clone()
        };
        assert!(same_radio(&a, &a));
        assert!(!same_radio(&a, &other_stick));
        assert!(same_radio(&a, &unknown));
    }
}
