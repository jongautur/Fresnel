//! Survey data model: project → building → floor → point → samples.
//!
//! Point coordinates are in the floor plan's pixel space (the image's natural
//! size as rendered by the UI). Distances in metres come from the floor's
//! scale line, so a plan can be re-scaled without touching any point.
//!
//! Samples are stored raw: the dBm the adapter reported, plus which adapter
//! and model reported it. Calibration offsets are applied at display time.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::findings::PointAnomaly;
use crate::wifi::models::{
    AccessPointObservation, AdapterCapabilities, AdapterId, Akm, Band, Capability, Cipher, Pmf,
    SecurityKind, Signal,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Building {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewBuilding {
    pub project_id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Floor {
    pub id: i64,
    pub building_id: i64,
    pub name: String,
    /// Sort order within the building: -1 basement, 0 ground, 1 first, …
    pub level: i64,
    pub plan: Option<FloorPlan>,
    pub scale: Option<FloorScale>,
    pub point_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFloor {
    pub building_id: i64,
    pub name: String,
    #[serde(default)]
    pub level: i64,
}

/// A floor plan image stored in the app's `floorplans/` directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorPlan {
    /// File name only (never a path).
    pub file: String,
    pub mime: String,
    /// Natural size in px; the coordinate space for points and the scale line.
    pub width: f64,
    pub height: f64,
}

/// A reference line drawn on the plan and its real-world length.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorScale {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub length_m: f64,
}

impl FloorScale {
    pub fn length_px(&self) -> f64 {
        (self.x2 - self.x1).hypot(self.y2 - self.y1)
    }

    pub fn px_per_metre(&self) -> f64 {
        self.length_px() / self.length_m
    }
}

/// The adapter that took a measurement, as it identified itself then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasuringAdapter {
    pub id: AdapterId,
    pub provider: String,
    /// Chipset/product name, e.g. "Wireless-AC 9560". Calibration keys on this.
    pub model: Option<String>,
    pub driver: Option<String>,
    /// Bus and IDs, e.g. "pci:8086:a370".
    pub hw_id: Option<String>,
}

/// Which bands the measuring card could receive, so "no 6 GHz heard here"
/// isn't confused with "this card has no 6 GHz". Stored per point as JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterBands {
    pub band_2ghz: Capability,
    pub band_5ghz: Capability,
    pub band_6ghz: Capability,
}

impl AdapterBands {
    pub fn from_capabilities(c: &AdapterCapabilities) -> Self {
        Self {
            band_2ghz: c.band_2ghz,
            band_5ghz: c.band_5ghz,
            band_6ghz: c.band_6ghz,
        }
    }

    /// `Unknown` for bands outside 2.4/5/6 GHz.
    pub fn get(&self, band: Band) -> Capability {
        match band {
            Band::Band2_4GHz => self.band_2ghz,
            Band::Band5GHz => self.band_5ghz,
            Band::Band6GHz => self.band_6ghz,
            _ => Capability::Unknown,
        }
    }
}

/// One "Measure Here": a position on a floor and what was heard there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurveyPoint {
    pub id: i64,
    pub floor_id: i64,
    pub x: f64,
    pub y: f64,
    pub measured_at: DateTime<Utc>,
    pub scan_duration_ms: i64,
    pub adapter: MeasuringAdapter,
    /// `None` on points measured before this was recorded.
    pub adapter_bands: Option<AdapterBands>,
    /// Strongest first.
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone)]
pub struct NewSurveyPoint {
    pub floor_id: i64,
    pub x: f64,
    pub y: f64,
    pub measured_at: DateTime<Utc>,
    pub scan_duration_ms: i64,
    pub adapter: MeasuringAdapter,
    pub adapter_bands: Option<AdapterBands>,
    pub samples: Vec<Sample>,
    /// Readings Measure Here dropped while de-duplicating that hint at a
    /// problem (one BSSID on two frequencies in one scan).
    pub anomalies: Vec<PointAnomaly>,
}

/// One BSSID's reading at one survey point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub bssid: String,
    pub ssid: Option<String>,
    pub ssid_raw: Vec<u8>,
    pub frequency_mhz: u32,
    pub channel: Option<u16>,
    pub band: Band,
    pub channel_width_mhz: Option<u32>,
    pub channel_center_mhz: Option<u32>,
    pub signal: Signal,
    pub security: SecurityKind,
    pub phy_type: Option<String>,
    pub wifi_generation: Option<u8>,
    pub noise_dbm: Option<f32>,
    pub snr_db: Option<f32>,
    pub channel_utilization_pct: Option<f32>,
    pub station_count: Option<u16>,
    pub last_seen_age_ms: Option<u64>,
    pub is_connected: bool,
    /// Security detail, hidden flag and MLD address. `None` for points
    /// measured before these were recorded (schema v6).
    pub detail: Option<SampleDetail>,
}

/// What a sample records beyond the summary [`SecurityKind`], for rogue and
/// security-mismatch checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleDetail {
    pub akms: Vec<Akm>,
    pub pairwise_ciphers: Vec<Cipher>,
    pub group_ciphers: Vec<Cipher>,
    pub group_mgmt_cipher: Option<Cipher>,
    /// `None`: the source didn't say (e.g. NetworkManager flags only).
    pub pmf: Option<Pmf>,
    /// The beacon hides the SSID.
    pub hidden: bool,
    /// Wi-Fi 7 AP MLD address, shared by the AP's affiliated radios.
    pub mld_address: Option<String>,
}

impl From<&AccessPointObservation> for Sample {
    fn from(ap: &AccessPointObservation) -> Self {
        Self {
            bssid: ap.bssid.clone(),
            ssid: ap.ssid.clone(),
            ssid_raw: ap.ssid_raw.clone(),
            frequency_mhz: ap.frequency_mhz,
            channel: ap.channel,
            band: ap.band,
            channel_width_mhz: ap.channel_width_mhz,
            channel_center_mhz: ap.channel_center_mhz,
            signal: ap.signal,
            security: ap.security.kind,
            phy_type: ap.phy_type.clone(),
            wifi_generation: ap.wifi_generation,
            noise_dbm: ap.noise_dbm,
            snr_db: ap.snr_db,
            channel_utilization_pct: ap.channel_utilization_pct,
            station_count: ap.station_count,
            last_seen_age_ms: ap.last_seen_age_ms,
            is_connected: ap.is_connected,
            detail: Some(SampleDetail {
                akms: ap.security.akms.clone(),
                pairwise_ciphers: ap.security.pairwise_ciphers.clone(),
                group_ciphers: ap.security.group_ciphers.clone(),
                group_mgmt_cipher: ap.security.group_mgmt_cipher,
                pmf: ap.security.pmf,
                hidden: ap.hidden,
                mld_address: ap.mld_address.clone(),
            }),
        }
    }
}

/// A physical access point marked on a floor plan, with the BSSIDs it
/// broadcasts. Names resolve building-wide: an AP placed on one floor is
/// recognised when its BSSIDs are heard on another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacedAp {
    pub id: i64,
    pub floor_id: i64,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub model: Option<String>,
    pub notes: Option<String>,
    /// Uppercase colon-separated MACs, sorted.
    pub bssids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fields set when creating or editing a placed AP (the BSSID list is
/// replaced as a whole).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacedApInput {
    pub name: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub bssids: Vec<String>,
}
