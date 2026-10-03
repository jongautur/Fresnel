//! Latency, jitter and loss: real ICMP echo without privileges where the OS
//! allows it (Linux ping sockets, Windows `IcmpSendEcho2Ex`), else timed
//! TCP connects, labelled as such.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::binding::{self, ConnectError, WifiBinding};
use super::{Cancel, TestError, TestResult};
use crate::WifiError;

/// Results JSON layout version, stored with each test.
pub const PING_RESULTS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeMethod {
    Icmp,
    /// Time from SYN to SYN-ACK (or RST) on a TCP port.
    TcpConnect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PingConfig {
    pub count: u32,
    /// Between probe starts.
    pub interval: Duration,
    /// Per probe.
    pub timeout: Duration,
    /// Port for TCP-connect probes.
    pub tcp_port: u16,
}

impl Default for PingConfig {
    fn default() -> Self {
        Self {
            count: 10,
            interval: Duration::from_millis(250),
            timeout: Duration::from_secs(1),
            tcp_port: 80,
        }
    }
}

impl PingConfig {
    fn validate(&self) -> Result<(), WifiError> {
        if !(1..=100).contains(&self.count) {
            return Err(WifiError::InvalidInput("ping count must be 1–100".into()));
        }
        if self.timeout.is_zero() || self.timeout > Duration::from_secs(10) {
            return Err(WifiError::InvalidInput(
                "ping timeout must be up to 10 s".into(),
            ));
        }
        if self.interval > Duration::from_secs(10) {
            return Err(WifiError::InvalidInput(
                "ping interval must be up to 10 s".into(),
            ));
        }
        Ok(())
    }

    /// Upper bound of a whole run, for the outer deadline.
    pub fn max_duration(&self) -> Duration {
        (self.interval + self.timeout) * self.count + Duration::from_secs(5)
    }
}

/// What happened to one probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProbeOutcome {
    Reply {
        rtt_ms: f64,
    },
    /// TCP only: the host answered with a reset (port closed). A real
    /// round trip, so it counts towards latency — except on Windows, which
    /// retries the connection after a reset (about 2 s) before reporting
    /// it, so the time isn't one round trip and is left out (None).
    Refused {
        rtt_ms: Option<f64>,
    },
    Timeout,
    /// An ICMP error or the OS said the host or network is unreachable.
    Unreachable {
        detail: String,
    },
    Error {
        detail: String,
    },
}

