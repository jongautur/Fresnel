//! Windows Native Wifi (`wlanapi.dll`) provider.
//!
//! The unsafe calls are deliberately contained here. Blocking WLAN API calls
//! run on Tokio's blocking pool: Tauri commands cannot be cancelled from JS.

use std::ffi::c_void;
use std::mem::size_of;
use std::slice;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::Utc;
use tokio::task;
use tracing::warn;
use windows::core::GUID;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_SERVICE_NOT_ACTIVE, HANDLE,
};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::WiFi::*;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

use super::traits::WifiAdapterProvider;
use super::windows_convert::{
    band_capability_from_phys, filetime_age_ms, frequency_for_channel, frequency_khz_to_mhz,
    ie_range, rate_kbps, rssi_is_quality_derived,
};
use crate::error::{Result, WifiError};
use crate::wifi::channel::{band_for_frequency, channel_center_mhz, channel_for_frequency};
use crate::wifi::ies::summarise;
use crate::wifi::models::*;

pub const PROVIDER_ID: &str = "windows-native-wifi";
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
// Conservative until verified on each supported driver; see windows-testing.md.
const MIN_SCAN_GAP: Duration = Duration::from_secs(4);
const LOCATION_HINT: &str = "Turn on both Location services and ‘Let desktop apps access your location’ in Windows Settings (ms-settings:privacy-location). Your organisation may lock these settings by policy.";
const ERROR_NDIS_DOT11_POWER_STATE_INVALID: u32 = 0x8034_2002;

/// A service handle is process-wide and is reopened after WlanSvc restarts.
struct WlanHandle(HANDLE);
unsafe impl Send for WlanHandle {}
unsafe impl Sync for WlanHandle {}
impl Drop for WlanHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = WlanCloseHandle(self.0, None);
        }
    }
}

pub struct WindowsProvider {
    handle: Arc<Mutex<Option<Arc<WlanHandle>>>>,
    /// A WLAN handle has one notification callback: a second registration
    /// replaces the first, and unregistering stops both. Scans on different
    /// adapters therefore take turns (they share the WLAN service anyway).
    scanning: Arc<Mutex<()>>,
}
impl Default for WindowsProvider {
    fn default() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
            scanning: Arc::new(Mutex::new(())),
        }
    }
}
impl Clone for WindowsProvider {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            scanning: self.scanning.clone(),
        }
    }
}

