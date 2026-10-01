//! Minimal zbus proxies for the NetworkManager D-Bus API (only members we use).
//! Reference: https://networkmanager.dev/docs/api/latest/spec.html
//!
//! Bulk property reads go through `org.freedesktop.DBus.Properties.GetAll`
//! (see `mod.rs`), so these proxies are mainly for methods and change streams.

use std::collections::HashMap;

use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, Value};

pub const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
pub const NM_PATH: &str = "/org/freedesktop/NetworkManager";
pub const IFACE_DEVICE: &str = "org.freedesktop.NetworkManager.Device";
pub const IFACE_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
pub const IFACE_ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
pub const IFACE_IP4_CONFIG: &str = "org.freedesktop.NetworkManager.IP4Config";

#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
pub trait NetworkManager {
    /// Realized devices only (not placeholder/unrealized ones).
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;

    /// Software Wi-Fi switch (`nmcli radio wifi`).
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    /// Hardware rfkill state as seen by NM.
    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
pub trait Wireless {
    /// Options: `ssids` (aay) for directed probes. Requires the
    /// `org.freedesktop.NetworkManager.wifi.scan` polkit action.
    fn request_scan(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    /// Includes APs with hidden SSIDs (unlike `GetAccessPoints`).
    fn get_all_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// CLOCK_BOOTTIME milliseconds of the last completed scan; -1 if never.
    #[zbus(property)]
    fn last_scan(&self) -> zbus::Result<i64>;
}
