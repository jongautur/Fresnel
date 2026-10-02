//! [`WifiAdapterProvider`] backed by NetworkManager over the system D-Bus.
//!
//! NetworkManager is used for what needs authorisation (triggering scans,
//! via polkit) and for the BSS list, connection state and security flags.
//! NM's D-Bus API has gaps: signal is a 0–100 quality value (never dBm),
//! there is no 6 GHz flag, and no PHY/noise/utilisation data. When the
//! kernel's nl80211 interface is reachable (it is, unprivileged, on any
//! cfg80211 driver) those gaps are filled from it, matched by BSSID.
//! Without nl80211 the provider still works, with NM data only.

mod convert;
mod proxies;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument, warn};
use zbus::fdo::{DBusProxy, PropertiesProxy};
use zbus::names::{BusName, InterfaceName};
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::Connection;

use self::convert::*;
use self::proxies::*;
use super::nl80211::{self, BssMeasurement, Nl80211, WiphyInfo};
use super::sysfs;
use super::traits::WifiAdapterProvider;
use crate::error::{Result, WifiError};
use crate::wifi::channel::{band_for_frequency, channel_center_mhz, channel_for_frequency};
use crate::wifi::models::*;

pub const PROVIDER_ID: &str = "networkmanager";

/// Upper bound for any single D-Bus method call (and for connecting to the
/// bus). zbus waits forever by default, and a wedged NetworkManager (after
/// suspend/resume, a firmware crash) or a stuck polkit check would otherwise
/// block the caller indefinitely. NM answers healthy calls in milliseconds;
/// `RequestScan` returns before the scan runs.
const DBUS_METHOD_TIMEOUT: Duration = Duration::from_secs(10);
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
/// A `LastScan` change sooner than this after `RequestScan` is a scan that
/// was already running (e.g. a supplicant background scan) finishing, not
/// ours; its results can be partial. Observed: 185 ms "scans" that heard
/// only the associated AP.
const MIN_OWN_SCAN: Duration = Duration::from_millis(1200);

/// A Wi-Fi device as seen by NM at one point in time.
struct NmWifiDevice {
    path: OwnedObjectPath,
    interface: String,
    device: Props,
    wireless: Props,
}

impl NmWifiDevice {
    fn id(&self) -> AdapterId {
        AdapterId::linux(&self.interface)
    }

    fn state(&self) -> u32 {
        prop(&self.device, "State").unwrap_or(0)
    }

    fn active_ap(&self) -> Option<String> {
        prop_path(&self.wireless, "ActiveAccessPoint")
    }
}

#[derive(Default)]
pub struct NetworkManagerProvider {
    /// Lazily established; reset on transport failure or timeout so we
    /// reconnect.
    conn: Mutex<Option<Connection>>,
    nl: Nl80211,
}

impl NetworkManagerProvider {
    pub fn new() -> Self {
        Self::default()
    }