impl WindowsProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn handle(&self) -> Result<Arc<WlanHandle>> {
        let mut slot = self
            .handle
            .lock()
            .map_err(|_| WifiError::Backend("Native Wifi handle lock poisoned".into()))?;
        if let Some(handle) = slot.as_ref() {
            return Ok(handle.clone());
        }
        let mut version = 0;
        let mut raw = HANDLE::default();
        let code = unsafe { WlanOpenHandle(2, None, &mut version, &mut raw) };
        check(code, "opening WLAN AutoConfig")?;
        let handle = Arc::new(WlanHandle(raw));
        *slot = Some(handle.clone());
        Ok(handle)
    }
    fn forget_handle(&self) {
        if let Ok(mut slot) = self.handle.lock() {
            *slot = None;
        }
    }
    fn retry_invalid<T>(&self, operation: impl Fn(&WlanHandle) -> Result<T>) -> Result<T> {
        let handle = self.handle()?;
        match operation(&handle) {
            Err(e) if is_invalid_handle(&e) => {
                self.forget_handle();
                operation(&*self.handle()?)
            }
            other => other,
        }
    }

    fn interfaces(&self) -> Result<Vec<Interface>> {
        self.retry_invalid(enum_interfaces)
    }
    fn scan_blocking(&self, id: AdapterId, request: ScanRequest) -> Result<ScanResult> {
        let started_at = Utc::now();
        let interface = self.find_interface(&id)?;
        let (triggered, mut notice) = if request.trigger {
            if request.ssids.len() > 1 {
                (true, Some("Windows Native Wifi can direct a scan to one SSID only; requested an undirected scan.".into()))
            } else {
                (true, None)
            }
        } else {
            (false, None)
        };
        if request.trigger {
            self.trigger_scan(&interface.guid, request.ssids.first().map(String::as_str))?;
        }
        let entries = self.bss_entries(&interface.guid)?;
        let derived = rssi_is_quality_derived(
            &entries
                .iter()
                .map(|b| (b.rssi, b.quality))
                .collect::<Vec<_>>(),
        );
        if derived {
            warn!(adapter = %id, "driver reports RSSI derived from link quality; suppressing dBm");
            notice = Some(match notice {
                Some(n) => format!("{n} Driver RSSI is derived from quality; showing % only."),
                None => "Driver RSSI is derived from quality; showing % only.".into(),
            });
        }
        let connected = self
            .retry_invalid(|h| connected_bss(h, &interface.guid))
            .ok()
            .flatten()
            .map(|(bssid, _)| bssid);
        let aps = entries
            .into_iter()
            .map(|b| observation(&id, b, derived, connected))
            .collect();
        Ok(ScanResult {
            adapter_id: id,
            provider: PROVIDER_ID.into(),
            started_at,
            completed_at: Utc::now(),
            scan_triggered: triggered,
            notice,
            access_points: aps,
        })
    }
    fn find_interface(&self, id: &AdapterId) -> Result<Interface> {
        self.interfaces()?
            .into_iter()
            .find(|i| AdapterId::windows(&guid_string(&i.guid)) == *id)
            .ok_or_else(|| WifiError::AdapterNotFound(id.to_string()))
    }
    fn trigger_scan(&self, guid: &GUID, ssid: Option<&str>) -> Result<()> {
        let _turn = self.scanning.lock().unwrap_or_else(|p| p.into_inner());
        let handle = self.handle()?;
        let (tx, rx) = mpsc::sync_channel(1);
        let context = Box::into_raw(Box::new(NotifyContext { guid: *guid, tx })) as *const c_void;
        let code = unsafe {
            WlanRegisterNotification(
                handle.0,
                WLAN_NOTIFICATION_SOURCE_ACM,
                true,
                Some(notification),
                Some(context),
                None,
                None,
            )
        };
        if let Err(e) = check(code, "registering scan notifications") {
            unsafe {
                drop(Box::from_raw(context as *mut NotifyContext));
            }
            return Err(e);
        }
        let ssid = ssid.and_then(dot11_ssid);
        let code = unsafe {
            WlanScan(
                handle.0,
                guid,
                ssid.as_ref().map(|s| s as *const _),
                None,
                None,
            )
        };
        let result = check(code, "requesting Wi-Fi scan").and_then(|_| {
            match rx.recv_timeout(SCAN_TIMEOUT) {
                Ok(Notify::Complete) => Ok(()),
                Ok(Notify::Failed) => Err(WifiError::ScanRejected(
                    "Windows reported that the scan failed".into(),
                )),
                Err(mpsc::RecvTimeoutError::Timeout) => Err(WifiError::Timeout(
                    "Windows Wi-Fi scan did not complete within 15 s".into(),
                )),
                Err(_) => Err(WifiError::Backend(
                    "Windows Wi-Fi notification channel closed".into(),
                )),
            }
        });
        // Unregister first; WlanApi guarantees no callback remains before the
        // context is freed. The callback only uses try_send and never blocks.
        unsafe {
            let _ = WlanRegisterNotification(
                handle.0,
                WLAN_NOTIFICATION_SOURCE_NONE,
                true,
                None,
                None,
                None,
                None,
            );
            drop(Box::from_raw(context as *mut NotifyContext));
        }
        result
    }
    fn bss_entries(&self, guid: &GUID) -> Result<Vec<Bss>> {
        self.retry_invalid(|h| bss_entries(h, guid))
    }
}

