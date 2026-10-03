//! What a Tools run talks to: the name the user typed, resolved with the
//! system resolver (`getaddrinfo`: hosts file, mDNS, a VPN's split DNS), and
//! the interface the traffic leaves through.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Cancel, TestError, TestResult, OS_CALL_TIMEOUT};
use crate::{Result, WifiError};

/// Resolving one name; `getaddrinfo` itself has no timeout.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest hostname (RFC 1035, without the trailing dot).
const MAX_NAME_LEN: usize = 253;

/// Which address family to use when a name has both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpFamily {
    /// The first address the system resolver returns (its RFC 6724 order).
    #[default]
    Any,
    V4,
    V6,
}

impl IpFamily {
    fn accepts(self, ip: IpAddr) -> bool {
        match self {
            Self::Any => true,
            Self::V4 => ip.is_ipv4(),
            Self::V6 => ip.is_ipv6(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Any => "IP",
            Self::V4 => "IPv4",
            Self::V6 => "IPv6",
        }
    }
}

/// A target the user typed and the address used for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTarget {
    /// As typed (trimmed).
    pub input: String,
    pub ip: IpAddr,
    /// Every address the resolver returned, in its order (just `ip` for a
    /// literal address).
    pub addresses: Vec<IpAddr>,
    /// Time the system resolver took; `None` for a literal address.
    pub resolve_ms: Option<f64>,
}

/// Check what the user typed: a literal address (IPv6 optionally in
/// brackets) or something that can be a hostname. Returns the text to
/// resolve.
pub fn parse_host(input: &str) -> Result<String> {
    let host = input.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err(WifiError::InvalidInput(
            "enter a host name or IP address".into(),
        ));
    }
    if host.parse::<IpAddr>().is_ok() {
        return Ok(host.to_owned());
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    let valid = name.len() <= MAX_NAME_LEN
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    if !valid {
        return Err(WifiError::InvalidInput(format!(
            "“{host}” is not a host name or IP address"
        )));
    }
    Ok(name.to_owned())
}

/// Pick the address to use from the resolver's list.
pub fn choose(addresses: &[IpAddr], family: IpFamily) -> Option<IpAddr> {
    addresses.iter().copied().find(|ip| family.accepts(*ip))
}

/// Resolve `input` with the system resolver. A literal address is used as
/// is (if it's of the requested family).
pub async fn resolve(input: &str, family: IpFamily, cancel: &Cancel) -> TestResult<ResolvedTarget> {
    let host = parse_host(input)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !family.accepts(ip) {
            return Err(WifiError::InvalidInput(format!(
                "{ip} is not an {} address",
                family.label()
            ))
            .into());
        }
        return Ok(ResolvedTarget {
            input: input.trim().to_owned(),
            ip,
            addresses: vec![ip],
            resolve_ms: None,
        });
    }
    let start = Instant::now();
    let lookup = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host.as_str(), 0)));
    let found = tokio::select! {
        _ = cancel.cancelled() => return Err(TestError::Cancelled),
        r = lookup => r,
    };
    let resolve_ms = start.elapsed().as_secs_f64() * 1000.0;
    let addresses: Vec<IpAddr> = match found {
        Err(_) => {
            return Err(WifiError::Timeout(format!(
                "resolving {host} took longer than {} s",
                RESOLVE_TIMEOUT.as_secs()
            ))
            .into())
        }
        Ok(Err(e)) => {
            return Err(WifiError::Backend(format!("cannot resolve {host}: {e}"))
                .with_hint("Check the name, and that this computer has a working DNS server.")
                .into())
        }
        Ok(Ok(addrs)) => {
            let mut out: Vec<IpAddr> = Vec::new();
            for a in addrs.map(|a| a.ip()) {
                if !out.contains(&a) {
                    out.push(a);
                }
            }
            out
        }
    };
    let ip = choose(&addresses, family)
        .ok_or_else(|| WifiError::Backend(format!("{host} has no {} address", family.label())))?;
    Ok(ResolvedTarget {
        input: input.trim().to_owned(),
        ip,
        addresses,
        resolve_ms: Some(resolve_ms),
    })
}

/// Where traffic to a target leaves this computer, as the OS decides it
/// (policy routing and VPNs included): the source address it would use and
/// the interface that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Egress {
    pub source: IpAddr,
    /// Interface name (Linux `wlp0s20f3`; Windows the friendly name);
    /// `None` if no interface owns the source address any more.
    pub iface: Option<String>,
}

