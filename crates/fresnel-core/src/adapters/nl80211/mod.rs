//! Read-only nl80211 (kernel cfg80211) access for Linux providers.
//!
//! Everything here works without privileges: wiphy, scan-result, station and
//! survey *dumps* are unprivileged. Triggering scans, monitor mode and frame
//! capture need CAP_NET_ADMIN and belong to a future root-capable provider,
//! which can reuse this module.
//!
//! Like [`super::sysfs`], this is a helper for providers, not a provider: the
//! NetworkManager provider uses it to add real dBm, band support and
//! freshness to NM's scan list.

mod ies;

use std::collections::HashMap;

use futures::TryStreamExt;
use tokio::sync::Mutex;
use tracing::debug;
use wl_nl80211::{
    new_connection, Nl80211Attr, Nl80211BandInfo, Nl80211BandType, Nl80211BssInfo, Nl80211Handle,
    Nl80211IfMode, Nl80211RateInfo, Nl80211StationInfo, Nl80211SurveyInfo,
};

pub use self::ies::ElementSummary;
use crate::error::{Result, WifiError};
use crate::wifi::models::{Band, LinkRate};

/// Hardware capabilities of the wiphy behind an interface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WiphyInfo {
    /// Bands the hardware supports (independent of regulatory restrictions).
    pub bands: Vec<Band>,
    pub monitor_mode: bool,
    pub ap_mode: bool,
}

/// One BSS from the kernel's scan cache.
#[derive(Debug, Clone, PartialEq)]
pub struct BssMeasurement {
    pub bssid: String,
    pub frequency_mhz: u32,
    pub signal_dbm: Option<f32>,
    /// 0–100 value from drivers that cannot report dBm.
    pub signal_unspec: Option<u8>,
    pub seen_ms_ago: Option<u32>,
    pub associated: bool,
    pub beacon_interval_tu: Option<u16>,
    pub elements: ElementSummary,
}

/// The associated AP as seen by the station (link) statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct StationLink {
    pub bssid: String,
    /// Signal of the last received frame.
    pub signal_dbm: Option<i8>,
    /// Driver-averaged signal; steadier than `signal_dbm`.
    pub signal_avg_dbm: Option<i8>,
    pub tx: Option<LinkRate>,
    pub rx: Option<LinkRate>,
}

