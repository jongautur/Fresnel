//! Translation of NetworkManager enums/flags/errors into the normalised model.
//! Constant values: https://networkmanager.dev/docs/api/latest/nm-dbus-types.html

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use crate::error::WifiError;
use crate::wifi::models::{
    AdapterCapabilities, AdapterStatus, Akm, Capability, Cipher, Security, SecurityKind, WifiMode,
};
use crate::wifi::security::classify;

pub type Props = HashMap<String, OwnedValue>;

/// Typed lookup in a `GetAll` result. Missing or mistyped → `None` (older NM
/// versions lack some properties, e.g. `Bandwidth` before 1.46).
pub fn prop<T>(props: &Props, key: &str) -> Option<T>
where
    T: TryFrom<OwnedValue>,
{
    let v = props.get(key)?.try_clone().ok()?;
    T::try_from(v).ok()
}

pub fn prop_bytes(props: &Props, key: &str) -> Option<Vec<u8>> {
    match &**props.get(key)? {
        Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|v| if let Value::U8(b) = v { Some(*b) } else { None })
                .collect(),
        ),
        _ => None,
    }
}

/// Non-root object path ("/" means "none" in NM).
pub fn prop_path(props: &Props, key: &str) -> Option<String> {
    match &**props.get(key)? {
        Value::ObjectPath(p) if p.as_str() != "/" => Some(p.to_string()),
        _ => None,
    }
}

// --- NMDeviceType ----------------------------------------------------------
pub const NM_DEVICE_TYPE_WIFI: u32 = 2;

// --- NMDeviceState ---------------------------------------------------------
pub const NM_DEVICE_STATE_ACTIVATED: u32 = 100;

pub fn device_status(state: u32) -> AdapterStatus {
    match state {
        10 => AdapterStatus::Unmanaged,
        20 => AdapterStatus::Unavailable,
        30 => AdapterStatus::Disconnected,
        40..=90 => AdapterStatus::Connecting,
        100 => AdapterStatus::Connected,
        110 => AdapterStatus::Disconnecting,
        120 => AdapterStatus::Failed,
        _ => AdapterStatus::Unknown,
    }
}

// --- NMDeviceWifiCapabilities ----------------------------------------------
const WIFI_CAP_AP: u32 = 0x40;
const WIFI_CAP_FREQ_VALID: u32 = 0x100;
const WIFI_CAP_FREQ_2GHZ: u32 = 0x200;
const WIFI_CAP_FREQ_5GHZ: u32 = 0x400;

pub fn capabilities(wifi_caps: Option<u32>) -> AdapterCapabilities {
    let caps = wifi_caps.unwrap_or(0);
    let band = |bit: u32| {
        if caps & WIFI_CAP_FREQ_VALID != 0 {
            Capability::from_bool(caps & bit != 0)
        } else {
            Capability::Unknown
        }
    };
    AdapterCapabilities {
        band_2ghz: band(WIFI_CAP_FREQ_2GHZ),
        band_5ghz: band(WIFI_CAP_FREQ_5GHZ),
        // NM has no 6 GHz capability flag; needs nl80211 (wiphy band list).
        band_6ghz: Capability::Unknown,
        // NM always issues active scans through wpa_supplicant.
        active_scan: Capability::Supported,
        passive_scan: Capability::Unknown,
        // Not discoverable through NM; NM cannot drive monitor mode anyway.
        monitor_mode: Capability::Unknown,
        packet_capture: Capability::Unknown,
        ap_mode: if wifi_caps.is_some() {
            Capability::from_bool(caps & WIFI_CAP_AP != 0)
        } else {
            Capability::Unknown
        },
        signal_dbm: Capability::Unsupported,
        signal_quality: Capability::Supported,
    }
}

// --- NM80211Mode ------------------------------------------------------------
pub fn wifi_mode(mode: u32) -> WifiMode {
    match mode {
        1 => WifiMode::AdHoc,
        2 => WifiMode::Infrastructure,
        3 => WifiMode::AccessPoint,
        4 => WifiMode::Mesh,
        _ => WifiMode::Unknown,
    }
}

// --- NM80211ApFlags / NM80211ApSecurityFlags --------------------------------
const AP_FLAGS_PRIVACY: u32 = 0x1;

