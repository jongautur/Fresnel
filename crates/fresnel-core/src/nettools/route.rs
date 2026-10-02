//! "Would this test's traffic leave through the Wi-Fi interface?"
//!
//! Each OS answers "which interface does the route to the target use" its
//! own way (Linux: the main route table in `/proc`; Windows:
//! `GetBestInterfaceEx`); the decision itself is [`require_wifi_route`],
//! shared and unit-tested on any OS. Binding the sockets to the interface
//! is what forces the traffic onto Wi-Fi; this check refuses the cases
//! where that binding would hide a misconfiguration (Ethernet plugged in,
//! a full-tunnel VPN) and the result would not describe what users get.

use std::net::IpAddr;

use super::binding::WifiBinding;
use super::OS_CALL_TIMEOUT;
use crate::{Result, WifiError};

/// Which interface the OS would route the target through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteVia {
    Wifi,
    /// Another interface, by the name the user knows it by.
    Other(String),
    NoRoute,
}

/// Refuse a test whose traffic would not use the Wi-Fi interface.
pub fn require_wifi_route(target: IpAddr, via: RouteVia, wifi: &str) -> Result<()> {
    match via {
        RouteVia::Wifi => Ok(()),
        RouteVia::Other(other) => Err(WifiError::AdapterUnavailable {
            id: wifi.into(),
            reason: format!(
                "traffic to {target} would leave through {other}, not the Wi-Fi interface; \
                 the test was not run"
            ),
        }
        .with_hint(
            "Unplug Ethernet or disconnect the VPN while surveying, or test a target that is \
             reached through the Wi-Fi network.",
        )),
        RouteVia::NoRoute => Err(WifiError::AdapterUnavailable {
            id: wifi.into(),
            reason: format!("there is no route to {target}"),
        }
        .with_hint("Check that the Wi-Fi network has an IP address and reaches the target.")),
    }
}

/// One route table entry, as an OS-specific reader reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub network: IpAddr,
    pub prefix: u8,
    pub metric: u32,
    pub iface: String,
}

/// The route the kernel would pick from one table: longest prefix, then
/// lowest metric. (Policy rules, e.g. WireGuard's fwmark tables, aren't
/// in the main table; the socket binding still keeps those tests on Wi-Fi.)
pub fn best_route(routes: &[Route], target: IpAddr) -> Option<&Route> {
    routes
        .iter()
        .filter(|r| contains(r.network, r.prefix, target))
        .min_by_key(|r| (std::cmp::Reverse(r.prefix), r.metric))
}

/// Interface indices and the names users know them by.
pub type InterfaceNames = Vec<(u32, String)>;

/// The pure part of the Windows check: the best interface index for the
/// target against the Wi-Fi interface's index. `names` maps indices to
/// friendly names ("Ethernet", "NordLynx") for the error.
pub fn via_for_index(best: Option<u32>, wifi_index: u32, names: &[(u32, String)]) -> RouteVia {
    match best {
        None => RouteVia::NoRoute,
        Some(i) if i == wifi_index => RouteVia::Wifi,
        Some(i) => RouteVia::Other(
            names
                .iter()
                .find(|(n, _)| *n == i)
                .map(|(_, name)| format!("“{name}”"))
                .unwrap_or_else(|| format!("interface {i}")),
        ),
    }
}