/// Kernel interface index for `interface`.
pub fn ifindex(interface: &str) -> Option<u32> {
    std::fs::read_to_string(format!("/sys/class/net/{interface}/ifindex"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn mac_string(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn nl_error(context: &str, e: impl std::fmt::Display) -> WifiError {
    WifiError::Backend(format!("nl80211 {context}: {e}"))
}

#[derive(Default)]
pub struct Nl80211 {
    /// Lazily opened generic-netlink socket; reset after a failure.
    handle: Mutex<Option<Nl80211Handle>>,
}

impl Nl80211 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Must be called from within a tokio runtime (spawns the socket task).
    async fn handle(&self) -> Result<Nl80211Handle> {
        let mut guard = self.handle.lock().await;
        if let Some(h) = guard.as_ref() {
            return Ok(h.clone());
        }
        let (connection, handle, _) = new_connection().map_err(|e| nl_error("socket", e))?;
        tokio::spawn(connection);
        debug!("opened nl80211 socket");
        *guard = Some(handle.clone());
        Ok(handle)
    }

    async fn reset(&self) {
        *self.handle.lock().await = None;
    }

    async fn on_err<T>(&self, r: Result<T>) -> Result<T> {
        if r.is_err() {
            self.reset().await;
        }
        r
    }

    pub async fn wiphy(&self, ifindex: u32) -> Result<WiphyInfo> {
        let r = async {
            let handle = self.handle().await?;
            let mut stream = handle
                .wireless_physic()
                .get()
                .if_index(ifindex)
                .execute()
                .await;
            let mut info = WiphyInfo::default();
            // Split dumps spread bands and iftypes over many messages.
            while let Some(msg) = stream.try_next().await.map_err(|e| nl_error("wiphy", e))? {
                for attr in msg.payload.attributes {
                    match attr {
                        Nl80211Attr::WiphyBands(bands) => {
                            for band in bands {
                                let has_freqs = band.info.iter().any(
                                    |i| matches!(i, Nl80211BandInfo::Freqs(f) if !f.is_empty()),
                                );
                                let b = match band.kind {
                                    Nl80211BandType::Band2GHz => Band::Band2_4GHz,
                                    Nl80211BandType::Band5GHz => Band::Band5GHz,
                                    Nl80211BandType::Band6GHz => Band::Band6GHz,
                                    Nl80211BandType::Band60GHz => Band::Band60GHz,
                                    _ => continue,
                                };
                                if has_freqs && !info.bands.contains(&b) {
                                    info.bands.push(b);
                                }
                            }
                        }
                        Nl80211Attr::SupportedIftypes(modes) => {
                            info.monitor_mode |= modes.contains(&Nl80211IfMode::Monitor);
                            info.ap_mode |= modes.contains(&Nl80211IfMode::Ap);
                        }
                        _ => {}
                    }
                }
            }
            info.bands.sort();
            Ok(info)
        }
        .await;
        self.on_err(r).await
    }

    /// The kernel's current scan cache. cfg80211 expires entries ~30 s after
    /// they were last heard, so this list is also a freshness filter.
    pub async fn scan_dump(&self, ifindex: u32) -> Result<Vec<BssMeasurement>> {
        let r = async {
            let handle = self.handle().await?;
            let mut stream = handle.scan().dump(ifindex).execute().await;
            let mut out = Vec::new();
            while let Some(msg) = stream
                .try_next()
                .await
                .map_err(|e| nl_error("scan dump", e))?
            {
                for attr in msg.payload.attributes {
                    if let Nl80211Attr::Bss(info) = attr {
                        if let Some(m) = bss_measurement(info) {
                            out.push(m);
                        }
                    }
                }
            }
            Ok(out)
        }
        .await;
        self.on_err(r).await
    }

    /// Link statistics for the AP a managed interface is associated with.
    pub async fn station(&self, ifindex: u32) -> Result<Option<StationLink>> {
        let r = async {
            let handle = self.handle().await?;
            let mut stream = handle.station().dump(ifindex).execute().await;
            while let Some(msg) = stream
                .try_next()
                .await
                .map_err(|e| nl_error("station dump", e))?
            {
                let mut bssid = None;
                let mut link = StationLink {
                    bssid: String::new(),
                    signal_dbm: None,
                    signal_avg_dbm: None,
                    tx: None,
                    rx: None,
                };
                for attr in msg.payload.attributes {
                    match attr {
                        Nl80211Attr::Mac(mac) => bssid = Some(mac_string(&mac)),
                        Nl80211Attr::StationInfo(info) => {
                            for i in info {
                                match i {
                                    Nl80211StationInfo::Signal(s) => link.signal_dbm = Some(s),
                                    Nl80211StationInfo::SignalAvg(s) => {
                                        link.signal_avg_dbm = Some(s)
                                    }
                                    Nl80211StationInfo::TxBitrate(r) => {
                                        link.tx = Some(link_rate(&r))
                                    }
                                    Nl80211StationInfo::RxBitrate(r) => {
                                        link.rx = Some(link_rate(&r))
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(b) = bssid {
                    link.bssid = b;
                    return Ok(Some(link));
                }
            }
            Ok(None)
        }
        .await;
        self.on_err(r).await
    }

    /// Noise floor per channel centre frequency. Empty when the driver doesn't
    /// implement survey (e.g. iwlwifi).
    pub async fn noise_by_frequency(&self, ifindex: u32) -> Result<HashMap<u32, f32>> {
        let r = async {
            let handle = self.handle().await?;
            let mut stream = handle
                .survey()
                .dump(vec![Nl80211Attr::IfIndex(ifindex)])
                .execute()
                .await;
            let mut out = HashMap::new();
            while let Some(msg) = stream
                .try_next()
                .await
                .map_err(|e| nl_error("survey dump", e))?
            {
                for attr in msg.payload.attributes {
                    if let Nl80211Attr::SurveyInfo(info) = attr {
                        let mut freq = None;
                        let mut noise = None;
                        for i in info {
                            match i {
                                Nl80211SurveyInfo::Frequency(f) => freq = Some(f),
                                Nl80211SurveyInfo::Noise(n) => noise = Some(n as f32),
                                _ => {}
                            }
                        }
                        if let (Some(f), Some(n)) = (freq, noise) {
                            out.insert(f, n);
                        }
                    }
                }
            }
            Ok(out)
        }
        .await;
        self.on_err(r).await
    }
}

fn bss_measurement(info: Vec<Nl80211BssInfo>) -> Option<BssMeasurement> {
    let mut bssid = None;
    let mut frequency_mhz = None;
    let mut m = BssMeasurement {
        bssid: String::new(),
        frequency_mhz: 0,
        signal_dbm: None,
        signal_unspec: None,
        seen_ms_ago: None,
        associated: false,
        beacon_interval_tu: None,
        elements: ElementSummary::default(),
    };
    let mut ies: Option<Vec<u8>> = None;
    let mut beacon_ies: Option<Vec<u8>> = None;
    for i in info {
        match i {
            Nl80211BssInfo::Bssid(b) => bssid = Some(mac_string(&b)),
            Nl80211BssInfo::Frequency(f) => frequency_mhz = Some(f),
            Nl80211BssInfo::SignalMbm(s) => m.signal_dbm = Some(s as f32 / 100.0),
            Nl80211BssInfo::SignalUnspec(s) => m.signal_unspec = Some(s),
            Nl80211BssInfo::SeenMsAgo(s) => m.seen_ms_ago = Some(s),
            // NL80211_BSS_STATUS_ASSOCIATED
            Nl80211BssInfo::Status(s) => m.associated = s == 1,
            Nl80211BssInfo::BeaconInterval(b) => m.beacon_interval_tu = Some(b),
            Nl80211BssInfo::RawInformationElements(v) => ies = Some(v),
            Nl80211BssInfo::RawBeaconInformationElements(v) => beacon_ies = Some(v),
            _ => {}
        }
    }
    m.bssid = bssid?;
    m.frequency_mhz = frequency_mhz?;
    if let Some(ies) = ies.or(beacon_ies) {
        m.elements = ies::summarise(&ies);
    }
    Some(m)
}

fn link_rate(info: &[Nl80211RateInfo]) -> LinkRate {
    let mut r = LinkRate::default();
    for i in info {
        match *i {
            // Both are in units of 100 kbit/s; Bitrate32 supersedes Bitrate.
            Nl80211RateInfo::Bitrate32(v) => r.bitrate_kbps = Some(v * 100),
            Nl80211RateInfo::Bitrate(v) if r.bitrate_kbps.is_none() => {
                r.bitrate_kbps = Some(v as u32 * 100)
            }
            Nl80211RateInfo::Mcs(m) => {
                // HT MCS index encodes the stream count (8 MCS per stream).
                r.phy = Some("HT".into());
                r.mcs = Some(m % 8);
                r.nss = Some(m / 8 + 1);
            }
            Nl80211RateInfo::VhtMcs(m) => (r.phy, r.mcs) = (Some("VHT".into()), Some(m)),
            Nl80211RateInfo::VhtNss(n) => r.nss = Some(n),
            Nl80211RateInfo::HeMcs(m) => (r.phy, r.mcs) = (Some("HE".into()), Some(m)),
            Nl80211RateInfo::HeNss(n) => r.nss = Some(n),
            Nl80211RateInfo::EhtMcs(m) => (r.phy, r.mcs) = (Some("EHT".into()), Some(m)),
            Nl80211RateInfo::EhtNss(n) => r.nss = Some(n),
            Nl80211RateInfo::MhzWidth(w) => r.width_mhz = Some(w),
            Nl80211RateInfo::MhzWidth80Plus80 => r.width_mhz = Some(160),
            Nl80211RateInfo::ShortGi => r.short_gi = true,
            _ => {}
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vht_rate() {
        let r = link_rate(&[
            Nl80211RateInfo::Bitrate32(17333),
            Nl80211RateInfo::Bitrate(17333),
            Nl80211RateInfo::MhzWidth(160),
            Nl80211RateInfo::VhtMcs(9),
            Nl80211RateInfo::VhtNss(2),
            Nl80211RateInfo::ShortGi,
        ]);
        assert_eq!(r.bitrate_kbps, Some(1_733_300));
        assert_eq!(r.phy.as_deref(), Some("VHT"));
        assert_eq!(
            (r.mcs, r.nss, r.width_mhz, r.short_gi),
            (Some(9), Some(2), Some(160), true)
        );
    }

    #[test]
    fn ht_mcs_index() {
        let r = link_rate(&[Nl80211RateInfo::Mcs(15)]);
        assert_eq!((r.mcs, r.nss), (Some(7), Some(2)));
    }
}
