//! Linux sysfs helpers shared by Linux providers: hardware identification
//! and rfkill state for a network interface.
//!
//! Product names come from the PCI/USB ID databases shipped by the `hwdata`
//! / `pciutils` packages. These are static data files, not tool output.

use std::fs;
use std::path::{Path, PathBuf};

use crate::wifi::models::{BusInfo, BusKind};

const PCI_IDS: &[&str] = &["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"];
const USB_IDS: &[&str] = &["/usr/share/hwdata/usb.ids", "/usr/share/misc/usb.ids"];

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn read_hex_id(path: impl AsRef<Path>) -> Option<String> {
    read_trimmed(path).map(|s| s.trim_start_matches("0x").to_ascii_lowercase())
}

/// Identify the hardware behind `interface`.
pub fn bus_info(interface: &str) -> Option<BusInfo> {
    let dev = PathBuf::from(format!("/sys/class/net/{interface}/device"));
    let subsystem = fs::read_link(dev.join("subsystem")).ok()?;
    let subsystem = subsystem.file_name()?.to_string_lossy().into_owned();

    match subsystem.as_str() {
        "pci" => {
            let vendor_id = read_hex_id(dev.join("vendor"));
            let product_id = read_hex_id(dev.join("device"));
            let sub_vendor = read_hex_id(dev.join("subsystem_vendor"));
            let sub_device = read_hex_id(dev.join("subsystem_device"));
            let (vendor_name, product_name) = match (&vendor_id, &product_id) {
                (Some(v), Some(p)) => lookup_ids(
                    PCI_IDS,
                    v,
                    p,
                    sub_vendor.as_deref().zip(sub_device.as_deref()),
                ),
                _ => (None, None),
            };
            Some(BusInfo {
                kind: BusKind::Pci,
                vendor_id,
                product_id,
                vendor_name,
                product_name,
            })
        }
        "usb" => {
            // `device` is the USB interface (e.g. 1-2:1.0); IDs live on its parent.
            let usb_dev = fs::canonicalize(&dev).ok()?.parent()?.to_path_buf();
            let vendor_id = read_hex_id(usb_dev.join("idVendor"));
            let product_id = read_hex_id(usb_dev.join("idProduct"));
            let (db_vendor, db_product) = match (&vendor_id, &product_id) {
                (Some(v), Some(p)) => lookup_ids(USB_IDS, v, p, None),
                _ => (None, None),
            };
            Some(BusInfo {
                kind: BusKind::Usb,
                vendor_name: read_trimmed(usb_dev.join("manufacturer")).or(db_vendor),
                product_name: read_trimmed(usb_dev.join("product")).or(db_product),
                vendor_id,
                product_id,
            })
        }
        "sdio" => Some(BusInfo {
            kind: BusKind::Sdio,
            vendor_id: read_hex_id(dev.join("vendor")),
            product_id: read_hex_id(dev.join("device")),
            vendor_name: None,
            product_name: None,
        }),
        _ => Some(BusInfo {
            kind: BusKind::Other,
            vendor_id: None,
            product_id: None,
            vendor_name: None,
            product_name: None,
        }),
    }
}

/// Look up vendor and product names in a pci.ids/usb.ids-format file.
/// A subsystem match, when present, is preferred because it names the
/// actual retail card (e.g. "Wi-Fi 6 AX201") rather than the chipset.
fn lookup_ids(
    files: &[&str],
    vendor: &str,
    product: &str,
    subsystem: Option<(&str, &str)>,
) -> (Option<String>, Option<String>) {
    let Some(content) = files.iter().find_map(|f| fs::read_to_string(f).ok()) else {
        return (None, None);
    };
    parse_ids(&content, vendor, product, subsystem)
}

