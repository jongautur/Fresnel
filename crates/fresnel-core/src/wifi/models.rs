//! Normalised Wi-Fi data model.
//!
//! Everything outside `adapters::*` works with these types only. Providers
//! translate their native structures into them. Fields a provider cannot
//! supply are `None`; never fabricate values (e.g. never convert a quality
//! percentage into dBm).
//!
//! All types serialise as camelCase JSON; the TypeScript mirror lives in
//! `src/types/wifi.ts` and must be kept in sync.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Adapters
// ---------------------------------------------------------------------------

/// Persistent internal adapter identity, e.g. `linux:wlp0s20f3`.
///
/// The prefix names the namespace the rest of the ID lives in (`linux:` for
/// kernel network interfaces, later e.g. `usbprobe:<serial>`), not the
/// provider: several providers may be able to drive the same Linux interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdapterId(pub String);

impl AdapterId {
    pub fn linux(interface_name: &str) -> Self {
        Self(format!("linux:{interface_name}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AdapterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Tri-state capability. `Unknown` means the provider cannot tell — it must
/// not be treated as "unsupported".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

impl Capability {
    pub fn from_bool(supported: bool) -> Self {
        if supported {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }

    pub fn is_supported(self) -> bool {
        self == Self::Supported
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterCapabilities {
    pub band_2ghz: Capability,
    pub band_5ghz: Capability,
    pub band_6ghz: Capability,
    /// Can request an active (probe request) scan.
    pub active_scan: Capability,
    /// Can perform a listen-only scan.
    pub passive_scan: Capability,
    pub monitor_mode: Capability,
    pub packet_capture: Capability,
    pub ap_mode: Capability,
    /// Scan results carry signal strength in dBm.
    pub signal_dbm: Capability,
    /// Scan results carry a 0–100 signal quality value.
    pub signal_quality: Capability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    Connected,
    Connecting,
    Disconnecting,
    Disconnected,
    /// Device exists but cannot be used (e.g. rfkill, missing firmware).
    Unavailable,
    /// Device exists but the provider's backend does not manage it.
    Unmanaged,
    /// Wi-Fi radio switched off (software or hardware rfkill).
    RadioOff,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusKind {
    Pci,
    Usb,
    Sdio,
    Other,
}

/// Hardware identification of the device behind an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusInfo {
    pub kind: BusKind,
    /// Hex, lowercase, no `0x` (e.g. `8086`).
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub vendor_name: Option<String>,
    pub product_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adapter {
    pub id: AdapterId,
    /// Provider currently serving this adapter, e.g. `networkmanager`.
    pub provider: String,
    /// Backends actually supplying data, e.g. `["networkmanager", "nl80211"]`.
    pub data_sources: Vec<String>,
    pub interface_name: Option<String>,
    pub display_name: String,
    pub driver: Option<String>,
    pub hw_address: Option<String>,
    /// Burned-in MAC, if different from `hw_address` (MAC randomisation).
    pub permanent_hw_address: Option<String>,
    pub bus: Option<BusInfo>,
    pub capabilities: AdapterCapabilities,
    pub status: AdapterStatus,
    /// Human-readable detail for `status` (e.g. "hardware rfkill switch is on").
    pub status_detail: Option<String>,
    /// SSID of the current connection, if any.
    pub connected_ssid: Option<String>,
    /// Minimum gap Fresnel keeps between triggered scans on this adapter
    /// ([`WifiAdapterProvider::min_scan_interval`]); `Some(0)` means none.
    /// Filled in by the registry when listing, so `None` from a provider
    /// itself means "not filled in", never "no spacing".
    ///
    /// [`WifiAdapterProvider::min_scan_interval`]: crate::adapters::WifiAdapterProvider::min_scan_interval
    pub scan_spacing_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// RF basics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Band {
    #[serde(rename = "2.4ghz")]
    Band2_4GHz,
    #[serde(rename = "5ghz")]
    Band5GHz,
    #[serde(rename = "6ghz")]
    Band6GHz,
    #[serde(rename = "60ghz")]
    Band60GHz,
    #[serde(rename = "unknown")]
    Unknown,
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Band::Band2_4GHz => "2.4 GHz",
            Band::Band5GHz => "5 GHz",
            Band::Band6GHz => "6 GHz",
            Band::Band60GHz => "60 GHz",
            Band::Unknown => "?",
        })
    }
}

/// Signal strength as reported by the hardware/provider.
///
/// `dbm` and `quality_percent` are independent: a provider fills whichever it
/// actually measured. NetworkManager's D-Bus API only provides
/// `quality_percent`; nl80211 provides `dbm`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    pub dbm: Option<f32>,
    pub quality_percent: Option<u8>,
}

impl Signal {
    pub fn from_quality(percent: u8) -> Self {
        Self {
            dbm: None,
            quality_percent: Some(percent.min(100)),
        }
    }

    pub fn from_dbm(dbm: f32) -> Self {
        Self {
            dbm: Some(dbm),
            quality_percent: None,
        }
    }

    /// Ordering key for "stronger first" sorting. dBm readings rank by dBm;
    /// readings with only a percentage rank by percentage. Mixed sets (which
    /// only occur if results from different providers are combined) put
    /// dBm readings first because they are the more precise measurement.
    pub fn sort_key(&self) -> (u8, i32) {
        match (self.dbm, self.quality_percent) {
            (Some(dbm), _) => (2, (dbm * 100.0) as i32),
            (None, Some(q)) => (1, q as i32),
            (None, None) => (0, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiMode {
    Infrastructure,
    AdHoc,
    Mesh,
    AccessPoint,
    Unknown,
}

// ---------------------------------------------------------------------------
// Security
// ---------------------------------------------------------------------------

/// Summary classification for display/filtering. Detail is in [`Security`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityKind {
    Open,
    /// Enhanced Open (OWE), including OWE transition mode.
    Owe,
    Wep,
    WpaPersonal,
    Wpa2Personal,
    Wpa3Personal,
    /// WPA2/WPA3 transition (PSK + SAE).
    Wpa2Wpa3Personal,
    WpaEnterprise,
    Wpa2Enterprise,
    /// WPA3-Enterprise 192-bit (Suite B).
    Wpa3Enterprise,
    Unknown,
}

/// Authentication and key management suites (IEEE 802.11-2020 Table 9-151;
/// WPA v1 suites map onto their RSN equivalents).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Akm {
    Psk,
    /// PSK with SHA-256 key derivation (00-0F-AC:6).
    PskSha256,
    /// 00-0F-AC:20.
    PskSha384,
    FtPsk,
    /// 00-0F-AC:19.
    FtPskSha384,
    Sae,
    /// SAE with group-dependent hash (00-0F-AC:24, used by Wi-Fi 7 / MLO).
    SaeExtKey,
    FtSae,
    FtSaeExtKey,
    Ieee8021x,
    /// 00-0F-AC:5.
    Ieee8021xSha256,
    /// 00-0F-AC:23.
    Ieee8021xSha384,
    FtIeee8021x,
    /// Suite B 128-bit (00-0F-AC:11).
    SuiteB,
    /// WPA3-Enterprise 192-bit (00-0F-AC:12).
    SuiteB192,
    /// FT over 802.1X with SHA-384 (00-0F-AC:13), the FT form of Suite B 192.
    FtSuiteB192,
    FilsSha256,
    FilsSha384,
    FtFilsSha256,
    FtFilsSha384,
    Owe,
    /// OWE Transition Mode element on the open BSS of an OWE pair. Not an
    /// AKM suite in the IE, but NetworkManager reports it as one.
    OweTransition,
    /// Any other suite: OUI in the upper 24 bits, suite type in the lowest 8
    /// (e.g. `0x000FAC15` for PASN). Serialised as a bare number.
    #[serde(untagged)]
    Unknown(u32),
}

/// Cipher suites (IEEE 802.11-2020 Table 9-149). `Bip*` only appear as the
/// group management cipher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cipher {
    Wep40,
    Wep104,
    Tkip,
    Ccmp,
    Ccmp256,
    Gcmp,
    Gcmp256,
    BipCmac128,
    BipCmac256,
    BipGmac128,
    BipGmac256,
    /// Any other suite, encoded like [`Akm::Unknown`]. Serialised as a bare
    /// number.
    #[serde(untagged)]
    Unknown(u32),
}

/// Protected Management Frames (802.11w), from the RSN Capabilities
/// MFPC/MFPR bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pmf {
    Disabled,
    Capable,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Security {
    pub kind: SecurityKind,
    /// Privacy bit set in the beacon capability field.
    pub privacy: bool,
    /// WPA (v1) information element present.
    pub wpa: bool,
    /// RSN (WPA2/WPA3) information element present.
    pub rsn: bool,
    pub akms: Vec<Akm>,
    pub pairwise_ciphers: Vec<Cipher>,
    pub group_ciphers: Vec<Cipher>,
    /// Group management (BIP) cipher; only the RSN element carries it.
    pub group_mgmt_cipher: Option<Cipher>,
    /// `None` when the source doesn't say (NetworkManager flags, no RSN
    /// Capabilities field).
    pub pmf: Option<Pmf>,
}

impl Security {
    pub fn unknown() -> Self {
        Self {
            kind: SecurityKind::Unknown,
            privacy: false,
            wpa: false,
            rsn: false,
            akms: vec![],
            pairwise_ciphers: vec![],
            group_ciphers: vec![],
            group_mgmt_cipher: None,
            pmf: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Access points & scans
// ---------------------------------------------------------------------------

/// One BSSID observed in one scan by one adapter.
///
/// The BSSID is the identity of an access point (radio/VAP). Observations
/// are never merged by SSID in the core; SSID grouping is presentation only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessPointObservation {
    pub timestamp: DateTime<Utc>,
    pub adapter_id: AdapterId,
    /// Uppercase colon-separated MAC, e.g. `78:20:51:37:70:62`.
    pub bssid: String,
    /// Wi-Fi 7 MLD MAC address from the Basic Multi-Link element, same form
    /// as `bssid`. Shared by all affiliated radios of one AP MLD.
    pub mld_address: Option<String>,
    /// SSID decoded as UTF-8 (lossy). `None` for hidden networks.
    pub ssid: Option<String>,
    /// SSID exactly as broadcast (SSIDs are arbitrary bytes, not text).
    pub ssid_raw: Vec<u8>,
    pub hidden: bool,
    pub frequency_mhz: u32,
    pub channel: Option<u16>,
    pub band: Band,
    pub channel_width_mhz: Option<u32>,
    /// Centre of the whole occupied channel (differs from `frequency_mhz`,
    /// the primary, for 40 MHz and wider). `None` if undeterminable.
    pub channel_center_mhz: Option<u32>,
    pub signal: Signal,
    pub security: Security,
    pub mode: WifiMode,
    pub max_bitrate_kbps: Option<u32>,
    /// How long before `timestamp` the BSS was last heard, if known.
    pub last_seen_age_ms: Option<u64>,
    pub is_connected: bool,

    // --- Extended RF metrics: not provided by NetworkManager. Reserved for
    // nl80211 / monitor-mode / external-probe providers.
    pub noise_dbm: Option<f32>,
    pub snr_db: Option<f32>,
    pub channel_utilization_pct: Option<f32>,
    /// Associated client count from the AP's BSS Load element.
    pub station_count: Option<u16>,
    pub beacon_interval_tu: Option<u16>,
    pub phy_type: Option<String>,
    /// e.g. 4 (n), 5 (ac), 6 (ax), 7 (be).
    pub wifi_generation: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRequest {
    /// `true`: ask the hardware for a fresh scan. `false`: return what the
    /// provider already has cached.
    pub trigger: bool,
    /// Directed probe for specific (possibly hidden) SSIDs. Optional.
    #[serde(default)]
    pub ssids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub adapter_id: AdapterId,
    pub provider: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    /// Whether a fresh hardware scan actually ran. `false` means the list is
    /// the provider's cache (e.g. the backend rate-limited the request).
    pub scan_triggered: bool,
    /// Non-fatal message about this scan, for display.
    pub notice: Option<String>,
    pub access_points: Vec<AccessPointObservation>,
}

// ---------------------------------------------------------------------------
// Current connection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub adapter_id: AdapterId,
    pub interface_name: Option<String>,
    pub ssid: Option<String>,
    pub bssid: Option<String>,
    pub frequency_mhz: Option<u32>,
    pub channel: Option<u16>,
    pub band: Option<Band>,
    pub channel_width_mhz: Option<u32>,
    pub signal: Signal,
    /// Current link rate.
    pub bitrate_kbps: Option<u32>,
    pub tx_rate: Option<LinkRate>,
    pub rx_rate: Option<LinkRate>,
    pub security: Option<Security>,
    /// IPv4 addresses in CIDR notation.
    pub ipv4_addresses: Vec<String>,
    pub ipv4_gateway: Option<String>,
}

/// Rate of the current link in one direction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRate {
    pub bitrate_kbps: Option<u32>,
    /// "HT", "VHT", "HE" or "EHT"; `None` for legacy rates.
    pub phy: Option<String>,
    pub mcs: Option<u8>,
    /// Spatial streams.
    pub nss: Option<u8>,
    pub width_mhz: Option<u32>,
    /// Short guard interval (HT/VHT); `None` when the provider doesn't say
    /// (Windows, and HE/EHT, which signal GI differently).
    pub short_gi: Option<bool>,
}