    async fn connection(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let connect = async {
            zbus::connection::Builder::system()?
                .method_timeout(DBUS_METHOD_TIMEOUT)
                .build()
                .await
        };
        let conn = tokio::time::timeout(DBUS_METHOD_TIMEOUT, connect)
            .await
            .map_err(|_| {
                WifiError::Timeout(format!(
                    "connecting to the system D-Bus took longer than {} s",
                    DBUS_METHOD_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                WifiError::ServiceUnavailable(format!("Cannot connect to the system D-Bus: {e}"))
            })?;
        debug!("connected to system D-Bus");
        *guard = Some(conn.clone());
        Ok(conn)
    }

    /// A call that timed out may have left the connection unusable (e.g. the
    /// bus or NM restarted across suspend/resume); drop it so the next call
    /// reconnects.
    async fn forget_on_timeout<T>(&self, r: Result<T>) -> Result<T> {
        if let Err(WifiError::Timeout(msg)) = &r {
            warn!(%msg, "D-Bus call timed out; reconnecting on next use");
            *self.conn.lock().await = None;
        }
        r
    }

    /// Connection plus a check that NM actually owns its bus name, so that we
    /// report "NetworkManager not running" rather than D-Bus activation noise.
    async fn nm(&self) -> Result<Connection> {
        let conn = self.connection().await?;
        let dbus = DBusProxy::new(&conn)
            .await
            .map_err(|e| map_zbus_error(e, "D-Bus"))?;
        let name = BusName::try_from(NM_SERVICE).expect("valid bus name");
        match dbus.name_has_owner(name).await {
            Ok(true) => Ok(conn),
            Ok(false) => Err(WifiError::ServiceUnavailable(NM_NOT_RUNNING.into())),
            Err(e) => {
                // Bus connection likely broken (e.g. dbus-daemon restarted).
                *self.conn.lock().await = None;
                Err(map_fdo_error(e, "D-Bus"))
            }
        }
    }

    async fn get_all(conn: &Connection, path: &str, iface: &'static str) -> Result<Props> {
        let proxy = PropertiesProxy::builder(conn)
            .destination(NM_SERVICE)
            .and_then(|b| b.path(path.to_owned()))
            .map_err(|e| map_zbus_error(e, path))?
            .build()
            .await
            .map_err(|e| map_zbus_error(e, path))?;
        proxy
            .get_all(InterfaceName::from_static_str_unchecked(iface))
            .await
            .map_err(|e| map_fdo_error(e, path))
    }

    async fn wifi_devices(conn: &Connection) -> Result<Vec<NmWifiDevice>> {
        let nm = NetworkManagerProxy::new(conn)
            .await
            .map_err(|e| map_zbus_error(e, "NetworkManager"))?;
        let paths = nm
            .get_devices()
            .await
            .map_err(|e| map_zbus_error(e, "GetDevices"))?;

        let mut out = Vec::new();
        for path in paths {
            // A device can vanish between GetDevices and GetAll; skip it.
            let device = match Self::get_all(conn, path.as_str(), IFACE_DEVICE).await {
                Ok(p) => p,
                Err(e) => {
                    debug!(%path, error = %e, "skipping device");
                    continue;
                }
            };
            if prop::<u32>(&device, "DeviceType") != Some(NM_DEVICE_TYPE_WIFI) {
                continue;
            }
            let Some(interface) = prop::<String>(&device, "Interface") else {
                continue;
            };
            let wireless = match Self::get_all(conn, path.as_str(), IFACE_WIRELESS).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(%interface, error = %e, "cannot read wireless properties");
                    Props::new()
                }
            };
            out.push(NmWifiDevice {
                path,
                interface,
                device,
                wireless,
            });
        }
        Ok(out)
    }

    async fn find_device(conn: &Connection, id: &AdapterId) -> Result<NmWifiDevice> {
        Self::wifi_devices(conn)
            .await?
            .into_iter()
            .find(|d| &d.id() == id)
            .ok_or_else(|| WifiError::AdapterNotFound(id.to_string()))
    }

    /// (software enabled, hardware enabled)
    async fn radio_state(conn: &Connection) -> (bool, bool) {
        let Ok(props) = Self::get_all(conn, NM_PATH, NM_SERVICE).await else {
            return (true, true);
        };
        (
            prop(&props, "WirelessEnabled").unwrap_or(true),
            prop(&props, "WirelessHardwareEnabled").unwrap_or(true),
        )
    }

    fn status_of(dev: &NmWifiDevice, radio: (bool, bool)) -> (AdapterStatus, Option<String>) {
        let rfkill = sysfs::rfkill_state(&dev.interface);
        if !radio.1 || rfkill.is_some_and(|r| r.hard_blocked) {
            return (
                AdapterStatus::RadioOff,
                Some("hardware radio switch (rfkill) is blocking Wi-Fi".into()),
            );
        }
        if !radio.0 {
            return (
                AdapterStatus::RadioOff,
                Some("Wi-Fi is disabled in NetworkManager".into()),
            );
        }
        if rfkill.is_some_and(|r| r.soft_blocked) {
            return (
                AdapterStatus::RadioOff,
                Some("Wi-Fi is soft-blocked by rfkill".into()),
            );
        }
        let status = device_status(dev.state());
        let detail = match status {
            AdapterStatus::Unmanaged => Some("device is not managed by NetworkManager".into()),
            AdapterStatus::Unavailable => {
                if prop::<bool>(&dev.device, "FirmwareMissing").unwrap_or(false) {
                    Some("firmware is missing".into())
                } else {
                    Some("device is unavailable".into())
                }
            }
            _ => None,
        };
        (status, detail)
    }