fn contains(network: IpAddr, prefix: u8, target: IpAddr) -> bool {
    match (network, target) {
        (IpAddr::V4(a), IpAddr::V4(b)) if prefix <= 32 => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) if prefix <= 128 => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

/// Check the route to `target` for the bound Wi-Fi interface, with a
/// deadline on the OS calls.
pub async fn check_route(binding: &WifiBinding, target: IpAddr) -> Result<()> {
    let wifi = binding.iface.clone();
    #[cfg(target_os = "linux")]
    {
        let routes =
            super::blocking("reading the route table", OS_CALL_TIMEOUT, linux_routes).await?;
        let via = match best_route(&routes, target) {
            None => RouteVia::NoRoute,
            Some(r) if r.iface == wifi => RouteVia::Wifi,
            Some(r) => RouteVia::Other(r.iface.clone()),
        };
        require_wifi_route(target, via, &wifi)
    }
    #[cfg(windows)]
    {
        let index = match target {
            IpAddr::V4(_) => binding.if_index_v4,
            IpAddr::V6(_) => binding.if_index_v6,
        }
        .ok_or_else(|| WifiError::AdapterUnavailable {
            id: wifi.clone(),
            reason: format!(
                "the Wi-Fi interface has no {} index",
                if target.is_ipv4() { "IPv4" } else { "IPv6" }
            ),
        })?;
        let (best, names) = super::blocking("finding the route", OS_CALL_TIMEOUT, move || {
            super::windows::best_interface(target)
        })
        .await?;
        require_wifi_route(target, via_for_index(best, index, &names), &wifi)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (OS_CALL_TIMEOUT, target);
        Err(WifiError::Unsupported(format!(
            "route checks for {wifi} are not implemented on this OS"
        )))
    }
}

/// The main IPv4 and IPv6 route tables from `/proc` (kernel data, no
/// external command). Rows that don't parse are skipped, so a table being
/// rewritten can't cause a panic; a missing route is refused by the caller.
#[cfg(target_os = "linux")]
pub fn linux_routes() -> Result<Vec<Route>> {
    let ipv4 = std::fs::read_to_string("/proc/net/route")
        .map_err(|e| WifiError::Backend(format!("cannot read /proc/net/route: {e}")))?;
    let mut routes = parse_proc_route(&ipv4);
    // No IPv6 at all is normal (ipv6.disable=1).
    if let Ok(ipv6) = std::fs::read_to_string("/proc/net/ipv6_route") {
        routes.extend(parse_proc_ipv6_route(&ipv6));
    }
    Ok(routes)
}

const RTF_UP: u32 = 0x0001;
const RTF_REJECT: u32 = 0x0200;

/// `/proc/net/route`: Iface Destination Gateway Flags RefCnt Use Metric Mask
/// …, in hex.
pub fn parse_proc_route(text: &str) -> Vec<Route> {
    text.lines()
        .skip(1)
        .filter_map(|row| {
            let f: Vec<_> = row.split_whitespace().collect();
            if f.len() < 8 {
                return None;
            }
            let dest = u32::from_str_radix(f[1], 16).ok()?;
            let flags = u32::from_str_radix(f[3], 16).ok()?;
            let metric = f[6].parse().ok()?;
            let mask = u32::from_str_radix(f[7], 16).ok()?;
            if flags & RTF_UP == 0 || flags & RTF_REJECT != 0 {
                return None;
            }
            // Printed as host-order integers of network-order addresses.
            let mask = u32::from_be_bytes(mask.to_ne_bytes());
            Some(Route {
                network: IpAddr::from(dest.to_ne_bytes()),
                prefix: mask.leading_ones() as u8,
                metric,
                iface: f[0].into(),
            })
        })
        .collect()
}

/// `/proc/net/ipv6_route`: dest prefix src src_prefix next_hop metric
/// refcnt use flags iface, hex without separators.
pub fn parse_proc_ipv6_route(text: &str) -> Vec<Route> {
    text.lines()
        .filter_map(|row| {
            let f: Vec<_> = row.split_whitespace().collect();
            if f.len() < 10 {
                return None;
            }
            let dest = u128::from_str_radix(f[0], 16)
                .ok()
                .filter(|_| f[0].len() == 32)?;
            let prefix = u8::from_str_radix(f[1], 16).ok().filter(|p| *p <= 128)?;
            let metric = u32::from_str_radix(f[5], 16).ok()?;
            let flags = u32::from_str_radix(f[8], 16).ok()?;
            if flags & RTF_UP == 0 || flags & RTF_REJECT != 0 || f[9] == "lo" {
                return None;
            }
            Some(Route {
                network: IpAddr::from(dest.to_be_bytes()),
                prefix,
                metric,
                iface: f[9].into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(net: &str, prefix: u8, metric: u32, iface: &str) -> Route {
        Route {
            network: net.parse().unwrap(),
            prefix,
            metric,
            iface: iface.into(),
        }
    }

    fn via(routes: &[Route], target: &str, wifi: &str) -> Result<()> {
        let target: IpAddr = target.parse().unwrap();
        let via = match best_route(routes, target) {
            None => RouteVia::NoRoute,
            Some(r) if r.iface == wifi => RouteVia::Wifi,
            Some(r) => RouteVia::Other(r.iface.clone()),
        };
        require_wifi_route(target, via, wifi)
    }

    #[test]
    fn longest_prefix_then_lowest_metric() {
        let routes = vec![
            route("0.0.0.0", 0, 600, "wlan0"),
            route("0.0.0.0", 0, 100, "eth0"),
            route("192.168.1.0", 24, 600, "wlan0"),
            route("10.8.0.0", 16, 50, "tun0"),
        ];
        // The Wi-Fi subnet itself (the gateway) is fine with Ethernet plugged in.
        assert!(via(&routes, "192.168.1.1", "wlan0").is_ok());
        // The internet would go through Ethernet: refused, with advice.
        let e = via(&routes, "8.8.8.8", "wlan0").unwrap_err();
        assert!(e.to_string().contains("eth0"), "{e}");
        assert!(e.hint().unwrap().contains("Ethernet"));
        // A VPN subnet.
        assert!(via(&routes, "10.8.3.4", "wlan0")
            .unwrap_err()
            .to_string()
            .contains("tun0"));
        // Metric order doesn't depend on the table order.
        let mut reversed = routes.clone();
        reversed.reverse();
        assert!(via(&reversed, "8.8.8.8", "wlan0").is_err());
        assert!(via(&routes[..1], "8.8.8.8", "wlan0").is_ok());
    }

    #[test]
    fn no_route_and_families() {
        let routes = vec![route("192.168.1.0", 24, 0, "wlan0")];
        let e = via(&routes, "10.0.0.1", "wlan0").unwrap_err();
        assert!(e.to_string().contains("no route"), "{e}");
        // An IPv4 default route never matches an IPv6 target.
        let routes = vec![route("0.0.0.0", 0, 0, "wlan0")];
        assert!(via(&routes, "2001:db8::1", "wlan0").is_err());
        let routes = vec![
            route("::", 0, 1024, "wlan0"),
            route("2001:db8:1::", 48, 256, "eth0"),
        ];
        assert!(via(&routes, "2001:db8:2::1", "wlan0").is_ok());
        assert!(via(&routes, "2001:db8:1::1", "wlan0").is_err());
    }

    #[test]
    fn windows_index_decision() {
        let names = vec![(4, "Ethernet".to_string()), (12, "Wi-Fi".to_string())];
        assert_eq!(via_for_index(Some(12), 12, &names), RouteVia::Wifi);
        assert_eq!(
            via_for_index(Some(4), 12, &names),
            RouteVia::Other("“Ethernet”".into())
        );
        assert_eq!(
            via_for_index(Some(30), 12, &names),
            RouteVia::Other("interface 30".into())
        );
        assert_eq!(via_for_index(None, 12, &names), RouteVia::NoRoute);
    }

    #[test]
    fn parses_proc_tables_and_skips_junk() {
        let v4 =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                  wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                  wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
                  eth0\t0000000A\t00000000\t0000\t0\t0\t0\t000000FF\t0\t0\t0\n\
                  garbage line\n\
                  eth1\tZZZZ\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0\n";
        let routes = parse_proc_route(v4);
        assert_eq!(
            routes,
            vec![
                route("0.0.0.0", 0, 600, "wlan0"),
                route("192.168.1.0", 24, 600, "wlan0"),
            ]
        );
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 wlan0\n\
                  20010db8000000000000000000000000 20 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n\
                  short row\n";
        assert_eq!(
            parse_proc_ipv6_route(v6),
            vec![
                route("::", 0, 1024, "wlan0"),
                route("2001:db8::", 32, 256, "eth0"),
            ]
        );
    }
}
