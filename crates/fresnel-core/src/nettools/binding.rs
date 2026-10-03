//! Which interface tests are bound to, and bound TCP connections.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use serde::Serialize;
use tokio::net::{TcpSocket, TcpStream};

use crate::wifi::models::ConnectionInfo;
use crate::{Result, WifiError};

/// The Wi-Fi interface a test is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WifiBinding {
    /// Interface name (Linux `wlp0s20f3`; Windows the friendly name).
    pub iface: String,
    /// Source addresses (Windows binds sockets and ICMP to these).
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    /// Windows interface indices (IPv4 and IPv6 can differ).
    pub if_index_v4: Option<u32>,
    pub if_index_v6: Option<u32>,
    /// The default gateway on the Wi-Fi interface.
    pub gateway_v4: Option<Ipv4Addr>,
}

impl WifiBinding {
    /// Linux: the kernel binds by name; addresses are informative.
    pub fn linux(iface: &str, connection: Option<&ConnectionInfo>) -> Self {
        let ipv4 = connection.and_then(|c| {
            c.ipv4_addresses
                .iter()
                .find_map(|a| a.split('/').next()?.parse().ok())
        });
        Self {
            iface: iface.into(),
            ipv4,
            ipv6: None,
            if_index_v4: None,
            if_index_v6: None,
            gateway_v4: connection
                .and_then(|c| c.ipv4_gateway.as_deref())
                .and_then(|g| g.parse().ok()),
        }
    }

    /// The source address to bind for `target`'s family (Windows).
    pub fn source_for(&self, target: IpAddr) -> Result<IpAddr> {
        match target {
            IpAddr::V4(_) => self.ipv4.map(IpAddr::V4),
            IpAddr::V6(_) => self.ipv6.map(IpAddr::V6),
        }
        .ok_or_else(|| WifiError::AdapterUnavailable {
            id: self.iface.clone(),
            reason: format!(
                "the Wi-Fi interface has no {} address to test {target} from",
                if target.is_ipv4() { "IPv4" } else { "IPv6" }
            ),
        })
    }
}

/// The binding for the adapter the point was measured with. `connection`
/// is the provider's current connection (interface name and gateway).
pub async fn wifi_binding(connection: &ConnectionInfo) -> Result<WifiBinding> {
    #[cfg(windows)]
    {
        let id = connection.adapter_id.as_str().to_owned();
        let gateway = connection
            .ipv4_gateway
            .as_deref()
            .and_then(|g| g.parse().ok());
        let mut binding = super::blocking(
            "reading the network interfaces",
            super::OS_CALL_TIMEOUT,
            move || super::windows::binding_for_adapter(&id),
        )
        .await?;
        binding.gateway_v4 = gateway.or(binding.gateway_v4);
        Ok(binding)
    }
    #[cfg(not(windows))]
    {
        let iface =
            connection
                .interface_name
                .as_deref()
                .ok_or_else(|| WifiError::AdapterUnavailable {
                    id: connection.adapter_id.to_string(),
                    reason: "the Wi-Fi interface name is unknown, so tests can't be bound to it"
                        .into(),
                })?;
        Ok(WifiBinding::linux(iface, Some(connection)))
    }
}

/// Whether IP Helper's `AdapterName` (`{GUID}`) is the adapter behind a
/// `windows:{guid}` adapter ID (lowercase, without braces).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn adapter_guid_matches(adapter_name: &str, adapter_id: &str) -> bool {
    let Some(guid) = adapter_id.strip_prefix("windows:") else {
        return false;
    };
    let clean = |s: &str| s.trim().trim_matches(['{', '}']).to_ascii_lowercase();
    !clean(guid).is_empty() && clean(guid) == clean(adapter_name)
}

/// Why a bound TCP connection failed.
#[derive(Debug)]
pub(crate) enum ConnectError {
    /// The socket couldn't be created or bound to the Wi-Fi interface.
    Bind(WifiError),
    /// The connection itself failed (refused, timed out, unreachable).
    Connect(std::io::Error),
}

/// A TCP connection to `addr` that can only use the Wi-Fi interface, or
/// (tests only) an unbound one. Bounded by `timeout`.
pub(crate) async fn connect(
    binding: Option<&WifiBinding>,
    addr: SocketAddr,
    timeout: Duration,
) -> Result<TcpStream, ConnectError> {
    let iface = binding.map_or("Wi-Fi", |b| b.iface.as_str());
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    }
    .map_err(|e| ConnectError::Bind(WifiError::Backend(format!("cannot create a socket: {e}"))))?;
    if let Some(binding) = binding {
        bind(&socket, binding, addr.ip()).map_err(|e| ConnectError::Bind(bind_error(iface, &e)))?;
    }
    match tokio::time::timeout(timeout, socket.connect(addr)).await {
        Ok(result) => result.map_err(ConnectError::Connect),
        Err(_) => Err(ConnectError::Connect(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "connect timed out",
        ))),
    }
}