impl ProbeOutcome {
    fn rtt_ms(&self) -> Option<f64> {
        match self {
            Self::Reply { rtt_ms } => Some(*rtt_ms),
            Self::Refused { rtt_ms } => *rtt_ms,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResult {
    pub version: u32,
    pub method: ProbeMethod,
    /// TCP-connect probes: the port timed.
    pub port: Option<u16>,
    pub sent: u32,
    pub received: u32,
    pub loss_percent: f64,
    pub min_ms: Option<f64>,
    pub avg_ms: Option<f64>,
    pub max_ms: Option<f64>,
    /// Mean absolute difference of consecutive round-trip times.
    pub jitter_ms: Option<f64>,
    /// Clock resolution of the RTTs when coarser than 0.1 ms (Windows ICMP:
    /// 1 ms), so jitter below it isn't meaningful.
    pub resolution_ms: Option<f64>,
    /// Why ICMP wasn't used, when it wasn't.
    pub fallback_reason: Option<String>,
    pub probes: Vec<ProbeOutcome>,
}

impl PingResult {
    pub fn from_probes(
        method: ProbeMethod,
        port: Option<u16>,
        resolution_ms: Option<f64>,
        probes: Vec<ProbeOutcome>,
    ) -> Self {
        let rtts: Vec<f64> = probes.iter().filter_map(ProbeOutcome::rtt_ms).collect();
        let sent = probes.len() as u32;
        // Answered probes, including resets without a usable time (Windows):
        // the host did answer, so they aren't loss.
        let received = probes
            .iter()
            .filter(|p| matches!(p, ProbeOutcome::Reply { .. } | ProbeOutcome::Refused { .. }))
            .count() as u32;
        Self {
            version: PING_RESULTS_VERSION,
            method,
            port,
            sent,
            received,
            loss_percent: if sent == 0 {
                0.0
            } else {
                100.0 * f64::from(sent - received) / f64::from(sent)
            },
            min_ms: rtts.iter().copied().reduce(f64::min),
            avg_ms: (!rtts.is_empty()).then(|| rtts.iter().sum::<f64>() / rtts.len() as f64),
            max_ms: rtts.iter().copied().reduce(f64::max),
            jitter_ms: (rtts.len() > 1).then(|| {
                rtts.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (rtts.len() - 1) as f64
            }),
            resolution_ms,
            fallback_reason: None,
            probes,
        }
    }
}

/// Ping `target` over the Wi-Fi interface. ICMP where the OS lets an
/// unprivileged process send echo requests; on Linux, if ping sockets are
/// disabled (`net.ipv4.ping_group_range`), timed TCP connects instead,
/// labelled with the reason. The caller has checked the route.
pub async fn ping(
    binding: &WifiBinding,
    target: IpAddr,
    config: PingConfig,
    cancel: &Cancel,
) -> TestResult<PingResult> {
    config.validate()?;
    cancel.check()?;
    let deadline = config.max_duration();
    #[cfg(any(target_os = "linux", windows))]
    {
        let b = binding.clone();
        let c = cancel.clone();
        #[cfg(target_os = "linux")]
        let run = move || linux::icmp(&b, target, config, &c);
        #[cfg(windows)]
        let run = move || super::windows::icmp(&b, target, config, &c);
        let outcome = match tokio::time::timeout(deadline, tokio::task::spawn_blocking(run)).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => Err(WifiError::Backend(format!("ping task failed: {e}")).into()),
            Err(_) => Err(WifiError::Timeout("ping didn't finish in time".into()).into()),
        };
        match outcome {
            Err(IcmpError::Unavailable(reason)) => {
                let mut result = tcp_ping(binding, target, config, cancel).await?;
                result.fallback_reason = Some(reason);
                Ok(result)
            }
            Err(IcmpError::Test(e)) => Err(e),
            Ok(result) => Ok(result),
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = deadline;
        let mut result = tcp_ping(binding, target, config, cancel).await?;
        result.fallback_reason = Some("ICMP isn't implemented on this OS".into());
        Ok(result)
    }
}

/// Why an ICMP run produced no result.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
pub(crate) enum IcmpError {
    /// ICMP can't be used here: fall back to TCP connects.
    Unavailable(String),
    Test(TestError),
}

impl From<TestError> for IcmpError {
    fn from(e: TestError) -> Self {
        Self::Test(e)
    }
}

impl From<WifiError> for IcmpError {
    fn from(e: WifiError) -> Self {
        Self::Test(TestError::Failed(e))
    }
}

/// Timed TCP connects to `target:config.tcp_port`, bound to Wi-Fi. A reset
/// (port closed) is a real round trip and counts; a timeout doesn't.
pub async fn tcp_ping(
    binding: &WifiBinding,
    target: IpAddr,
    config: PingConfig,
    cancel: &Cancel,
) -> TestResult<PingResult> {
    tcp_probes(Some(binding), target, config, cancel).await
}

pub(crate) async fn tcp_probes(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    config: PingConfig,
    cancel: &Cancel,
) -> TestResult<PingResult> {
    config.validate()?;
    let addr = SocketAddr::new(target, config.tcp_port);
    let mut probes = Vec::with_capacity(config.count as usize);
    for i in 0..config.count {
        cancel.check()?;
        let start = Instant::now();
        let outcome = tokio::select! {
            _ = cancel.cancelled() => return Err(TestError::Cancelled),
            r = binding::connect(binding, addr, config.timeout) => r,
        };
        let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
        probes.push(match outcome {
            Ok(stream) => {
                drop(stream);
                ProbeOutcome::Reply { rtt_ms }
            }
            Err(ConnectError::Bind(e)) => return Err(e.into()),
            Err(ConnectError::Connect(e)) => match e.kind() {
                std::io::ErrorKind::ConnectionRefused => ProbeOutcome::Refused {
                    rtt_ms: (!cfg!(windows)).then_some(rtt_ms),
                },
                std::io::ErrorKind::TimedOut => ProbeOutcome::Timeout,
                std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::NetworkUnreachable => {
                    ProbeOutcome::Unreachable {
                        detail: e.to_string(),
                    }
                }
                _ => ProbeOutcome::Error {
                    detail: e.to_string(),
                },
            },
        });
        if i + 1 < config.count {
            let left = config.interval.saturating_sub(start.elapsed());
            cancel.sleep(left).await?;
        }
    }
    Ok(PingResult::from_probes(
        ProbeMethod::TcpConnect,
        Some(config.tcp_port),
        None,
        probes,
    ))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::ErrorKind;
    use std::net::{IpAddr, SocketAddr};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    use socket2::{Domain, Protocol, SockAddr, Socket, Type};

    use super::{IcmpError, PingConfig, PingResult, ProbeMethod, ProbeOutcome};
    use crate::nettools::binding::bind_error;
    use crate::nettools::{Cancel, WifiBinding};
    use crate::WifiError;

    /// Echo over an unprivileged ping socket (`SOCK_DGRAM` + `IPPROTO_ICMP`),
    /// bound to the interface. The kernel sets the identifier and filters
    /// replies to this socket; we match the sequence number.
    pub(super) fn icmp(
        binding: &WifiBinding,
        target: IpAddr,
        config: PingConfig,
        cancel: &Cancel,
    ) -> Result<PingResult, IcmpError> {
        let (domain, protocol, request, reply) = match target {
            IpAddr::V4(_) => (Domain::IPV4, Protocol::ICMPV4, 8u8, 0u8),
            IpAddr::V6(_) => (Domain::IPV6, Protocol::ICMPV6, 128, 129),
        };
        let socket = match Socket::new(domain, Type::DGRAM, Some(protocol)) {
            Ok(s) => s,
            Err(e) if matches!(e.kind(), ErrorKind::PermissionDenied) => {
                return Err(IcmpError::Unavailable(format!(
                    "unprivileged ICMP is not allowed for this user \
                     (net.ipv4.ping_group_range): {e}"
                )))
            }
            Err(e) if e.raw_os_error() == Some(libc::EAFNOSUPPORT) => {
                return Err(IcmpError::Unavailable(format!(
                    "ICMP socket unavailable: {e}"
                )))
            }
            Err(e) => {
                return Err(WifiError::Backend(format!("cannot create a ping socket: {e}")).into())
            }
        };
        bind_to_device(&socket, &binding.iface)?;
        if let Err(e) = socket.connect(&SockAddr::from(SocketAddr::new(target, 0))) {
            return Err(WifiError::Backend(format!("cannot address {target}: {e}")).into());
        }
        let mut probes = Vec::with_capacity(config.count as usize);
        for seq in 0..config.count {
            cancel.check()?;
            let seq16 = seq as u16;
            let start = Instant::now();
            let mut packet = [0u8; 24];
            packet[0] = request;
            packet[6..8].copy_from_slice(&seq16.to_be_bytes());
            packet[8..16].copy_from_slice(b"FRESNEL\0");
            let outcome = match socket.send(&packet) {
                Err(e) => match classify(&e) {
                    Some(o) => o,
                    None => {
                        return Err(WifiError::Backend(format!(
                            "sending an echo request failed: {e}"
                        ))
                        .into())
                    }
                },
                Ok(_) => wait_reply(&socket, reply, seq16, start, config, cancel)?,
            };
            probes.push(outcome);
            if seq + 1 < config.count {
                cancel.sleep_blocking(config.interval.saturating_sub(start.elapsed()))?;
            }
        }
        Ok(PingResult::from_probes(
            ProbeMethod::Icmp,
            None,
            None,
            probes,
        ))
    }

    fn wait_reply(
        socket: &Socket,
        reply_type: u8,
        seq: u16,
        start: Instant,
        config: PingConfig,
        cancel: &Cancel,
    ) -> Result<ProbeOutcome, IcmpError> {
        let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 1500];
        loop {
            cancel.check()?;
            let left = config.timeout.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Ok(ProbeOutcome::Timeout);
            }
            // Wake at least every 100 ms to notice a cancel.
            let wait = left.min(std::time::Duration::from_millis(100));
            socket
                .set_read_timeout(Some(wait))
                .map_err(|e| WifiError::Backend(format!("ping socket: {e}")))?;
            match socket.recv(&mut buf) {
                Ok(n) => {
                    // SAFETY: recv initialised the first n bytes.
                    let bytes: Vec<u8> = buf[..n]
                        .iter()
                        .map(|b| unsafe { b.assume_init() })
                        .collect();
                    if n >= 8
                        && bytes[0] == reply_type
                        && u16::from_be_bytes([bytes[6], bytes[7]]) == seq
                    {
                        return Ok(ProbeOutcome::Reply {
                            rtt_ms: start.elapsed().as_secs_f64() * 1000.0,
                        });
                    }
                    // A late reply to an earlier probe: keep waiting.
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => match classify(&e) {
                    Some(o) => return Ok(o),
                    None => {
                        return Err(WifiError::Backend(format!(
                            "receiving an echo reply failed: {e}"
                        ))
                        .into())
                    }
                },
            }
        }
    }

    /// ICMP errors arrive as socket errors on a connected ping socket.
    fn classify(e: &std::io::Error) -> Option<ProbeOutcome> {
        match e.raw_os_error() {
            Some(libc::EHOSTUNREACH | libc::ENETUNREACH | libc::ECONNREFUSED | libc::EHOSTDOWN) => {
                Some(ProbeOutcome::Unreachable {
                    detail: e.to_string(),
                })
            }
            Some(libc::EMSGSIZE | libc::ENOBUFS | libc::EAGAIN) => Some(ProbeOutcome::Error {
                detail: e.to_string(),
            }),
            _ => None,
        }
    }

    fn bind_to_device(socket: &Socket, iface: &str) -> Result<(), IcmpError> {
        let name = std::ffi::CString::new(iface)
            .map_err(|_| WifiError::InvalidInput("interface name contains a NUL".into()))?;
        // SAFETY: a valid socket fd and a NUL-terminated name with its length.
        let rc = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                name.as_ptr().cast(),
                name.as_bytes_with_nul().len() as libc::socklen_t,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(bind_error(iface, &std::io::Error::last_os_error()).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_from_probes() {
        let r = PingResult::from_probes(
            ProbeMethod::Icmp,
            None,
            None,
            vec![
                ProbeOutcome::Reply { rtt_ms: 2.0 },
                ProbeOutcome::Timeout,
                ProbeOutcome::Reply { rtt_ms: 6.0 },
                ProbeOutcome::Reply { rtt_ms: 4.0 },
                ProbeOutcome::Unreachable { detail: "x".into() },
            ],
        );
        assert_eq!((r.sent, r.received), (5, 3));
        assert_eq!(r.loss_percent, 40.0);
        assert_eq!(
            (r.min_ms, r.avg_ms, r.max_ms),
            (Some(2.0), Some(4.0), Some(6.0))
        );
        assert_eq!(r.jitter_ms, Some(3.0));
        let none =
            PingResult::from_probes(ProbeMethod::Icmp, None, None, vec![ProbeOutcome::Timeout]);
        assert_eq!(none.loss_percent, 100.0);
        assert_eq!((none.avg_ms, none.jitter_ms), (None, None));
        // The JSON is tagged and versioned for storage.
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["probes"][1]["outcome"], "timeout");
    }

    #[tokio::test]
    async fn tcp_probes_time_accepts_and_resets() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                drop(s);
            }
        });
        let config = PingConfig {
            count: 3,
            interval: Duration::from_millis(1),
            timeout: Duration::from_secs(2),
            tcp_port: port,
        };
        let r = tcp_probes(None, "127.0.0.1".parse().unwrap(), config, &Cancel::never())
            .await
            .unwrap();
        assert_eq!(
            (r.method, r.port, r.received),
            (ProbeMethod::TcpConnect, Some(port), 3)
        );