    async fn build_adapter(
        &self,
        conn: &Connection,
        dev: &NmWifiDevice,
        radio: (bool, bool),
    ) -> Adapter {
        let bus = sysfs::bus_info(&dev.interface);
        let driver = prop::<String>(&dev.device, "Driver").filter(|s| !s.is_empty());
        let display_name = display_name(bus.as_ref(), driver.as_deref(), &dev.interface);
        let (status, status_detail) = Self::status_of(dev, radio);

        let connected_ssid = match dev.active_ap() {
            Some(ap) => Self::get_all(conn, &ap, IFACE_ACCESS_POINT)
                .await
                .ok()
                .and_then(|p| prop_bytes(&p, "Ssid"))
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(&s).into_owned()),
            None => None,
        };

        let hw_address = prop::<String>(&dev.device, "HwAddress").map(|s| normalise_mac(&s));
        let permanent_hw_address = prop::<String>(&dev.wireless, "PermHwAddress")
            .map(|s| normalise_mac(&s))
            .filter(|p| !p.is_empty() && Some(p) != hw_address.as_ref());

        let mut capabilities = capabilities(prop(&dev.wireless, "WirelessCapabilities"));
        let mut data_sources = vec![PROVIDER_ID.to_string()];
        if let Some(ifindex) = nl80211::ifindex(&dev.interface) {
            match self.nl.wiphy(ifindex).await {
                Ok(wiphy) => {
                    apply_wiphy(&mut capabilities, &wiphy);
                    data_sources.push("nl80211".into());
                    // Whether the driver reports dBm is only visible in scan results.
                    if let Ok(bss) = self.nl.scan_dump(ifindex).await {
                        if bss.iter().any(|b| b.signal_dbm.is_some()) {
                            capabilities.signal_dbm = Capability::Supported;
                        } else if !bss.is_empty() {
                            capabilities.signal_dbm = Capability::Unsupported;
                        } else {
                            capabilities.signal_dbm = Capability::Unknown;
                        }
                    }
                }
                Err(e) => debug!(interface = %dev.interface, error = %e, "nl80211 unavailable"),
            }
        }

