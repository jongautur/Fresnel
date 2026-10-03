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

/// Probes kept in a result; a longer continuous run keeps the most recent
/// ones (statistics still cover every probe).
pub const MAX_KEPT_PROBES: usize = 10_000;
/// Most probes in a counted run.
pub const MAX_COUNT: u32 = 100_000;
/// Largest ICMP payload (bytes after the 8-byte header).
pub const MAX_PAYLOAD: u16 = 8192;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PingConfig {
    /// `None`: until cancelled (the Tools page's continuous mode).
    pub count: Option<u32>,
    /// Between probe starts.
    pub interval: Duration,
    /// Per probe.
    pub timeout: Duration,
    /// Port for TCP-connect probes.
    pub tcp_port: u16,
    /// ICMP payload bytes after the 8-byte header.
    pub payload_len: u16,
}

impl Default for PingConfig {
    fn default() -> Self {
        Self {
            count: Some(10),
            interval: Duration::from_millis(250),
            timeout: Duration::from_secs(1),
            tcp_port: 80,
            payload_len: 32,
        }
    }
}

impl PingConfig {
    fn validate(&self) -> Result<(), WifiError> {
        if self.count.is_some_and(|n| !(1..=MAX_COUNT).contains(&n)) {
            return Err(WifiError::InvalidInput(format!(
                "ping count must be 1–{MAX_COUNT}"
            )));
        }
        if self.payload_len > MAX_PAYLOAD {
            return Err(WifiError::InvalidInput(format!(
                "ping payload must be 0–{MAX_PAYLOAD} bytes"
            )));
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

    /// Upper bound of a whole run, for the outer deadline; `None` for a
    /// continuous run (it ends when cancelled; each probe has a timeout).
    pub fn max_duration(&self) -> Option<Duration> {
        self.count
            .map(|n| (self.interval + self.timeout) * n + Duration::from_secs(5))
    }

    pub(crate) fn wants_more(&self, sent: u32) -> bool {
        self.count.is_none_or(|n| sent < n)
    }
}

/// What happened to one probe. Fields are camelCase like the rest of the
/// results; point tests stored before 0.5 wrote `rtt_ms`, still read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProbeOutcome {
    Reply {
        #[serde(alias = "rtt_ms")]
        rtt_ms: f64,
    },
    /// TCP only: the host answered with a reset (port closed). A real
    /// round trip, so it counts towards latency — except on Windows, which
    /// retries the connection after a reset (about 2 s) before reporting
    /// it, so the time isn't one round trip and is left out (None).
    Refused {
        #[serde(alias = "rtt_ms")]
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
    /// ICMP payload bytes (absent in results stored before it was recorded).
    #[serde(default)]
    pub payload_len: Option<u16>,
    /// The user stopped the run; the numbers cover the probes sent so far.
    #[serde(default)]
    pub stopped: bool,
    /// Probes before `probes[0]` that aren't kept (a long continuous run
    /// keeps the last [`MAX_KEPT_PROBES`]); the statistics include them.
    #[serde(default)]
    pub probes_dropped: u32,
    pub probes: Vec<ProbeOutcome>,
}

impl PingResult {
    pub fn from_probes(
        method: ProbeMethod,
        port: Option<u16>,
        resolution_ms: Option<f64>,
        probes: Vec<ProbeOutcome>,
    ) -> Self {
        let mut log = ProbeLog::new(method, port, resolution_ms);
        for p in probes {
            log.push(p);
        }
        log.into_result()
    }
}

/// Called with each probe's sequence number (from 0) and outcome as it
/// completes: the Tools page's live output.
pub type OnProbe = Box<dyn FnMut(u32, &ProbeOutcome) + Send>;

fn no_live_output() -> OnProbe {
    Box::new(|_, _| {})
}

/// Running statistics over every probe, keeping the most recent outcomes.
#[derive(Debug, Clone)]
pub(crate) struct ProbeLog {
    method: ProbeMethod,
    port: Option<u16>,
    resolution_ms: Option<f64>,
    payload_len: Option<u16>,
    probes: std::collections::VecDeque<ProbeOutcome>,
    dropped: u32,
    sent: u32,
    received: u32,
    rtt_count: u32,
    rtt_sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    last_rtt: Option<f64>,
    jitter_sum: f64,
}

impl ProbeLog {
    pub(crate) fn new(method: ProbeMethod, port: Option<u16>, resolution_ms: Option<f64>) -> Self {
        Self {
            method,
            port,
            resolution_ms,
            payload_len: None,
            probes: std::collections::VecDeque::new(),
            dropped: 0,
            sent: 0,
            received: 0,
            rtt_count: 0,
            rtt_sum: 0.0,
            min: None,
            max: None,
            last_rtt: None,
            jitter_sum: 0.0,
        }
    }

    pub(crate) fn sent(&self) -> u32 {
        self.sent
    }

    pub(crate) fn push(&mut self, probe: ProbeOutcome) {
        self.sent += 1;
        // Answered probes, including resets without a usable time (Windows):
        // the host did answer, so they aren't loss.
        if matches!(
            probe,
            ProbeOutcome::Reply { .. } | ProbeOutcome::Refused { .. }
        ) {
            self.received += 1;
        }
        if let Some(rtt) = probe.rtt_ms() {
            self.rtt_count += 1;
            self.rtt_sum += rtt;
            self.min = Some(self.min.map_or(rtt, |m| m.min(rtt)));
            self.max = Some(self.max.map_or(rtt, |m| m.max(rtt)));
            if let Some(last) = self.last_rtt {
                self.jitter_sum += (rtt - last).abs();
            }
            self.last_rtt = Some(rtt);
        }
        if self.probes.len() == MAX_KEPT_PROBES {
            self.probes.pop_front();
            self.dropped += 1;
        }
        self.probes.push_back(probe);
    }

    pub(crate) fn into_result(self) -> PingResult {
        let sent = self.sent;
        PingResult {
            version: PING_RESULTS_VERSION,
            method: self.method,
            port: self.port,
            sent,
            received: self.received,
            loss_percent: if sent == 0 {
                0.0
            } else {
                100.0 * f64::from(sent - self.received) / f64::from(sent)
            },
            min_ms: self.min,
            avg_ms: (self.rtt_count > 0).then(|| self.rtt_sum / f64::from(self.rtt_count)),
            max_ms: self.max,
            jitter_ms: (self.rtt_count > 1)
                .then(|| self.jitter_sum / f64::from(self.rtt_count - 1)),
            resolution_ms: self.resolution_ms,
            fallback_reason: None,
            payload_len: self.payload_len,
            stopped: false,
            probes_dropped: self.dropped,
            probes: self.probes.into(),
        }
    }
}

/// How a probe loop ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    Done,
    Cancelled,
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
    if config.count.is_none() {
        return Err(WifiError::InvalidInput("a point test needs a ping count".into()).into());
    }
    match run(Some(binding), target, config, cancel, no_live_output()).await? {
        (result, Ended::Done) => Ok(result),
        (_, Ended::Cancelled) => Err(TestError::Cancelled),
    }
}

/// The Tools page's ping: bound to the Wi-Fi interface or (`None`) along
/// the system's route, reporting each probe through `on_probe`. Cancelling
/// stops the run and returns what was measured so far (`stopped`).
pub async fn ping_live(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    config: PingConfig,
    method: ProbeMethod,
    cancel: &Cancel,
    on_probe: OnProbe,
) -> TestResult<PingResult> {
    let (mut result, ended) = match method {
        ProbeMethod::Icmp => run(binding, target, config, cancel, on_probe).await?,
        ProbeMethod::TcpConnect => {
            config.validate()?;
            let mut log = ProbeLog::new(ProbeMethod::TcpConnect, Some(config.tcp_port), None);
            let mut on_probe = on_probe;
            let ended = tcp_run(binding, target, config, cancel, &mut log, &mut on_probe).await?;
            (log.into_result(), ended)
        }
    };
    result.stopped = ended == Ended::Cancelled;
    Ok(result)
}

async fn run(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    config: PingConfig,
    cancel: &Cancel,
    on_probe: OnProbe,
) -> TestResult<(PingResult, Ended)> {
    config.validate()?;
    cancel.check()?;
    #[cfg(any(target_os = "linux", windows))]
    {
        let b = binding.cloned();
        let c = cancel.clone();
        let run = move || {
            let mut on_probe = on_probe;
            #[cfg(target_os = "linux")]
            let mut log = ProbeLog::new(ProbeMethod::Icmp, None, None);
            #[cfg(windows)]
            let mut log = ProbeLog::new(
                ProbeMethod::Icmp,
                None,
                Some(super::windows::ICMP_RESOLUTION_MS),
            );
            log.payload_len = Some(config.payload_len);
            #[cfg(target_os = "linux")]
            let outcome = linux::icmp(b.as_ref(), target, config, &c, &mut log, &mut on_probe);
            #[cfg(windows)]
            let outcome =
                super::windows::icmp(b.as_ref(), target, config, &c, &mut log, &mut on_probe);
            (log, on_probe, outcome)
        };
        let task = tokio::task::spawn_blocking(run);
        let joined = match config.max_duration() {
            Some(deadline) => match tokio::time::timeout(deadline, task).await {
                Ok(joined) => joined,
                Err(_) => {
                    return Err(WifiError::Timeout("ping didn't finish in time".into()).into())
                }
            },
            None => task.await,
        };
        let (log, mut on_probe, outcome) =
            joined.map_err(|e| WifiError::Backend(format!("ping task failed: {e}")))?;
        match outcome {
            Ok(()) => Ok((log.into_result(), Ended::Done)),
            Err(IcmpError::Test(TestError::Cancelled)) => Ok((log.into_result(), Ended::Cancelled)),
            Err(IcmpError::Test(e)) => Err(e),
            Err(IcmpError::Unavailable(reason)) => {
                let mut log = ProbeLog::new(ProbeMethod::TcpConnect, Some(config.tcp_port), None);
                let ended =
                    tcp_run(binding, target, config, cancel, &mut log, &mut on_probe).await?;
                let mut result = log.into_result();
                result.fallback_reason = Some(reason);
                Ok((result, ended))
            }
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let mut on_probe = on_probe;
        let mut log = ProbeLog::new(ProbeMethod::TcpConnect, Some(config.tcp_port), None);
        let ended = tcp_run(binding, target, config, cancel, &mut log, &mut on_probe).await?;
        let mut result = log.into_result();
        result.fallback_reason = Some("ICMP isn't implemented on this OS".into());
        Ok((result, ended))
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
    if config.count.is_none() {
        return Err(WifiError::InvalidInput("a point test needs a ping count".into()).into());
    }
    let mut log = ProbeLog::new(ProbeMethod::TcpConnect, Some(config.tcp_port), None);
    match tcp_run(
        binding,
        target,
        config,
        cancel,
        &mut log,
        &mut no_live_output(),
    )
    .await?
    {
        Ended::Done => Ok(log.into_result()),
        Ended::Cancelled => Err(TestError::Cancelled),
    }
}

/// One TCP connect, timed: the outcome as a probe, or a bind failure.
pub(crate) async fn tcp_probe(
    binding: Option<&WifiBinding>,
    addr: SocketAddr,
    timeout: Duration,
) -> TestResult<ProbeOutcome> {
    let start = Instant::now();
    let outcome = binding::connect(binding, addr, timeout).await;
    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(match outcome {
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
    })
}

async fn tcp_run(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    config: PingConfig,
    cancel: &Cancel,
    log: &mut ProbeLog,
    on_probe: &mut OnProbe,
) -> TestResult<Ended> {
    let addr = SocketAddr::new(target, config.tcp_port);
    while config.wants_more(log.sent()) {
        if cancel.is_cancelled() {
            return Ok(Ended::Cancelled);
        }
        let start = Instant::now();
        let probe = tokio::select! {
            _ = cancel.cancelled() => return Ok(Ended::Cancelled),
            r = tcp_probe(binding, addr, config.timeout) => r?,
        };
        on_probe(log.sent(), &probe);
        log.push(probe);
        if config.wants_more(log.sent()) {
            let left = config.interval.saturating_sub(start.elapsed());
            if cancel.sleep(left).await.is_err() {
                return Ok(Ended::Cancelled);
            }
        }
    }
    Ok(Ended::Done)
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::io::ErrorKind;
    use std::net::{IpAddr, SocketAddr};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    use socket2::{Domain, Protocol, SockAddr, Socket, Type};

    use super::{IcmpError, OnProbe, PingConfig, ProbeLog, ProbeOutcome};
    use crate::nettools::binding::bind_error;
    use crate::nettools::{Cancel, WifiBinding};
    use crate::WifiError;

    /// Identifies our echo payloads (the rest is zero padding).
    const PAYLOAD_TAG: &[u8] = b"FRESNEL\0";

    /// Echo over an unprivileged ping socket (`SOCK_DGRAM` + `IPPROTO_ICMP`),
    /// bound to the interface when `binding` is set. The kernel sets the
    /// identifier and filters replies to this socket; we match the sequence
    /// number.
    pub(super) fn icmp(
        binding: Option<&WifiBinding>,
        target: IpAddr,
        config: PingConfig,
        cancel: &Cancel,
        log: &mut ProbeLog,
        on_probe: &mut OnProbe,
    ) -> Result<(), IcmpError> {
        let socket = ping_socket(target)?;
        if let Some(binding) = binding {
            bind_to_device(&socket, &binding.iface)?;
        }
        if let Err(e) = socket.connect(&SockAddr::from(SocketAddr::new(target, 0))) {
            return Err(WifiError::Backend(format!("cannot address {target}: {e}")).into());
        }
        let (request, reply) = echo_types(target);
        let mut packet = vec![0u8; 8 + usize::from(config.payload_len)];
        packet[0] = request;
        let tag = PAYLOAD_TAG.len().min(usize::from(config.payload_len));
        packet[8..8 + tag].copy_from_slice(&PAYLOAD_TAG[..tag]);
        let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); packet.len() + 512];
        while config.wants_more(log.sent()) {
            cancel.check()?;
            let seq = log.sent();
            let seq16 = seq as u16;
            let start = Instant::now();
            packet[6..8].copy_from_slice(&seq16.to_be_bytes());
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
                Ok(_) => wait_reply(&socket, &mut buf, reply, seq16, start, config, cancel)?,
            };
            on_probe(seq, &outcome);
            log.push(outcome);
            if config.wants_more(log.sent()) {
                cancel.sleep_blocking(config.interval.saturating_sub(start.elapsed()))?;
            }
        }
        Ok(())
    }