const SEC_PAIR_WEP40: u32 = 0x1;
const SEC_PAIR_WEP104: u32 = 0x2;
const SEC_PAIR_TKIP: u32 = 0x4;
const SEC_PAIR_CCMP: u32 = 0x8;
const SEC_GROUP_WEP40: u32 = 0x10;
const SEC_GROUP_WEP104: u32 = 0x20;
const SEC_GROUP_TKIP: u32 = 0x40;
const SEC_GROUP_CCMP: u32 = 0x80;
const SEC_KEY_MGMT_PSK: u32 = 0x100;
const SEC_KEY_MGMT_802_1X: u32 = 0x200;
const SEC_KEY_MGMT_SAE: u32 = 0x400;
const SEC_KEY_MGMT_OWE: u32 = 0x800;
const SEC_KEY_MGMT_OWE_TM: u32 = 0x1000;
const SEC_KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;

pub fn security(flags: u32, wpa_flags: u32, rsn_flags: u32) -> Security {
    let all = wpa_flags | rsn_flags;
    let privacy = flags & AP_FLAGS_PRIVACY != 0;
    let wpa = wpa_flags != 0;
    let rsn = rsn_flags != 0;

    let mut akms = Vec::new();
    for (bit, akm) in [
        (SEC_KEY_MGMT_PSK, Akm::Psk),
        (SEC_KEY_MGMT_SAE, Akm::Sae),
        (SEC_KEY_MGMT_802_1X, Akm::Ieee8021x),
        (SEC_KEY_MGMT_OWE, Akm::Owe),
        (SEC_KEY_MGMT_OWE_TM, Akm::OweTransition),
        (SEC_KEY_MGMT_EAP_SUITE_B_192, Akm::SuiteB192),
    ] {
        if all & bit != 0 {
            akms.push(akm);
        }
    }
    let ciphers = |pairs: [(u32, Cipher); 4]| {
        pairs
            .into_iter()
            .filter(|(bit, _)| all & bit != 0)
            .map(|(_, c)| c)
            .collect::<Vec<_>>()
    };
    let pairwise_ciphers = ciphers([
        (SEC_PAIR_WEP40, Cipher::Wep40),
        (SEC_PAIR_WEP104, Cipher::Wep104),
        (SEC_PAIR_TKIP, Cipher::Tkip),
        (SEC_PAIR_CCMP, Cipher::Ccmp),
    ]);
    let group_ciphers = ciphers([
        (SEC_GROUP_WEP40, Cipher::Wep40),
        (SEC_GROUP_WEP104, Cipher::Wep104),
        (SEC_GROUP_TKIP, Cipher::Tkip),
        (SEC_GROUP_CCMP, Cipher::Ccmp),
    ]);

    let mut s = Security {
        kind: SecurityKind::Unknown,
        privacy,
        wpa,
        rsn,
        akms,
        pairwise_ciphers,
        group_ciphers,
        // NM's flags carry neither.
        group_mgmt_cipher: None,
        pmf: None,
    };
    s.kind = classify(&s);
    s
}

/// Uppercase, colon-separated MAC.
pub fn normalise_mac(mac: &str) -> String {
    mac.trim().to_ascii_uppercase()
}

// --- Errors -----------------------------------------------------------------

pub fn nm_not_running() -> WifiError {
    WifiError::ServiceUnavailable(
        "NetworkManager is not running (no owner for \
         org.freedesktop.NetworkManager on the system bus)."
            .into(),
    )
    .with_hint("Start it with `sudo systemctl start NetworkManager`.")
}

/// For errors reaching the system D-Bus at all.
pub const DBUS_HINT: &str = "Fresnel reads Wi-Fi data from NetworkManager over the \
    system D-Bus. Check that the dbus and NetworkManager services are running.";

const POLKIT_HINT: &str = "NetworkManager's polkit policy did not allow this for your \
    user. Make sure you are in an active local session (not remote or inactive).";

/// Map a D-Bus error name/message to a [`WifiError`].
pub fn classify_dbus_error(name: &str, message: &str, context: &str) -> WifiError {
    let detail = if message.is_empty() {
        name.to_string()
    } else {
        message.to_string()
    };
    if name.ends_with(".ServiceUnknown") || name.ends_with(".NameHasNoOwner") {
        nm_not_running()
    } else if name.ends_with(".PermissionDenied") || name.ends_with(".AccessDenied") {
        WifiError::PermissionDenied(format!("{context}: {detail}")).with_hint(POLKIT_HINT)
    } else if name == "org.freedesktop.NetworkManager.Device.NotAllowed" {
        WifiError::ScanRejected(detail)
    } else if name.ends_with(".UnknownObject")
        || name.ends_with(".UnknownMethod")
        || name.ends_with(".UnknownInterface")
    {
        WifiError::Backend(format!("{context}: object disappeared ({detail})"))
    } else if name.ends_with(".NoReply") || name.ends_with(".Timeout") {
        WifiError::Timeout(format!("{context}: NetworkManager did not reply"))
    } else {
        WifiError::Backend(format!("{context}: {detail}"))
    }
}