        Adapter {
            id: dev.id(),
            provider: PROVIDER_ID.into(),
            data_sources,
            interface_name: Some(dev.interface.clone()),
            display_name,
            driver,
            hw_address,
            permanent_hw_address,
            bus,
            capabilities,
            status,
            status_detail,
            connected_ssid,
        }
    }

    /// Add kernel measurements (dBm, freshness, PHY, BSS Load, noise) to
    /// NM's observations. Returns whether nl80211 data was available.
    async fn enrich(&self, dev: &NmWifiDevice, aps: &mut [AccessPointObservation]) -> bool {
        let Some(ifindex) = nl80211::ifindex(&dev.interface) else {
            return false;
        };
        let bss = match self.nl.scan_dump(ifindex).await {
            Ok(b) => b,
            Err(e) => {
                debug!(interface = %dev.interface, error = %e, "nl80211 scan dump unavailable");
                return false;
            }
        };
        let noise = self
            .nl
            .noise_by_frequency(ifindex)
            .await
            .unwrap_or_default();

        let mut by_bssid: HashMap<&str, Vec<&BssMeasurement>> = HashMap::new();
        for b in &bss {
            by_bssid.entry(b.bssid.as_str()).or_default().push(b);
        }
        let mut matched = 0;
        for ap in aps.iter_mut() {
            let Some(candidates) = by_bssid.get(ap.bssid.as_str()) else {
                // Not in the kernel cache: not heard for ~30 s. Keep NM's
                // (older) data; no dBm reading exists for it.
                continue;
            };
            let m = candidates
                .iter()
                .find(|m| m.frequency_mhz == ap.frequency_mhz)
                .or(candidates.first())
                .copied()
                .expect("non-empty");
            matched += 1;
            apply_measurement(ap, m, noise.get(&m.frequency_mhz).copied());
        }
        debug!(interface = %dev.interface, kernel = bss.len(), nm = aps.len(), matched, "nl80211 enrichment");
        true
    }

    /// Fail early with a precise reason when the radio can't scan.
    fn check_scannable(dev: &NmWifiDevice, radio: (bool, bool)) -> Result<()> {
        let (status, detail) = Self::status_of(dev, radio);
        let detail = detail.unwrap_or_default();
        match status {
            AdapterStatus::RadioOff => Err(WifiError::RadioDisabled(detail)),
            AdapterStatus::Unmanaged | AdapterStatus::Unavailable => {
                Err(WifiError::AdapterUnavailable {
                    id: dev.id().to_string(),
                    reason: detail,
                })
            }
            _ => Ok(()),
        }
    }

    /// Ask NM to scan and wait for `LastScan` to advance. Returns
    /// `(triggered, notice)`. Rejections that still leave usable cached
    /// results are reported as a notice rather than an error.
    async fn trigger_scan(
        conn: &Connection,
        dev: &NmWifiDevice,
        ssids: &[String],
    ) -> Result<(bool, Option<String>)> {
        let wireless = WirelessProxy::builder(conn)
            .path(dev.path.clone())
            .map_err(|e| map_zbus_error(e, &dev.interface))?
            .build()
            .await
            .map_err(|e| map_zbus_error(e, &dev.interface))?;

        let before: i64 = prop(&dev.wireless, "LastScan").unwrap_or(-1);
        // Subscribe before requesting so a fast scan can't be missed.
        let mut changes = wireless.receive_last_scan_changed().await;

        let options = || {
            let mut options: HashMap<&str, Value<'_>> = HashMap::new();
            if !ssids.is_empty() {
                let raw: Vec<Vec<u8>> = ssids.iter().map(|s| s.as_bytes().to_vec()).collect();
                options.insert("ssids", Value::from(raw));
            }
            options
        };

        let requested = Instant::now();
        if let Err(e) = wireless.request_scan(options()).await {
            return match map_zbus_error(e, "RequestScan") {
                WifiError::ScanRejected(msg) => {
                    warn!(interface = %dev.interface, %msg, "scan rejected; using cached results");
                    Ok((
                        false,
                        Some(format!(
                            "NetworkManager declined the scan ({msg}); showing cached results."
                        )),
                    ))
                }
                other => Err(other),
            };
        }

        let wait = async {
            let mut last = before;
            let mut asked_again = false;
            while let Some(change) = changes.next().await {
                let Ok(ts) = change.get().await else { continue };
                if ts == last || ts <= 0 {
                    continue;
                }
                if !asked_again && requested.elapsed() < MIN_OWN_SCAN {
                    // Someone else's scan just ended; the radio is free now.
                    asked_again = true;
                    last = ts;
                    debug!(interface = %dev.interface, "a running scan finished first; requesting ours again");
                    if wireless.request_scan(options()).await.is_ok() {
                        continue;
                    }
                }
                return ts;
            }
            last
        };
        match tokio::time::timeout(SCAN_TIMEOUT, wait).await {
            Ok(_) => Ok((true, None)),
            Err(_) => {
                warn!(interface = %dev.interface, "scan did not complete within {SCAN_TIMEOUT:?}");
                Ok((
                    false,
                    Some(format!(
                        "Scan did not complete within {} s; showing cached results.",
                        SCAN_TIMEOUT.as_secs()
                    )),
                ))
            }
        }
    }

    async fn read_access_points(
        conn: &Connection,
        dev: &NmWifiDevice,
    ) -> Result<Vec<AccessPointObservation>> {
        let wireless = WirelessProxy::builder(conn)
            .path(dev.path.clone())
            .map_err(|e| map_zbus_error(e, &dev.interface))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|e| map_zbus_error(e, &dev.interface))?;
        let paths = wireless
            .get_all_access_points()
            .await
            .map_err(|e| map_zbus_error(e, "GetAllAccessPoints"))?;

        let fetched = futures::future::join_all(
            paths
                .iter()
                .map(|p| Self::get_all(conn, p.as_str(), IFACE_ACCESS_POINT)),
        )
        .await;

        let active = dev.active_ap();
        let now = Utc::now();
        let boottime = boottime_secs();
        let adapter_id = dev.id();

        let mut out = Vec::with_capacity(paths.len());
        for (path, props) in paths.iter().zip(fetched) {
            match props {
                Ok(props) => {
                    let is_connected = active.as_deref() == Some(path.as_str());
                    if let Some(obs) = observation(&props, &adapter_id, now, boottime, is_connected)
                    {
                        out.push(obs);
                    }
                }
                // APs expire from NM's list continuously; a vanished one is normal.
                Err(e) => debug!(%path, error = %e, "access point vanished"),
            }
        }
        Ok(out)
    }

    async fn read_ipv4(conn: &Connection, dev: &NmWifiDevice) -> (Vec<String>, Option<String>) {
        let Some(path) = prop_path(&dev.device, "Ip4Config") else {
            return (vec![], None);
        };
        let Ok(props) = Self::get_all(conn, &path, IFACE_IP4_CONFIG).await else {
            return (vec![], None);
        };

        let mut addrs = Vec::new();
        if let Some(v) = props.get("AddressData") {
            if let Value::Array(arr) = &**v {
                for item in arr.iter() {
                    if let Value::Dict(d) = item {
                        let addr: Option<String> = d.get::<&str, String>(&"address").ok().flatten();
                        let prefix: Option<u32> = d.get::<&str, u32>(&"prefix").ok().flatten();
                        if let Some(a) = addr {
                            addrs.push(match prefix {
                                Some(p) => format!("{a}/{p}"),
                                None => a,
                            });
                        }
                    }
                }
            }
        }
        let gateway = prop::<String>(&props, "Gateway").filter(|g| !g.is_empty());
        (addrs, gateway)
    }
}