    /// Echo request and reply types for the target's family.
    pub(crate) fn echo_types(target: IpAddr) -> (u8, u8) {
        match target {
            IpAddr::V4(_) => (8, 0),
            IpAddr::V6(_) => (128, 129),
        }
    }

    /// An unprivileged ping socket, or `Unavailable` (fall back to TCP)
    /// when this user may not open one.
    pub(crate) fn ping_socket(target: IpAddr) -> Result<Socket, IcmpError> {
        let (domain, protocol) = match target {
            IpAddr::V4(_) => (Domain::IPV4, Protocol::ICMPV4),
            IpAddr::V6(_) => (Domain::IPV6, Protocol::ICMPV6),
        };
        match Socket::new(domain, Type::DGRAM, Some(protocol)) {
            Ok(s) => Ok(s),
            Err(e) if matches!(e.kind(), ErrorKind::PermissionDenied) => {
                Err(IcmpError::Unavailable(format!(
                    "unprivileged ICMP is not allowed for this user \
                     (net.ipv4.ping_group_range): {e}"
                )))
            }
            Err(e) if e.raw_os_error() == Some(libc::EAFNOSUPPORT) => Err(IcmpError::Unavailable(
                format!("ICMP socket unavailable: {e}"),
            )),
            Err(e) => Err(WifiError::Backend(format!("cannot create a ping socket: {e}")).into()),
        }
    }

