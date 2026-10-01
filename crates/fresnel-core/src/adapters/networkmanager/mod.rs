//! [`WifiAdapterProvider`] backed by NetworkManager over the system D-Bus.
//!
//! Limitations inherent to NM's D-Bus API (documented so nobody "fixes" them
//! by faking data):
//!
//! * signal strength is a 0–100 quality value, never dBm;
//! * no 6 GHz capability flag;
//! * no noise, utilisation, beacon interval or PHY information.
//!
//! The future nl80211 provider fills these gaps.

mod convert;
mod proxies;

use std::collections::HashMap;
use std::time::Duration;

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
use super::sysfs;
use super::traits::WifiAdapterProvider;
use crate::error::{Result, WifiError};
use crate::wifi::channel::{band_for_frequency, channel_for_frequency};
use crate::wifi::models::*;

pub const PROVIDER_ID: &str = "networkmanager";

const SCAN_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// Lazily established; reset on transport failure so we reconnect.
    conn: Mutex<Option<Connection>>,
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
        let conn = Connection::system().await.map_err(|e| {
            WifiError::ServiceUnavailable(format!("Cannot connect to the system D-Bus: {e}"))
        })?;
        debug!("connected to system D-Bus");
        *guard = Some(conn.clone());
        Ok(conn)
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

    async fn build_adapter(conn: &Connection, dev: &NmWifiDevice, radio: (bool, bool)) -> Adapter {
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

        Adapter {
            id: dev.id(),
            provider: PROVIDER_ID.into(),
            interface_name: Some(dev.interface.clone()),
            display_name,
            driver,
            hw_address,
            permanent_hw_address,
            bus,
            capabilities: capabilities(prop(&dev.wireless, "WirelessCapabilities")),
            status,
            status_detail,
            connected_ssid,
        }
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

        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        if !ssids.is_empty() {
            let raw: Vec<Vec<u8>> = ssids.iter().map(|s| s.as_bytes().to_vec()).collect();
            options.insert("ssids", Value::from(raw));
        }

        if let Err(e) = wireless.request_scan(options).await {
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
            while let Some(change) = changes.next().await {
                if let Ok(ts) = change.get().await {
                    if ts != before && ts > 0 {
                        return ts;
                    }
                }
            }
            before
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

    #[instrument(skip(self))]
    async fn list_adapters(&self) -> Result<Vec<Adapter>> {
        let conn = self.nm().await?;
        let radio = Self::radio_state(&conn).await;
        let devices = Self::wifi_devices(&conn).await?;
        let mut adapters = Vec::with_capacity(devices.len());
        for dev in &devices {
            adapters.push(Self::build_adapter(&conn, dev, radio).await);
        }
        Ok(adapters)
    }

    async fn get_adapter(&self, id: &AdapterId) -> Result<Adapter> {
        let conn = self.nm().await?;
        let radio = Self::radio_state(&conn).await;
        let dev = Self::find_device(&conn, id).await?;
        Ok(Self::build_adapter(&conn, &dev, radio).await)
    }

    #[instrument(skip(self, request), fields(trigger = request.trigger))]
    async fn scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
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

        let access_points = Self::read_access_points(&conn, &dev).await?;
        info!(adapter = %id, count = access_points.len(), scan_triggered, "scan complete");
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

    async fn get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
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

        Ok(Some(ConnectionInfo {
            adapter_id: id.clone(),
            interface_name: Some(dev.interface.clone()),
            ssid: prop_bytes(&ap, "Ssid")
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(&s).into_owned()),
            bssid: prop::<String>(&ap, "HwAddress").map(|s| normalise_mac(&s)),
            frequency_mhz,
            channel: frequency_mhz.and_then(channel_for_frequency),
            band: frequency_mhz.map(band_for_frequency),
            channel_width_mhz: prop::<u32>(&ap, "Bandwidth").filter(|b| *b > 0),
            signal: prop::<u8>(&ap, "Strength")
                .map(Signal::from_quality)
                .unwrap_or_default(),
            bitrate_kbps: prop::<u32>(&dev.wireless, "Bitrate").filter(|b| *b > 0),
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
        beacon_interval_tu: None,
        phy_type: None,
        wifi_generation: None,
    })
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