/// A UDP socket for `target`'s family that can only use the Wi-Fi
/// interface, or (`None`) an unbound one that follows the system's route.
pub(crate) fn udp_socket(
    binding: Option<&WifiBinding>,
    target: IpAddr,
) -> Result<tokio::net::UdpSocket> {
    let iface = binding.map_or("Wi-Fi", |b| b.iface.as_str());
    let unspecified: SocketAddr = match target {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = match binding {
        None => std::net::UdpSocket::bind(unspecified)
            .map_err(|e| WifiError::Backend(format!("cannot create a socket: {e}")))?,
        #[cfg(target_os = "linux")]
        Some(binding) => {
            use socket2::{Domain, Socket, Type};
            let domain = if target.is_ipv4() {
                Domain::IPV4
            } else {
                Domain::IPV6
            };
            let s = Socket::new(domain, Type::DGRAM, None)
                .map_err(|e| WifiError::Backend(format!("cannot create a socket: {e}")))?;
            s.bind_device(Some(binding.iface.as_bytes()))
                .map_err(|e| bind_error(iface, &e))?;
            s.bind(&unspecified.into())
                .map_err(|e| WifiError::Backend(format!("cannot bind a socket: {e}")))?;
            s.into()
        }
        #[cfg(not(target_os = "linux"))]
        Some(binding) => {
            let source = binding.source_for(target)?;
            std::net::UdpSocket::bind(SocketAddr::new(source, 0))
                .map_err(|e| bind_error(iface, &e))?
        }
    };
    socket
        .set_nonblocking(true)
        .map_err(|e| WifiError::Backend(format!("cannot configure a socket: {e}")))?;
    tokio::net::UdpSocket::from_std(socket)
        .map_err(|e| WifiError::Backend(format!("cannot register a socket: {e}")))
}

#[cfg(target_os = "linux")]
fn bind(socket: &TcpSocket, binding: &WifiBinding, _target: IpAddr) -> std::io::Result<()> {
    socket.bind_device(Some(binding.iface.as_bytes()))
}

#[cfg(not(target_os = "linux"))]
fn bind(socket: &TcpSocket, binding: &WifiBinding, target: IpAddr) -> std::io::Result<()> {
    // Windows' strong host model sends from the interface that owns the
    // source address; the route check refuses targets routed elsewhere.
    let source = binding
        .source_for(target)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, e.to_string()))?;
    socket.bind(SocketAddr::new(source, 0))
}

/// The user-facing error for a failed bind: SO_BINDTODEVICE needs Linux 5.7
/// or CAP_NET_RAW.
pub(crate) fn bind_error(iface: &str, e: &std::io::Error) -> WifiError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        WifiError::PermissionDenied(format!(
            "the system didn't allow binding the test to {iface}: {e}"
        ))
        .with_hint("Binding sockets to an interface needs Linux 5.7 or newer (or CAP_NET_RAW).")
    } else {
        WifiError::Backend(format!("cannot bind the test to {iface}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::models::{AdapterId, Signal};

    #[test]
    fn linux_binding_from_connection() {
        let c = ConnectionInfo {
            adapter_id: AdapterId::linux("wlan0"),
            interface_name: Some("wlan0".into()),
            ssid: None,
            bssid: None,
            frequency_mhz: None,
            channel: None,
            band: None,
            channel_width_mhz: None,
            signal: Signal {
                dbm: None,
                quality_percent: None,
            },
            bitrate_kbps: None,
            tx_rate: None,
            rx_rate: None,
            security: None,
            ipv4_addresses: vec!["garbage".into(), "192.168.1.20/24".into()],
            ipv4_gateway: Some("192.168.1.1".into()),
        };
        let b = WifiBinding::linux("wlan0", Some(&c));
        assert_eq!(b.ipv4, Some(Ipv4Addr::new(192, 168, 1, 20)));
        assert_eq!(b.gateway_v4, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(
            b.source_for("10.0.0.1".parse().unwrap()).unwrap(),
            IpAddr::from([192, 168, 1, 20])
        );
        assert!(b.source_for("2001:db8::1".parse().unwrap()).is_err());
    }

    #[test]
    fn windows_adapter_guid_matching() {
        let id = "windows:3f2a5c1e-0b7d-4e7a-9c1f-1234567890ab";
        assert!(adapter_guid_matches(
            "{3F2A5C1E-0B7D-4E7A-9C1F-1234567890AB}",
            id
        ));
        assert!(!adapter_guid_matches(
            "{00000000-0B7D-4E7A-9C1F-1234567890AB}",
            id
        ));
        assert!(!adapter_guid_matches("{}", "windows:"));
        assert!(!adapter_guid_matches("wlan0", "linux:wlan0"));
    }
}