fn parse_ids(
    content: &str,
    vendor: &str,
    product: &str,
    subsystem: Option<(&str, &str)>,
) -> (Option<String>, Option<String>) {
    let mut vendor_name = None;
    let mut product_name = None;
    let mut in_vendor = false;
    let mut in_product = false;

    for line in content.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("\t\t") {
            if in_product {
                if let Some((sv, sd)) = subsystem {
                    let mut parts = rest.splitn(3, ' ');
                    if parts.next() == Some(sv) && parts.next() == Some(sd) {
                        if let Some(name) = parts.next() {
                            return (vendor_name, Some(name.trim().to_string()));
                        }
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix('\t') {
            if in_vendor {
                in_product = false;
                if let Some((id, name)) = rest.split_once("  ") {
                    if id.eq_ignore_ascii_case(product) {
                        product_name = Some(name.trim().to_string());
                        in_product = true;
                    }
                }
            }
        } else {
            if in_vendor {
                break; // past our vendor block
            }
            // Top-level lines are vendors, except for trailing class/lang sections
            // whose lines start with a letter followed by a space ("C 00 ...").
            if let Some((id, name)) = line.split_once("  ") {
                if id.eq_ignore_ascii_case(vendor) {
                    vendor_name = Some(name.trim().to_string());
                    in_vendor = true;
                }
            }
        }
    }
    (vendor_name, product_name)
}

/// Shorten "Intel Corporation" → "Intel", "Realtek Semiconductor Co., Ltd." → "Realtek".
pub fn short_vendor(name: &str) -> String {
    const SUFFIXES: &[&str] = &[
        " Semiconductor Co., Ltd.",
        " Technology Corp.",
        " Technology, Inc.",
        " Communications Inc.",
        " Corporation",
        " Co., Ltd.",
        " Corp.",
        ", Inc.",
        " Inc.",
        " Ltd.",
    ];
    let mut s = name.trim().to_string();
    for suffix in SUFFIXES {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_string();
            break;
        }
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfkillState {
    pub soft_blocked: bool,
    pub hard_blocked: bool,
}

/// rfkill state of the wiphy behind `interface`, if exposed.
pub fn rfkill_state(interface: &str) -> Option<RfkillState> {
    let phy = PathBuf::from(format!("/sys/class/net/{interface}/phy80211"));
    let entry = fs::read_dir(&phy)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("rfkill"))?;
    let read_flag = |name: &str| read_trimmed(entry.path().join(name)).map(|v| v != "0");
    Some(RfkillState {
        soft_blocked: read_flag("soft").unwrap_or(false),
        hard_blocked: read_flag("hard").unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# comment
8086  Intel Corporation
\t2725  Wi-Fi 6E(802.11ax) AX210/AX1675* 2x2 [Typhoon Peak]
\t\t8086 0024  Wi-Fi 6E AX210 160MHz
\t9df0  Cannon Point-LP CNVi [Wireless-AC]
\t\t8086 0034  Wireless-AC 9560 160MHz
10ec  Realtek Semiconductor Co., Ltd.
\tc822  RTL8822CE 802.11ac PCIe Wireless Network Adapter
";

    #[test]
    fn subsystem_preferred() {
        let (v, p) = parse_ids(SAMPLE, "8086", "2725", Some(("8086", "0024")));
        assert_eq!(v.as_deref(), Some("Intel Corporation"));
        assert_eq!(p.as_deref(), Some("Wi-Fi 6E AX210 160MHz"));
    }

    #[test]
    fn falls_back_to_chipset() {
        let (_, p) = parse_ids(SAMPLE, "8086", "9df0", Some(("8086", "0030")));
        assert_eq!(p.as_deref(), Some("Cannon Point-LP CNVi [Wireless-AC]"));
    }

    #[test]
    fn other_vendor() {
        let (v, p) = parse_ids(SAMPLE, "10ec", "c822", None);
        assert_eq!(v.as_deref(), Some("Realtek Semiconductor Co., Ltd."));
        assert!(p.unwrap().starts_with("RTL8822CE"));
        assert_eq!(short_vendor("Realtek Semiconductor Co., Ltd."), "Realtek");
        assert_eq!(short_vendor("Intel Corporation"), "Intel");
    }

    #[test]
    fn unknown_ids() {
        assert_eq!(parse_ids(SAMPLE, "dead", "beef", None), (None, None));
    }
}