#[async_trait]
impl WifiAdapterProvider for NetworkManagerProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    async fn list_adapters(&self) -> Result<Vec<Adapter>> {
        let r = self.do_list_adapters().await;
        self.forget_on_timeout(r).await
    }

    async fn get_adapter(&self, id: &AdapterId) -> Result<Adapter> {
        let r = self.do_get_adapter(id).await;
        self.forget_on_timeout(r).await
    }

    async fn scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
        let r = self.do_scan(id, request).await;
        self.forget_on_timeout(r).await
    }

    async fn get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
        let r = self.do_get_current_connection(id).await;
        self.forget_on_timeout(r).await
    }
}

/// The trait methods' bodies; the wrappers above add connection recovery.
impl NetworkManagerProvider {
    #[instrument(skip(self))]
    async fn do_list_adapters(&self) -> Result<Vec<Adapter>> {
        let conn = self.nm().await?;
        let radio = Self::radio_state(&conn).await;
        let devices = Self::wifi_devices(&conn).await?;
        let mut adapters = Vec::with_capacity(devices.len());
        for dev in &devices {
            adapters.push(self.build_adapter(&conn, dev, radio).await);
        }
        Ok(adapters)
    }

    async fn do_get_adapter(&self, id: &AdapterId) -> Result<Adapter> {
        let conn = self.nm().await?;
        let radio = Self::radio_state(&conn).await;
        let dev = Self::find_device(&conn, id).await?;
        Ok(self.build_adapter(&conn, &dev, radio).await)
    }

    #[instrument(skip(self, request), fields(trigger = request.trigger))]
    async fn do_scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
        let started_at = Utc::now();
        let conn = self.nm().await?;
        let radio = Self::radio_state(&conn).await;
        let mut dev = Self::find_device(&conn, id).await?;
        Self::check_scannable(&dev, radio)?;

        let (scan_triggered, notice) = if request.trigger {
            let r = Self::trigger_scan(&conn, &dev, &request.ssids).await?;
            // Re-read: active AP and device state may have changed during the scan.
            dev = Self::find_device(&conn, id).await?;
            r
        } else {
            (false, None)
        };