    fn wait_reply(
        socket: &Socket,
        buf: &mut [std::mem::MaybeUninit<u8>],
        reply_type: u8,
        seq: u16,
        start: Instant,
        config: PingConfig,
        cancel: &Cancel,
    ) -> Result<ProbeOutcome, IcmpError> {
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
            match socket.recv(buf) {
                Ok(n) => {
                    // SAFETY: recv initialised the first n bytes.
                    let bytes: Vec<u8> = buf[..n.min(8)]
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
    pub(crate) fn classify(e: &std::io::Error) -> Option<ProbeOutcome> {
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

    pub(crate) fn bind_to_device(socket: &Socket, iface: &str) -> Result<(), IcmpError> {
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
        assert_eq!(json["probes"][0]["rttMs"], 2.0);
        // Results stored before the field was camelCase still read.
        let old: ProbeOutcome =
            serde_json::from_value(serde_json::json!({ "outcome": "reply", "rtt_ms": 3.5 }))
                .unwrap();
        assert_eq!(old, ProbeOutcome::Reply { rtt_ms: 3.5 });
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
            count: Some(3),
            interval: Duration::from_millis(1),
            timeout: Duration::from_secs(2),
            tcp_port: port,
            ..PingConfig::default()
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
                count: Some(1),
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
            count: Some(0),
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
            count: Some(2),
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