#[async_trait]
impl WifiAdapterProvider for WindowsProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn min_scan_interval(&self, _: &AdapterId) -> Duration {
        MIN_SCAN_GAP
    }
    async fn list_adapters(&self) -> Result<Vec<Adapter>> {
        let this = self.clone();
        task::spawn_blocking(move || {
            this.interfaces()?
                .into_iter()
                .map(|i| this.adapter(i))
                .collect()
        })
        .await
        .map_err(join_error)?
    }
    async fn scan(&self, id: &AdapterId, request: &ScanRequest) -> Result<ScanResult> {
        let this = self.clone();
        let id = id.clone();
        let request = request.clone();
        task::spawn_blocking(move || this.scan_blocking(id, request))
            .await
            .map_err(join_error)?
    }
    async fn get_current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
        let this = self.clone();
        let id = id.clone();
        task::spawn_blocking(move || this.current_connection(&id))
            .await
            .map_err(join_error)?
    }
}
impl WindowsProvider {
    fn current_connection(&self, id: &AdapterId) -> Result<Option<ConnectionInfo>> {
        let interface = self.find_interface(id)?;
        self.retry_invalid(|handle| current_connection(handle, id, &interface))
    }
    fn adapter(&self, i: Interface) -> Result<Adapter> {
        let radio_off = radio_off(&*self.handle()?, &i.guid).unwrap_or(false);
        let connected_ssid = connected_bss(&*self.handle()?, &i.guid)
            .ok()
            .flatten()
            .and_then(|(_, ssid)| ssid);
        Ok(Adapter {
            id: AdapterId::windows(&guid_string(&i.guid)),
            provider: PROVIDER_ID.into(),
            data_sources: vec![PROVIDER_ID.into()],
            interface_name: Some(i.name.clone()),
            display_name: i.name,
            driver: None,
            hw_address: None,
            permanent_hw_address: None,
            bus: None,
            capabilities: capabilities(&*self.handle()?, &i.guid),
            status: if radio_off {
                AdapterStatus::RadioOff
            } else {
                state(i.state)
            },
            status_detail: radio_off.then_some("Wi-Fi radio is switched off".into()),
            connected_ssid,
            scan_spacing_ms: None,
        })
    }
}

struct Interface {
    guid: GUID,
    name: String,
    state: WLAN_INTERFACE_STATE,
}
struct Bss {
    ssid: Vec<u8>,
    bssid: [u8; 6],
    rssi: i32,
    quality: u32,
    frequency_khz: u32,
    beacon: u16,
    host_time: u64,
    privacy: bool,
    ies: Vec<u8>,
    phy: DOT11_PHY_TYPE,
}
enum Notify {
    Complete,
    Failed,
}
struct NotifyContext {
    guid: GUID,
    tx: mpsc::SyncSender<Notify>,
}
unsafe extern "system" fn notification(data: *mut L2_NOTIFICATION_DATA, context: *mut c_void) {
    let (Some(data), Some(context)) = (unsafe { data.as_ref() }, unsafe {
        (context as *const NotifyContext).as_ref()
    }) else {
        return;
    };
    if data.InterfaceGuid != context.guid || data.NotificationSource != WLAN_NOTIFICATION_SOURCE_ACM
    {
        return;
    }
    let event = match data.NotificationCode as i32 {
        7 => Notify::Complete,
        8 => Notify::Failed,
        _ => return,
    };
    let _ = context.tx.try_send(event);
}