        // A closed port answers with a reset: a round trip, labelled.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let r = tcp_probes(
            None,
            "127.0.0.1".parse().unwrap(),
            PingConfig {
                tcp_port: port,
                count: 1,
                // Windows retries a reset connect for about 2 s first.
                timeout: Duration::from_secs(10),
                ..config
            },
            &Cancel::never(),
        )
        .await
        .unwrap();
        assert!(matches!(r.probes[0], ProbeOutcome::Refused { .. }), "{r:?}");
        assert_eq!(r.received, 1, "{r:?}");
        assert_eq!(r.avg_ms.is_some(), !cfg!(windows), "{r:?}");
    }

    #[tokio::test]
    async fn tcp_probes_cancel() {
        let (tx, cancel) = Cancel::new();
        tx.send(true).unwrap();
        let r = tcp_probes(
            None,
            "127.0.0.1".parse().unwrap(),
            PingConfig::default(),
            &cancel,
        )
        .await;
        assert!(matches!(r, Err(TestError::Cancelled)));
        assert!(PingConfig {
            count: 0,
            ..PingConfig::default()
        }
        .validate()
        .is_err());
    }

    /// Real ICMP to loopback when this machine allows ping sockets.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_icmp_to_loopback_or_labelled_fallback() {
        let binding = WifiBinding::linux("lo", None);
        let config = PingConfig {
            count: 2,
            interval: Duration::from_millis(10),
            ..PingConfig::default()
        };
        match ping(
            &binding,
            "127.0.0.1".parse().unwrap(),
            config,
            &Cancel::never(),
        )
        .await
        {
            Ok(r) if r.method == ProbeMethod::Icmp => assert_eq!(r.received, 2, "{r:?}"),
            Ok(r) => assert!(r.fallback_reason.is_some()),
            // Kernels before 5.7 refuse SO_BINDTODEVICE without CAP_NET_RAW.
            Err(TestError::Failed(e)) => assert_eq!(e.kind(), "permission_denied", "{e}"),
            Err(e) => panic!("{e}"),
        }
    }
}