/// Ask the OS which source address it would use for `target` (a connected
/// UDP socket sends nothing) and which interface owns it.
pub async fn egress(target: IpAddr) -> Result<Egress> {
    super::blocking("finding the route", OS_CALL_TIMEOUT, move || {
        let bind: SocketAddr = match target {
            IpAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
            IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = UdpSocket::bind(bind)
            .map_err(|e| WifiError::Backend(format!("cannot create a socket: {e}")))?;
        socket.connect((target, 9)).map_err(|e| {
            WifiError::Backend(format!("there is no route to {target}: {e}")).with_hint(
                "Check that this computer is connected to a network that reaches the target.",
            )
        })?;
        let source = socket
            .local_addr()
            .map_err(|e| WifiError::Backend(format!("cannot read the source address: {e}")))?
            .ip();
        Ok(Egress {
            source,
            iface: interface_owning(source)?,
        })
    })
    .await
}

#[cfg(target_os = "linux")]
fn interface_owning(addr: IpAddr) -> Result<Option<String>> {
    Ok(linux_addresses()?
        .into_iter()
        .find(|(_, ip)| *ip == addr)
        .map(|(name, _)| name))
}

#[cfg(windows)]
fn interface_owning(addr: IpAddr) -> Result<Option<String>> {
    super::windows::interface_owning(addr)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn interface_owning(_addr: IpAddr) -> Result<Option<String>> {
    Ok(None)
}

/// Every interface address (`getifaddrs`), as (interface name, address).
#[cfg(target_os = "linux")]
pub(crate) fn linux_addresses() -> Result<Vec<(String, IpAddr)>> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list freed below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(WifiError::Backend(format!(
            "cannot list interface addresses: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut out = Vec::new();
    let mut p = head;
    // SAFETY: a list from getifaddrs, alive until freeifaddrs.
    while let Some(entry) = unsafe { p.as_ref() } {
        p = entry.ifa_next;
        let Some(sa) = (unsafe { entry.ifa_addr.as_ref() }) else {
            continue;
        };
        let ip = match i32::from(sa.sa_family) {
            libc::AF_INET => {
                let sin = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                IpAddr::from(u32::from_be(sin.sin_addr.s_addr).to_be_bytes())
            }
            libc::AF_INET6 => {
                let sin6 = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                IpAddr::from(sin6.sin6_addr.s6_addr)
            }
            _ => continue,
        };
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .into_owned();
        out.push((name, ip));
    }
    // SAFETY: the list from getifaddrs, freed once.
    unsafe { libc::freeifaddrs(head) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_checked() {
        assert_eq!(parse_host(" 192.168.1.1 ").unwrap(), "192.168.1.1");
        assert_eq!(parse_host("[2001:db8::1]").unwrap(), "2001:db8::1");
        assert_eq!(parse_host("router.local.").unwrap(), "router.local");
        assert_eq!(
            parse_host("_sip._tcp.example.com").unwrap(),
            "_sip._tcp.example.com"
        );
        for bad in ["", "  ", "a b", "http://x", "x..y", &"a".repeat(64)] {
            assert!(parse_host(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn family_picks_the_first_matching_address() {
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(choose(&[v6, v4], IpFamily::Any), Some(v6));
        assert_eq!(choose(&[v6, v4], IpFamily::V4), Some(v4));
        assert_eq!(choose(&[v4], IpFamily::V6), None);
    }

    #[tokio::test]
    async fn literals_and_localhost_resolve() {
        let r = resolve("127.0.0.1", IpFamily::Any, &Cancel::never())
            .await
            .unwrap();
        assert_eq!((r.ip, r.resolve_ms), ("127.0.0.1".parse().unwrap(), None));
        assert!(resolve("127.0.0.1", IpFamily::V6, &Cancel::never())
            .await
            .is_err());
        let r = resolve("localhost", IpFamily::V4, &Cancel::never())
            .await
            .unwrap();
        assert!(r.ip.is_loopback() && r.resolve_ms.is_some(), "{r:?}");
    }

    #[tokio::test]
    async fn loopback_egress_is_the_loopback_interface() {
        let e = egress("127.0.0.1".parse().unwrap()).await.unwrap();
        assert!(e.source.is_loopback());
        if cfg!(target_os = "linux") {
            assert_eq!(e.iface.as_deref(), Some("lo"));
        }
    }
}