        let mut access_points = Self::read_access_points(&conn, &dev).await?;
        let enriched = self.enrich(&dev, &mut access_points).await;
        info!(adapter = %id, count = access_points.len(), scan_triggered, enriched, "scan complete");
        Ok(ScanResult {
            adapter_id: id.clone(),
            provider: PROVIDER_ID.into(),
            started_at,
            completed_at: Utc::now(),
            scan_triggered,
            notice,
            access_points,
        })
    }

    async fn do_get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
        let conn = self.nm().await?;
        let dev = Self::find_device(&conn, id).await?;
        let Some(ap_path) = dev.active_ap() else {
            return Ok(None);
        };
        let ap = match Self::get_all(&conn, &ap_path, IFACE_ACCESS_POINT).await {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, "active AP vanished");
                return Ok(None);
            }
        };

        let frequency_mhz = prop::<u32>(&ap, "Frequency").filter(|f| *f > 0);
        let (ipv4_addresses, ipv4_gateway) = if dev.state() == NM_DEVICE_STATE_ACTIVATED {
            Self::read_ipv4(&conn, &dev).await
        } else {
            (vec![], None)
        };
        let bssid = prop::<String>(&ap, "HwAddress").map(|s| normalise_mac(&s));

        // Live link statistics from the kernel, if they are for the same AP.
        let station = match nl80211::ifindex(&dev.interface) {
            Some(ifindex) => match self.nl.station(ifindex).await {
                Ok(s) => s.filter(|s| Some(&s.bssid) == bssid.as_ref()),
                Err(e) => {
                    debug!(error = %e, "nl80211 station info unavailable");
                    None
                }
            },
            None => None,
        };
        let mut signal = prop::<u8>(&ap, "Strength")
            .map(Signal::from_quality)
            .unwrap_or_default();
        if let Some(s) = &station {
            signal.dbm = s.signal_avg_dbm.or(s.signal_dbm).map(f32::from);
        }

        Ok(Some(ConnectionInfo {
            adapter_id: id.clone(),
            interface_name: Some(dev.interface.clone()),
            ssid: prop_bytes(&ap, "Ssid")
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(&s).into_owned()),
            bssid,
            frequency_mhz,
            channel: frequency_mhz.and_then(channel_for_frequency),
            band: frequency_mhz.map(band_for_frequency),
            channel_width_mhz: prop::<u32>(&ap, "Bandwidth").filter(|b| *b > 0),
            signal,
            bitrate_kbps: prop::<u32>(&dev.wireless, "Bitrate").filter(|b| *b > 0),
            tx_rate: station.as_ref().and_then(|s| s.tx.clone()),
            rx_rate: station.as_ref().and_then(|s| s.rx.clone()),
            security: Some(security(
                prop(&ap, "Flags").unwrap_or(0),
                prop(&ap, "WpaFlags").unwrap_or(0),
                prop(&ap, "RsnFlags").unwrap_or(0),
            )),
            ipv4_addresses,
            ipv4_gateway,
        }))
    }
}