pub fn map_zbus_error(err: zbus::Error, context: &str) -> WifiError {
    match err {
        zbus::Error::MethodError(name, msg, _) => {
            classify_dbus_error(name.as_str(), msg.as_deref().unwrap_or(""), context)
        }
        zbus::Error::FDO(fdo) => map_fdo_error(*fdo, context),
        // The connection's method timeout fired.
        zbus::Error::InputOutput(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            WifiError::Timeout(format!(
                "{context}: no reply on the system D-Bus within {} s \
                 (NetworkManager or polkit may be stuck)",
                super::DBUS_METHOD_TIMEOUT.as_secs()
            ))
        }
        zbus::Error::InputOutput(e) => {
            WifiError::ServiceUnavailable(format!("Cannot reach the system D-Bus: {e}"))
                .with_hint(DBUS_HINT)
        }
        zbus::Error::Address(e) => {
            WifiError::ServiceUnavailable(format!("System D-Bus address is invalid: {e}"))
                .with_hint(DBUS_HINT)
        }
        other => WifiError::Backend(format!("{context}: {other}")),
    }
}

pub fn map_fdo_error(err: zbus::fdo::Error, context: &str) -> WifiError {
    use zbus::DBusError;
    match err {
        zbus::fdo::Error::ZBus(e) => map_zbus_error(e, context),
        other => classify_dbus_error(
            other.name().as_str(),
            other.description().unwrap_or(""),
            context,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_timeout_is_a_timeout() {
        let io = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let e = map_zbus_error(zbus::Error::from(io), "GetDevices");
        assert_eq!(e.kind(), "timeout");
        assert!(e.to_string().contains("GetDevices"));

        let io = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let e = map_zbus_error(zbus::Error::from(io), "GetDevices");
        assert_eq!(e.kind(), "service_unavailable");
    }

    #[test]
    fn wpa2_psk_ccmp() {
        // Real value observed on an NM 1.54 system: RsnFlags = 392.
        let s = security(0x3, 0, 392);
        assert_eq!(s.kind, SecurityKind::Wpa2Personal);
        assert_eq!(s.akms, vec![Akm::Psk]);
        assert_eq!(s.pairwise_ciphers, vec![Cipher::Ccmp]);
        assert_eq!(s.group_ciphers, vec![Cipher::Ccmp]);
        assert!(s.rsn && !s.wpa && s.privacy);
    }

    #[test]
    fn classification() {
        assert_eq!(security(0, 0, 0).kind, SecurityKind::Open);
        assert_eq!(security(1, 0, 0).kind, SecurityKind::Wep);
        assert_eq!(
            security(1, 0, 0x88 | 0x400).kind,
            SecurityKind::Wpa3Personal
        );
        assert_eq!(
            security(1, 0, 0x88 | 0x500).kind,
            SecurityKind::Wpa2Wpa3Personal
        );
        assert_eq!(
            security(1, 0, 0x88 | 0x200).kind,
            SecurityKind::Wpa2Enterprise
        );
        assert_eq!(security(1, 0x144, 0).kind, SecurityKind::WpaPersonal);
        assert_eq!(security(1, 0, 0x2000).kind, SecurityKind::Wpa3Enterprise);
        assert_eq!(security(0, 0, 0x800).kind, SecurityKind::Owe);
    }

    #[test]
    fn caps() {
        let c = capabilities(Some(0x27ff));
        assert_eq!(c.band_2ghz, Capability::Supported);
        assert_eq!(c.band_5ghz, Capability::Supported);
        assert_eq!(c.band_6ghz, Capability::Unknown);
        let c = capabilities(Some(0x0ff));
        assert_eq!(c.band_2ghz, Capability::Unknown);
        let c = capabilities(Some(0x100 | 0x200));
        assert_eq!(c.band_5ghz, Capability::Unsupported);
    }

    #[test]
    fn errors() {
        assert_eq!(
            classify_dbus_error("org.freedesktop.DBus.Error.ServiceUnknown", "", "x").kind(),
            "service_unavailable"
        );
        assert_eq!(
            classify_dbus_error("org.freedesktop.NetworkManager.PermissionDenied", "no", "x")
                .kind(),
            "permission_denied"
        );
        assert!(
            classify_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "no", "x")
                .hint()
                .is_some_and(|h| h.contains("polkit"))
        );
        assert_eq!(
            classify_dbus_error(
                "org.freedesktop.NetworkManager.Device.NotAllowed",
                "busy",
                "x"
            )
            .kind(),
            "scan_rejected"
        );
    }
}