fn enum_interfaces(handle: &WlanHandle) -> Result<Vec<Interface>> {
    let mut ptr = std::ptr::null_mut();
    check(
        unsafe { WlanEnumInterfaces(handle.0, None, &mut ptr) },
        "enumerating Wi-Fi adapters",
    )?;
    let _guard = WlanMemory(ptr.cast());
    let list = unsafe { ptr.as_ref() }
        .ok_or_else(|| WifiError::Backend("WlanEnumInterfaces returned null".into()))?;
    let items = unsafe {
        slice::from_raw_parts(list.InterfaceInfo.as_ptr(), list.dwNumberOfItems as usize)
    };
    Ok(items
        .iter()
        .map(|i| Interface {
            guid: i.InterfaceGuid,
            name: utf16(&i.strInterfaceDescription),
            state: i.isState,
        })
        .collect())
}
fn bss_entries(handle: &WlanHandle, guid: &GUID) -> Result<Vec<Bss>> {
    let mut ptr = std::ptr::null_mut();
    check(
        unsafe {
            WlanGetNetworkBssList(
                handle.0,
                guid,
                None,
                dot11_BSS_type_any,
                false,
                None,
                &mut ptr,
            )
        },
        "reading Wi-Fi BSS list",
    )?;
    let _guard = WlanMemory(ptr.cast());
    let list = unsafe { ptr.as_ref() }
        .ok_or_else(|| WifiError::Backend("WlanGetNetworkBssList returned null".into()))?;
    let total = list.dwTotalSize as usize;
    let base = ptr.cast::<u8>();
    let items = unsafe {
        slice::from_raw_parts(list.wlanBssEntries.as_ptr(), list.dwNumberOfItems as usize)
    };
    Ok(items
        .iter()
        .filter_map(|e| {
            let entry_start = (e as *const _ as usize).checked_sub(base as usize)?;
            let range = ie_range(
                entry_start.checked_add(e.ulIeOffset as usize)? as u32,
                e.ulIeSize,
                total,
            )?;
            let ies = unsafe { slice::from_raw_parts(base.add(range.start), range.len()) }.to_vec();
            Some(Bss {
                ssid: e.dot11Ssid.ucSSID[..(e.dot11Ssid.uSSIDLength as usize).min(32)].to_vec(),
                bssid: e.dot11Bssid,
                rssi: e.lRssi,
                quality: e.uLinkQuality,
                frequency_khz: e.ulChCenterFrequency,
                beacon: e.usBeaconPeriod,
                host_time: e.ullHostTimestamp,
                privacy: e.usCapabilityInformation & 0x10 != 0,
                ies,
                phy: e.dot11BssPhyType,
            })
        })
        .collect())
}
fn capabilities(handle: &WlanHandle, guid: &GUID) -> AdapterCapabilities {
    let mut ptr = std::ptr::null_mut();
    let Ok(()) = check(
        unsafe { WlanGetInterfaceCapability(handle.0, guid, None, &mut ptr) },
        "reading Wi-Fi capabilities",
    ) else {
        return AdapterCapabilities {
            active_scan: Capability::Supported,
            passive_scan: Capability::Unknown,
            monitor_mode: Capability::Unsupported,
            packet_capture: Capability::Unsupported,
            signal_dbm: Capability::Supported,
            signal_quality: Capability::Supported,
            ..Default::default()
        };
    };
    let _guard = WlanMemory(ptr.cast());
    let Some(caps) = (unsafe { ptr.as_ref() }) else {
        return AdapterCapabilities::default();
    };
    let phys: Vec<i32> = caps.dot11PhyTypes[..(caps.dwNumberOfSupportedPhys as usize).min(64)]
        .iter()
        .map(|p| p.0)
        .collect();
    AdapterCapabilities {
        band_2ghz: band_capability_from_phys(&phys, Band::Band2_4GHz),
        band_5ghz: band_capability_from_phys(&phys, Band::Band5GHz),
        band_6ghz: band_capability_from_phys(&phys, Band::Band6GHz),
        active_scan: Capability::Supported,
        passive_scan: Capability::Unknown,
        monitor_mode: Capability::Unsupported,
        packet_capture: Capability::Unsupported,
        ap_mode: Capability::Unsupported,
        signal_dbm: Capability::Supported,
        signal_quality: Capability::Supported,
    }
}
fn current_connection(
    handle: &WlanHandle,
    id: &AdapterId,
    interface: &Interface,
) -> Result<Option<ConnectionInfo>> {
    let Some(attributes) = query_value::<WLAN_CONNECTION_ATTRIBUTES>(
        handle,
        &interface.guid,
        wlan_intf_opcode_current_connection,
        "reading current Wi-Fi connection",
    )?
    else {
        return Ok(None);
    };
    if attributes.isState != wlan_interface_state_connected {
        return Ok(None);
    }
    let assoc = attributes.wlanAssociationAttributes;
    let quality = u8::try_from(assoc.wlanSignalQuality)
        .ok()
        .filter(|q| *q <= 100);
    let rssi = query_value::<i32>(
        handle,
        &interface.guid,
        wlan_intf_opcode_rssi,
        "reading current Wi-Fi RSSI",
    )?;
    let channel = query_value::<u32>(
        handle,
        &interface.guid,
        wlan_intf_opcode_channel_number,
        "reading current Wi-Fi channel",
    )?
    .and_then(|c| u16::try_from(c).ok());
    // The channel-number opcode does not carry a band. Match the connected
    // BSSID against the BSS list to retain its actual primary frequency.
    let frequency_mhz = bss_entries(handle, &interface.guid)
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|b| b.bssid == assoc.dot11Bssid)
                .and_then(|b| frequency_khz_to_mhz(b.frequency_khz))
        })
        .or_else(|| channel.and_then(frequency_for_channel));
    let (ipv4_addresses, ipv4_gateway) = ipv4_for_guid(&guid_string(&interface.guid));
    Ok(Some(ConnectionInfo {
        adapter_id: id.clone(),
        interface_name: Some(interface.name.clone()),
        ssid: ssid_text(&assoc.dot11Ssid),
        bssid: Some(mac(assoc.dot11Bssid)),
        frequency_mhz,
        channel,
        band: frequency_mhz.map(band_for_frequency),
        channel_width_mhz: None,
        signal: Signal {
            dbm: rssi.map(|v| v as f32),
            quality_percent: quality,
        },
        bitrate_kbps: rate_kbps(assoc.ulTxRate).or_else(|| rate_kbps(assoc.ulRxRate)),
        tx_rate: Some(LinkRate {
            bitrate_kbps: rate_kbps(assoc.ulTxRate),
            phy: Some(phy_name(assoc.dot11PhyType).into()),
            mcs: None,
            nss: None,
            width_mhz: None,
            short_gi: None,
        }),
        rx_rate: Some(LinkRate {
            bitrate_kbps: rate_kbps(assoc.ulRxRate),
            phy: Some(phy_name(assoc.dot11PhyType).into()),
            mcs: None,
            nss: None,
            width_mhz: None,
            short_gi: None,
        }),
        security: Some(native_security(attributes.wlanSecurityAttributes)),
        ipv4_addresses,
        ipv4_gateway,
    }))
}
/// The BSSID and SSID of the current association, if connected.
fn connected_bss(handle: &WlanHandle, guid: &GUID) -> Result<Option<([u8; 6], Option<String>)>> {
    let attributes = query_value::<WLAN_CONNECTION_ATTRIBUTES>(
        handle,
        guid,
        wlan_intf_opcode_current_connection,
        "reading current Wi-Fi connection",
    )?;
    Ok(attributes
        .filter(|a| a.isState == wlan_interface_state_connected)
        .map(|a| {
            let assoc = a.wlanAssociationAttributes;
            (assoc.dot11Bssid, ssid_text(&assoc.dot11Ssid))
        }))
}
fn query_value<T: Copy>(
    handle: &WlanHandle,
    guid: &GUID,
    opcode: WLAN_INTF_OPCODE,
    what: &str,
) -> Result<Option<T>> {
    let mut size = 0;
    let mut ptr = std::ptr::null_mut();
    let code =
        unsafe { WlanQueryInterface(handle.0, guid, opcode, None, &mut size, &mut ptr, None) };
    if code != 0 {
        return if code == 1168 {
            Ok(None)
        } else {
            check(code, what).map(|_| None)
        };
    }
    let _guard = WlanMemory(ptr);
    if size < size_of::<T>() as u32 || ptr.is_null() {
        return Ok(None);
    }
    Ok(Some(unsafe { *(ptr as *const T) }))
}
fn native_security(value: WLAN_SECURITY_ATTRIBUTES) -> Security {
    let mut security = Security::unknown();
    security.privacy = value.bSecurityEnabled.as_bool();
    security.kind = match value.dot11AuthAlgorithm {
        x if x == DOT11_AUTH_ALGO_80211_OPEN => SecurityKind::Open,
        x if x == DOT11_AUTH_ALGO_80211_SHARED_KEY => SecurityKind::Wep,
        x if x == DOT11_AUTH_ALGO_WPA || x == DOT11_AUTH_ALGO_WPA_PSK => SecurityKind::WpaPersonal,
        x if x == DOT11_AUTH_ALGO_RSNA_PSK => SecurityKind::Wpa2Personal,
        x if x == DOT11_AUTH_ALGO_WPA3_SAE => SecurityKind::Wpa3Personal,
        x if x == DOT11_AUTH_ALGO_WPA3_ENT_192 => SecurityKind::Wpa3Enterprise,
        x if x == DOT11_AUTH_ALGO_WPA3_ENT => SecurityKind::Wpa3Enterprise,
        x if x == DOT11_AUTH_ALGO_OWE => SecurityKind::Owe,
        x if x == DOT11_AUTH_ALGO_RSNA => SecurityKind::Wpa2Enterprise,
        _ => SecurityKind::Unknown,
    };
    security
}
fn ssid_text(ssid: &DOT11_SSID) -> Option<String> {
    let raw = &ssid.ucSSID[..(ssid.uSSIDLength as usize).min(32)];
    (!raw.is_empty() && !raw.iter().all(|b| *b == 0))
        .then(|| String::from_utf8_lossy(raw).into_owned())
}
fn ipv4_for_guid(guid: &str) -> (Vec<String>, Option<String>) {
    let mut size = 15_000_u32;
    let mut bytes = vec![0_u8; size as usize];
    let code = unsafe {
        GetAdaptersAddresses(
            AF_INET.0 as u32,
            GAA_FLAG_INCLUDE_GATEWAYS,
            None,
            Some(bytes.as_mut_ptr().cast()),
            &mut size,
        )
    };
    if code != 0 {
        return (vec![], None);
    }
    let mut adapter = bytes.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while let Some(item) = unsafe { adapter.as_ref() } {
        // AdapterName is the GUID in braces; `guid` has none.
        let name = unsafe { std::ffi::CStr::from_ptr(item.AdapterName.0.cast()) }.to_string_lossy();
        if name
            .trim_matches(['{', '}'])
            .eq_ignore_ascii_case(guid.trim_matches(['{', '}']))
        {
            return ipv4_from_adapter(item);
        }
        adapter = item.Next;
    }
    (vec![], None)
}
fn ipv4_from_adapter(adapter: &IP_ADAPTER_ADDRESSES_LH) -> (Vec<String>, Option<String>) {
    let mut addresses = Vec::new();
    let mut node = adapter.FirstUnicastAddress;
    while let Some(value) = unsafe { node.as_ref() } {
        if let Some(ip) = sockaddr_ipv4(&value.Address) {
            addresses.push(format!("{ip}/{}", value.OnLinkPrefixLength));
        }
        node = value.Next;
    }
    let gateway = unsafe { adapter.FirstGatewayAddress.as_ref() }
        .and_then(|value| sockaddr_ipv4(&value.Address));
    (addresses, gateway)
}
fn sockaddr_ipv4(address: &windows::Win32::Networking::WinSock::SOCKET_ADDRESS) -> Option<String> {
    let value = unsafe { address.lpSockaddr.as_ref()? };
    if value.sa_family != AF_INET {
        return None;
    }
    let value = unsafe { &*(value as *const _ as *const SOCKADDR_IN) };
    let octets = unsafe { value.sin_addr.S_un.S_un_b };
    Some(format!(
        "{}.{}.{}.{}",
        octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4
    ))
}
struct WlanMemory(*const c_void);
impl Drop for WlanMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WlanFreeMemory(self.0) }
        }
    }
}
fn observation(
    id: &AdapterId,
    b: Bss,
    derived: bool,
    connected: Option<[u8; 6]>,
) -> AccessPointObservation {
    let frequency_mhz = frequency_khz_to_mhz(b.frequency_khz).unwrap_or_default();
    let elements = summarise(&b.ies);
    let span = elements.channel_span(frequency_mhz);
    let hidden = b.ssid.is_empty() || b.ssid.iter().all(|v| *v == 0);
    AccessPointObservation {
        timestamp: Utc::now(),
        adapter_id: id.clone(),
        bssid: mac(b.bssid),
        mld_address: elements.mld_address(),
        ssid: (!hidden).then(|| String::from_utf8_lossy(&b.ssid).into_owned()),
        ssid_raw: b.ssid,
        hidden,
        frequency_mhz,
        channel: channel_for_frequency(frequency_mhz),
        band: band_for_frequency(frequency_mhz),
        channel_width_mhz: span.width_mhz,
        channel_center_mhz: span.center_mhz.or_else(|| {
            channel_center_mhz(frequency_mhz, span.width_mhz, elements.ht_secondary_offset)
        }),
        signal: Signal {
            dbm: (!derived).then_some(b.rssi as f32),
            quality_percent: u8::try_from(b.quality).ok().filter(|q| *q <= 100),
        },
        security: elements
            .security(b.privacy)
            .unwrap_or_else(Security::unknown),
        mode: WifiMode::Infrastructure,
        max_bitrate_kbps: None,
        last_seen_age_ms: filetime_age_ms(b.host_time, SystemTime::now()),
        is_connected: connected == Some(b.bssid),
        noise_dbm: None,
        snr_db: None,
        channel_utilization_pct: elements.channel_utilization_pct(),
        station_count: elements.station_count,
        beacon_interval_tu: Some(b.beacon),
        phy_type: Some(phy_name(b.phy).into()),
        wifi_generation: elements
            .wifi_generation(band_for_frequency(frequency_mhz) == Band::Band2_4GHz),
    }
}
fn check(code: u32, what: &str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(map_error(code, what))
    }
}
fn map_error(code: u32, what: &str) -> WifiError {
    match code {
        c if c == ERROR_SERVICE_NOT_ACTIVE.0 => {
            WifiError::ServiceUnavailable(format!("{what}: WLAN AutoConfig is not running"))
                .with_hint("Start the WLAN AutoConfig service (WlanSvc).")
        }
        c if c == ERROR_ACCESS_DENIED.0 => {
            WifiError::PermissionDenied(format!("{what}: access denied")).with_hint(LOCATION_HINT)
        }
        ERROR_NDIS_DOT11_POWER_STATE_INVALID => {
            WifiError::RadioDisabled(format!("{what}: Wi-Fi radio is off"))
        }
        c if c == ERROR_INVALID_HANDLE.0 => {
            WifiError::Backend(format!("invalid WLAN handle while {what}"))
        }
        _ => WifiError::Backend(format!("{what} failed with Windows error {code}")),
    }
}
fn is_invalid_handle(e: &WifiError) -> bool {
    matches!(e.base(), WifiError::Backend(message) if message.starts_with("invalid WLAN handle"))
}
fn join_error(e: task::JoinError) -> WifiError {
    WifiError::Backend(format!("Native Wifi worker failed: {e}"))
}
fn utf16(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|v| *v == 0).unwrap_or(value.len())])
}
fn guid_string(guid: &GUID) -> String {
    format!("{guid:?}").to_ascii_lowercase()
}
fn mac(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|v| format!("{v:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}
fn dot11_ssid(ssid: &str) -> Option<DOT11_SSID> {
    let bytes = ssid.as_bytes();
    (bytes.len() <= 32).then(|| {
        let mut value = DOT11_SSID {
            uSSIDLength: bytes.len() as u32,
            ..Default::default()
        };
        value.ucSSID[..bytes.len()].copy_from_slice(bytes);
        value
    })
}
fn state(value: WLAN_INTERFACE_STATE) -> AdapterStatus {
    match value {
        x if x == wlan_interface_state_connected => AdapterStatus::Connected,
        x if x == wlan_interface_state_associating || x == wlan_interface_state_authenticating => {
            AdapterStatus::Connecting
        }
        x if x == wlan_interface_state_disconnecting => AdapterStatus::Disconnecting,
        x if x == wlan_interface_state_disconnected => AdapterStatus::Disconnected,
        x if x == wlan_interface_state_not_ready => AdapterStatus::Unavailable,
        _ => AdapterStatus::Unknown,
    }
}
fn phy_name(phy: DOT11_PHY_TYPE) -> &'static str {
    match phy {
        x if x == dot11_phy_type_eht => "802.11be",
        x if x == dot11_phy_type_he => "802.11ax",
        x if x == dot11_phy_type_vht => "802.11ac",
        x if x == dot11_phy_type_ht => "802.11n",
        x if x == dot11_phy_type_ofdm => "802.11a",
        _ => "802.11b/g",
    }
}
fn radio_off(handle: &WlanHandle, guid: &GUID) -> Result<bool> {
    let mut size = 0;
    let mut ptr = std::ptr::null_mut();
    check(
        unsafe {
            WlanQueryInterface(
                handle.0,
                guid,
                wlan_intf_opcode_radio_state,
                None,
                &mut size,
                &mut ptr,
                None,
            )
        },
        "reading Wi-Fi radio state",
    )?;
    let _guard = WlanMemory(ptr);
    if size < size_of::<WLAN_RADIO_STATE>() as u32 {
        return Ok(false);
    }
    let state = unsafe { &*(ptr as *const WLAN_RADIO_STATE) };
    Ok(
        state.PhyRadioState[..(state.dwNumberOfPhys as usize).min(64)]
            .iter()
            .any(|s| {
                s.dot11SoftwareRadioState == dot11_radio_state_off
                    || s.dot11HardwareRadioState == dot11_radio_state_off
            }),
    )
}