/// Build a normalised observation from AccessPoint properties.
fn observation(
    props: &Props,
    adapter_id: &AdapterId,
    now: chrono::DateTime<Utc>,
    boottime: Option<f64>,
    is_connected: bool,
) -> Option<AccessPointObservation> {
    let bssid = normalise_mac(&prop::<String>(props, "HwAddress")?);
    let frequency_mhz: u32 = prop(props, "Frequency")?;
    let ssid_raw = prop_bytes(props, "Ssid").unwrap_or_default();
    // Some hidden APs broadcast a zero-filled SSID of the real length.
    let hidden = ssid_raw.is_empty() || ssid_raw.iter().all(|b| *b == 0);
    let ssid = (!hidden).then(|| String::from_utf8_lossy(&ssid_raw).into_owned());

    // LastSeen: CLOCK_BOOTTIME seconds, -1 if never seen.
    let last_seen_age_ms = match (prop::<i32>(props, "LastSeen"), boottime) {
        (Some(seen), Some(now_s)) if seen >= 0 => {
            Some(((now_s - seen as f64).max(0.0) * 1000.0) as u64)
        }
        _ => None,
    };

    Some(AccessPointObservation {
        timestamp: now,
        adapter_id: adapter_id.clone(),
        bssid,
        ssid,
        ssid_raw,
        hidden,
        frequency_mhz,
        channel: channel_for_frequency(frequency_mhz),
        band: band_for_frequency(frequency_mhz),
        channel_width_mhz: prop::<u32>(props, "Bandwidth").filter(|b| *b > 0),
        // Refined with the HT Operation element if nl80211 data is available.
        channel_center_mhz: channel_center_mhz(
            frequency_mhz,
            prop::<u32>(props, "Bandwidth").filter(|b| *b > 0),
            None,
        ),
        signal: prop::<u8>(props, "Strength")
            .map(Signal::from_quality)
            .unwrap_or_default(),
        security: security(
            prop(props, "Flags").unwrap_or(0),
            prop(props, "WpaFlags").unwrap_or(0),
            prop(props, "RsnFlags").unwrap_or(0),
        ),
        mode: wifi_mode(prop(props, "Mode").unwrap_or(0)),
        max_bitrate_kbps: prop::<u32>(props, "MaxBitrate").filter(|b| *b > 0),
        last_seen_age_ms,
        is_connected,
        noise_dbm: None,
        snr_db: None,
        channel_utilization_pct: None,
        station_count: None,
        beacon_interval_tu: None,
        phy_type: None,
        wifi_generation: None,
    })
}

fn apply_wiphy(caps: &mut AdapterCapabilities, wiphy: &WiphyInfo) {
    let has = |b: Band| Capability::from_bool(wiphy.bands.contains(&b));
    caps.band_2ghz = has(Band::Band2_4GHz);
    caps.band_5ghz = has(Band::Band5GHz);
    caps.band_6ghz = has(Band::Band6GHz);
    caps.monitor_mode = Capability::from_bool(wiphy.monitor_mode);
    // 802.11 frame capture needs a monitor interface (and root).
    caps.packet_capture = Capability::from_bool(wiphy.monitor_mode);
    caps.ap_mode = Capability::from_bool(wiphy.ap_mode);
}

fn apply_measurement(ap: &mut AccessPointObservation, m: &BssMeasurement, noise_dbm: Option<f32>) {
    ap.signal.dbm = m.signal_dbm;
    if let Some(age) = m.seen_ms_ago {
        ap.last_seen_age_ms = Some(age as u64);
    }
    ap.beacon_interval_tu = m.beacon_interval_tu;
    let is_2ghz = ap.band == Band::Band2_4GHz;
    ap.wifi_generation = m.elements.wifi_generation(is_2ghz);
    ap.phy_type = Some(m.elements.phy_type(is_2ghz).to_string());
    ap.channel_utilization_pct = m.elements.channel_utilization_pct();
    ap.station_count = m.elements.station_count;
    ap.channel_center_mhz = channel_center_mhz(
        ap.frequency_mhz,
        ap.channel_width_mhz,
        m.elements.ht_secondary_offset,
    );
    ap.noise_dbm = noise_dbm;
    ap.snr_db = match (m.signal_dbm, noise_dbm) {
        (Some(s), Some(n)) => Some(s - n),
        _ => None,
    };
}

/// Seconds since boot including suspend (matches NM's CLOCK_BOOTTIME stamps).
fn boottime_secs() -> Option<f64> {
    // /proc/uptime is derived from CLOCK_BOOTTIME.
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn display_name(bus: Option<&BusInfo>, driver: Option<&str>, interface: &str) -> String {
    if let Some(bus) = bus {
        match (&bus.vendor_name, &bus.product_name) {
            (Some(v), Some(p)) => {
                let v = sysfs::short_vendor(v);
                return if p.to_lowercase().starts_with(&v.to_lowercase()) {
                    p.clone()
                } else {
                    format!("{v} {p}")
                };
            }
            (None, Some(p)) => return p.clone(),
            _ => {}
        }
    }
    match driver {
        Some(d) => format!("{d} Wi-Fi adapter"),
        None => format!("Wi-Fi adapter {interface}"),
    }
}
