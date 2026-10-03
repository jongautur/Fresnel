//! Traceroute / MTR without privileges: probes with increasing TTL (hop
//! limit), the routers' ICMP "time exceeded" errors name each hop.
//!
//! - Linux: an unprivileged ICMP ping socket (UDP if ping sockets aren't
//!   allowed) with `IP_RECVERR` / `IPV6_RECVERR`: the kernel queues the
//!   ICMP errors on the socket's error queue with the router's address and
//!   the start of our probe (its sequence number), like `tracepath`.
//! - Windows: `IcmpSendEcho2Ex` / `Icmp6SendEcho2` with a TTL; a probe that
//!   expires returns `IP_TTL_EXPIRED_TRANSIT` and the router's address. No
//!   admin rights needed.
//!
//! A round sends one probe per TTL at once and waits up to the timeout for
//! the answers. The first round finds the destination's distance; later
//! rounds (MTR mode) probe only that far. Hops that don't answer are shown
//! as no answer, never guessed. Hop names come from reverse DNS (PTR)
//! queries to the system's DNS server, along the system route.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::binding::WifiBinding;
use super::ping::IcmpError;
use super::{dns, Cancel, TestError, TestResult};
use crate::WifiError;

/// Results JSON layout version, stored with each run.
pub const TRACE_RESULTS_VERSION: u32 = 1;
pub const MAX_HOPS: u8 = 64;
pub const MAX_ROUNDS: u32 = 10_000;
/// UDP probes go to 33434 + TTL − 1 (the classic traceroute ports).
const UDP_BASE_PORT: u16 = 33434;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceMethod {
    /// ICMP echo requests (Linux ping sockets, Windows ICMP API).
    Icmp,
    /// UDP to high ports; the destination answers "port unreachable".
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TraceConfig {
    pub max_hops: u8,
    /// How long a round waits for answers.
    pub timeout: Duration,
    /// `None`: until cancelled (MTR mode).
    pub rounds: Option<u32>,
    /// Between round starts.
    pub interval: Duration,
    /// ICMP falls back to UDP where ping sockets aren't allowed (Linux);
    /// UDP is Linux only.
    pub method: TraceMethod,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            max_hops: 30,
            timeout: Duration::from_secs(2),
            rounds: Some(3),
            interval: Duration::from_secs(1),
            method: TraceMethod::Icmp,
        }
    }
}

impl TraceConfig {
    fn validate(&self) -> Result<(), WifiError> {
        if !(1..=MAX_HOPS).contains(&self.max_hops) {
            return Err(WifiError::InvalidInput(format!(
                "max hops must be 1–{MAX_HOPS}"
            )));
        }
        if self.timeout < Duration::from_millis(100) || self.timeout > Duration::from_secs(10) {
            return Err(WifiError::InvalidInput(
                "traceroute timeout must be 0.1–10 s".into(),
            ));
        }
        if self.rounds.is_some_and(|r| !(1..=MAX_ROUNDS).contains(&r)) {
            return Err(WifiError::InvalidInput(format!(
                "traceroute rounds must be 1–{MAX_ROUNDS}"
            )));
        }
        if self.interval > Duration::from_secs(60) {
            return Err(WifiError::InvalidInput(
                "traceroute interval must be up to 60 s".into(),
            ));
        }
        if self.method == TraceMethod::Udp && !cfg!(target_os = "linux") {
            return Err(WifiError::Unsupported(
                "UDP traceroute needs administrator rights on this OS; use ICMP".into(),
            ));
        }
        Ok(())
    }
}

/// What answered one probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HopReply {
    /// A router on the way: ICMP time exceeded.
    TimeExceeded,
    /// The destination itself (echo reply, or "port unreachable" to UDP).
    Reached,
    /// An ICMP destination-unreachable error from a router (or the
    /// destination for something other than our UDP port).
    Unreachable { detail: String },
}

/// One probe's outcome. `from` and `rtt_ms` are `None` when nothing
/// answered within the timeout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HopProbe {
    pub round: u32,
    pub ttl: u8,
    pub from: Option<IpAddr>,
    pub rtt_ms: Option<f64>,
    pub reply: Option<HopReply>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEvent {
    Probe(HopProbe),
    /// A reverse-DNS name for a hop address.
    Name {
        address: IpAddr,
        name: String,
    },
}

/// One address seen at a hop (several with load balancing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HopAddress {
    pub address: IpAddr,
    pub name: Option<String>,
    pub replies: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HopStats {
    pub ttl: u8,
    /// Most frequent first.
    pub addresses: Vec<HopAddress>,
    pub sent: u32,
    pub received: u32,
    pub loss_percent: f64,
    pub last_ms: Option<f64>,
    pub best_ms: Option<f64>,
    pub avg_ms: Option<f64>,
    pub worst_ms: Option<f64>,
    /// Standard deviation of the round-trip times.
    pub stdev_ms: Option<f64>,
    /// The last unreachable error at this hop, if any.
    pub unreachable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceResult {
    pub version: u32,
    pub method: TraceMethod,
    /// Why ICMP wasn't used, when it wasn't.
    pub fallback_reason: Option<String>,
    pub max_hops: u8,
    pub rounds: u32,
    /// The hop count of the destination, once it answered.
    pub destination_ttl: Option<u8>,
    /// RTT resolution when coarser than 0.1 ms (Windows: 1 ms).
    pub resolution_ms: Option<f64>,
    pub hops: Vec<HopStats>,
    pub stopped: bool,
}

pub type OnTraceEvent = Box<dyn FnMut(&TraceEvent) + Send>;

/// Per-hop aggregation.
#[derive(Default)]
struct Hop {
    sent: u32,
    received: u32,
    addresses: Vec<(IpAddr, u32)>,
    last: Option<f64>,
    best: Option<f64>,
    worst: Option<f64>,
    sum: f64,
    sum_sq: f64,
    rtts: u32,
    unreachable: Option<String>,
}

impl Hop {
    fn add(&mut self, p: &HopProbe) {
        self.sent += 1;
        if let Some(from) = p.from {
            self.received += 1;
            match self.addresses.iter_mut().find(|(a, _)| *a == from) {
                Some((_, n)) => *n += 1,
                None => self.addresses.push((from, 1)),
            }
        }
        if let Some(rtt) = p.rtt_ms {
            self.last = Some(rtt);
            self.best = Some(self.best.map_or(rtt, |b| b.min(rtt)));
            self.worst = Some(self.worst.map_or(rtt, |w| w.max(rtt)));
            self.sum += rtt;
            self.sum_sq += rtt * rtt;
            self.rtts += 1;
        }
        if let Some(HopReply::Unreachable { detail }) = &p.reply {
            self.unreachable = Some(detail.clone());
        }
    }

    fn stats(&self, ttl: u8, names: &HashMap<IpAddr, String>) -> HopStats {
        let mut addresses: Vec<HopAddress> = self
            .addresses
            .iter()
            .map(|(a, n)| HopAddress {
                address: *a,
                name: names.get(a).cloned(),
                replies: *n,
            })
            .collect();
        addresses.sort_by_key(|a| std::cmp::Reverse(a.replies));
        let n = f64::from(self.rtts);
        let avg = (self.rtts > 0).then(|| self.sum / n);
        HopStats {
            ttl,
            addresses,
            sent: self.sent,
            received: self.received,
            loss_percent: if self.sent == 0 {
                0.0
            } else {
                100.0 * f64::from(self.sent - self.received) / f64::from(self.sent)
            },
            last_ms: self.last,
            best_ms: self.best,
            avg_ms: avg,
            worst_ms: self.worst,
            stdev_ms: avg
                .filter(|_| self.rtts > 1)
                .map(|avg| (self.sum_sq / n - avg * avg).max(0.0).sqrt()),
            unreachable: self.unreachable.clone(),
        }
    }
}

/// Trace the route to `target`, bound to the Wi-Fi interface or along the
/// system route. `ptr_server`: the DNS server for hop names (`None`: no
/// names). Cancelling returns what was measured (`stopped`).
pub async fn traceroute(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    config: TraceConfig,
    ptr_server: Option<SocketAddr>,
    cancel: &Cancel,
    mut on_event: OnTraceEvent,
) -> TestResult<TraceResult> {
    config.validate()?;
    cancel.check()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<HopProbe>();
    let prober = {
        let binding = binding.cloned();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            probe_rounds(binding.as_ref(), target, config, &cancel, tx)
        })
    };

    let mut hops: HashMap<u8, Hop> = HashMap::new();
    let mut names: HashMap<IpAddr, String> = HashMap::new();
    let mut asked: Vec<IpAddr> = Vec::new();
    let mut lookups: JoinSet<(IpAddr, Option<String>)> = JoinSet::new();
    let mut destination: Option<u8> = None;
    let mut rounds_seen = 0u32;
    loop {
        tokio::select! {
            probe = rx.recv() => {
                let Some(probe) = probe else { break };
                if probe.reply == Some(HopReply::Reached) {
                    destination = Some(destination.map_or(probe.ttl, |d| d.min(probe.ttl)));
                }
                rounds_seen = rounds_seen.max(probe.round + 1);
                hops.entry(probe.ttl).or_default().add(&probe);
                if let (Some(server), Some(from)) = (ptr_server, probe.from) {
                    if !asked.contains(&from) && asked.len() < 256 {
                        asked.push(from);
                        lookups.spawn(reverse_name(server, from));
                    }
                }
                on_event(&TraceEvent::Probe(probe));
            }
            Some(Ok((address, name))) = lookups.join_next(), if !lookups.is_empty() => {
                if let Some(name) = name {
                    on_event(&TraceEvent::Name { address, name: name.clone() });
                    names.insert(address, name);
                }
            }
        }
    }
    let (method, fallback_reason, resolution_ms, outcome) = prober
        .await
        .map_err(|e| WifiError::Backend(format!("traceroute task failed: {e}")))?;
    let stopped = match outcome {
        Ok(()) => false,
        Err(IcmpError::Test(TestError::Cancelled)) => true,
        Err(IcmpError::Test(e)) => return Err(e),
        Err(IcmpError::Unavailable(reason)) => {
            return Err(
                WifiError::Unsupported(format!("traceroute isn't possible here: {reason}")).into(),
            )
        }
    };
    // Give the names still being looked up a moment, unless stopped.
    if !stopped {
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(joined) = lookups.join_next().await {
                if let Ok((address, Some(name))) = joined {
                    on_event(&TraceEvent::Name {
                        address,
                        name: name.clone(),
                    });
                    names.insert(address, name);
                }
            }
        })
        .await;
    }
    let last = destination.unwrap_or(config.max_hops);
    let empty = Hop::default();
    Ok(TraceResult {
        version: TRACE_RESULTS_VERSION,
        method,
        fallback_reason,
        max_hops: config.max_hops,
        rounds: rounds_seen,
        destination_ttl: destination,
        resolution_ms,
        hops: (1..=last)
            .filter(|ttl| hops.contains_key(ttl) || destination.is_some())
            .map(|ttl| hops.get(&ttl).unwrap_or(&empty).stats(ttl, &names))
            .collect(),
        stopped,
    })
}

/// The first PTR name for `address`, without the trailing dot.
async fn reverse_name(server: SocketAddr, address: IpAddr) -> (IpAddr, Option<String>) {
    let query = dns::DnsQuery {
        name: address.to_string(),
        record_type: "PTR".into(),
        server,
        transport: dns::DnsTransport::Udp,
        timeout: Duration::from_secs(2),
    };
    let name = dns::lookup(None, &query, &Cancel::never())
        .await
        .ok()
        .and_then(|r| {
            r.records
                .into_iter()
                .find(|rec| rec.section == dns::DnsSection::Answer && rec.record_type == "PTR")
                .map(|rec| rec.data.trim_end_matches('.').to_owned())
        });
    (address, name)
}

type ProberOutcome = (
    TraceMethod,
    Option<String>,
    Option<f64>,
    Result<(), IcmpError>,
);

/// The blocking side: rounds of probes until done or cancelled.
fn probe_rounds(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    config: TraceConfig,
    cancel: &Cancel,
    tx: mpsc::UnboundedSender<HopProbe>,
) -> ProberOutcome {
    #[cfg(target_os = "linux")]
    {
        let first = linux::Prober::new(binding, target, config.method);
        let (mut prober, method, fallback) = match first {
            Ok(p) => (p, config.method, None),
            Err(IcmpError::Unavailable(reason)) if config.method == TraceMethod::Icmp => {
                match linux::Prober::new(binding, target, TraceMethod::Udp) {
                    Ok(p) => (p, TraceMethod::Udp, Some(reason)),
                    Err(e) => return (TraceMethod::Udp, Some(reason), None, Err(e)),
                }
            }
            Err(e) => return (config.method, None, None, Err(e)),
        };
        let outcome = rounds(config, cancel, &tx, |round, limit, emit| {
            prober.round(round, limit, config.timeout, cancel, emit)
        });
        (method, fallback, None, outcome)
    }
    #[cfg(windows)]
    {
        let outcome = rounds(config, cancel, &tx, |round, limit, emit| {
            super::windows::trace_round(binding, target, round, limit, config.timeout, cancel, emit)
        });
        (
            TraceMethod::Icmp,
            None,
            Some(super::windows::ICMP_RESOLUTION_MS),
            outcome,
        )
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (binding, target, config, cancel, tx);
        (
            TraceMethod::Icmp,
            None,
            None,
            Err(IcmpError::Unavailable("not implemented on this OS".into())),
        )
    }
}

/// Run rounds, probing up to the destination once it is known.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn rounds(
    config: TraceConfig,
    cancel: &Cancel,
    tx: &mpsc::UnboundedSender<HopProbe>,
    mut round_fn: impl FnMut(u32, u8, &mut dyn FnMut(HopProbe)) -> Result<(), IcmpError>,
) -> Result<(), IcmpError> {
    let mut limit = config.max_hops;
    let mut round = 0u32;
    while config.rounds.is_none_or(|n| round < n) {
        cancel.check()?;
        let start = std::time::Instant::now();
        let mut reached: Option<u8> = None;
        round_fn(round, limit, &mut |p: HopProbe| {
            if p.reply == Some(HopReply::Reached) {
                reached = Some(reached.map_or(p.ttl, |r| r.min(p.ttl)));
            }
            let _ = tx.send(p);
        })?;
        if let Some(r) = reached {
            limit = limit.min(r);
        }
        round += 1;
        if config.rounds.is_none_or(|n| round < n) {
            cancel.sleep_blocking(config.interval.saturating_sub(start.elapsed()))?;
        }
    }
    Ok(())
}

/// The probe sequence number: unique per (round, TTL) for 1,024 rounds,
/// which is far longer than any reply can arrive late.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn sequence(round: u32, ttl: u8) -> u16 {
    ((round % 1024) * 64 + u32::from(ttl - 1)) as u16
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ttl_of(seq: u16) -> u8 {
    (seq % 64) as u8 + 1
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::io::ErrorKind;
    use std::mem::{size_of, MaybeUninit};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    use socket2::{Domain, Protocol, SockAddr, Socket, Type};

    use super::{sequence, ttl_of, HopProbe, HopReply, TraceMethod, UDP_BASE_PORT};
    use crate::nettools::ping::linux::{bind_to_device, echo_types, ping_socket};
    use crate::nettools::ping::IcmpError;
    use crate::nettools::{Cancel, WifiBinding};
    use crate::WifiError;

    pub(super) struct Prober {
        socket: Socket,
        method: TraceMethod,
        target: IpAddr,
    }

    fn os_error(what: &str) -> IcmpError {
        WifiError::Backend(format!("{what}: {}", std::io::Error::last_os_error())).into()
    }

    fn set_int(
        socket: &Socket,
        level: i32,
        name: i32,
        value: i32,
        what: &str,
    ) -> Result<(), IcmpError> {
        // SAFETY: a valid socket fd and a c_int option value.
        let rc = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                level,
                name,
                (&value as *const i32).cast(),
                size_of::<i32>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(os_error(what))
        }
    }

    impl Prober {
        pub(super) fn new(
            binding: Option<&WifiBinding>,
            target: IpAddr,
            method: TraceMethod,
        ) -> Result<Self, IcmpError> {
            let socket = match method {
                TraceMethod::Icmp => ping_socket(target)?,
                TraceMethod::Udp => {
                    let domain = if target.is_ipv4() {
                        Domain::IPV4
                    } else {
                        Domain::IPV6
                    };
                    Socket::new(domain, Type::DGRAM, Some(Protocol::UDP)).map_err(|e| {
                        WifiError::Backend(format!("cannot create a UDP socket: {e}"))
                    })?
                }
            };
            if let Some(binding) = binding {
                bind_to_device(&socket, &binding.iface)?;
            }
            match target {
                IpAddr::V4(_) => set_int(&socket, libc::SOL_IP, libc::IP_RECVERR, 1, "IP_RECVERR")?,
                IpAddr::V6(_) => set_int(
                    &socket,
                    libc::SOL_IPV6,
                    libc::IPV6_RECVERR,
                    1,
                    "IPV6_RECVERR",
                )?,
            }
            socket
                .set_nonblocking(true)
                .map_err(|e| WifiError::Backend(format!("cannot configure the socket: {e}")))?;
            Ok(Self {
                socket,
                method,
                target,
            })
        }

        fn set_ttl(&self, ttl: u8) -> Result<(), IcmpError> {
            match self.target {
                IpAddr::V4(_) => set_int(
                    &self.socket,
                    libc::SOL_IP,
                    libc::IP_TTL,
                    ttl.into(),
                    "IP_TTL",
                ),
                IpAddr::V6(_) => set_int(
                    &self.socket,
                    libc::SOL_IPV6,
                    libc::IPV6_UNICAST_HOPS,
                    ttl.into(),
                    "IPV6_UNICAST_HOPS",
                ),
            }
        }

        fn send(&self, seq: u16, ttl: u8) -> std::io::Result<()> {
            self.set_ttl(ttl)
                .map_err(|_| std::io::Error::last_os_error())?;
            match self.method {
                TraceMethod::Icmp => {
                    let (request, _) = echo_types(self.target);
                    let mut packet = [0u8; 24];
                    packet[0] = request;
                    packet[6..8].copy_from_slice(&seq.to_be_bytes());
                    packet[8..16].copy_from_slice(b"FRESNEL\0");
                    let to = SockAddr::from(SocketAddr::new(self.target, 0));
                    self.socket.send_to(&packet, &to).map(|_| ())
                }
                TraceMethod::Udp => {
                    let mut payload = [0u8; 16];
                    payload[0..2].copy_from_slice(&seq.to_be_bytes());
                    payload[2..10].copy_from_slice(b"FRESNEL\0");
                    let port = UDP_BASE_PORT + u16::from(ttl) - 1;
                    let to = SockAddr::from(SocketAddr::new(self.target, port));
                    self.socket.send_to(&payload, &to).map(|_| ())
                }
            }
        }

        /// Our sequence number from the start of a probe (as sent, or as
        /// quoted back in an ICMP error).
        fn seq_of(&self, data: &[u8]) -> Option<u16> {
            match self.method {
                TraceMethod::Icmp => {
                    (data.len() >= 8).then(|| u16::from_be_bytes([data[6], data[7]]))
                }
                TraceMethod::Udp => {
                    (data.len() >= 2).then(|| u16::from_be_bytes([data[0], data[1]]))
                }
            }
        }

        pub(super) fn round(
            &mut self,
            round: u32,
            limit: u8,
            timeout: Duration,
            cancel: &Cancel,
            emit: &mut dyn FnMut(HopProbe),
        ) -> Result<(), IcmpError> {
            // Drain anything left from the previous round (late answers).
            while self.read_error().is_some() {}
            while self.read_reply().is_some() {}
            let mut pending: HashMap<u16, Instant> = HashMap::new();
            for ttl in 1..=limit {
                let seq = sequence(round, ttl);
                let sent_at = Instant::now();
                match self.send(seq, ttl) {
                    Ok(()) => {
                        pending.insert(seq, sent_at);
                    }
                    // The local routing table says no: every TTL would fail.
                    Err(e)
                        if matches!(
                            e.raw_os_error(),
                            Some(libc::EHOSTUNREACH | libc::ENETUNREACH)
                        ) =>
                    {
                        return Err(WifiError::Backend(format!(
                            "there is no route to {}: {e}",
                            self.target
                        ))
                        .with_hint(
                            "This computer has no route for that address family or network \
                             (e.g. no IPv6 connectivity).",
                        )
                        .into())
                    }
                    Err(e) => {
                        return Err(
                            WifiError::Backend(format!("sending a probe failed: {e}")).into()
                        )
                    }
                }
            }
            let deadline = Instant::now() + timeout;
            while !pending.is_empty() {
                cancel.check()?;
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                let mut pfd = libc::pollfd {
                    fd: self.socket.as_raw_fd(),
                    events: libc::POLLIN | libc::POLLERR,
                    revents: 0,
                };
                let wait_ms = left.min(Duration::from_millis(100)).as_millis().max(1) as i32;
                // SAFETY: one valid pollfd.
                unsafe { libc::poll(&mut pfd, 1, wait_ms) };
                let now = Instant::now();
                let mut answered =
                    |seq: u16,
                     from: Option<IpAddr>,
                     reply: HopReply,
                     emit: &mut dyn FnMut(HopProbe)| {
                        if let Some(sent_at) = pending.remove(&seq) {
                            emit(HopProbe {
                                round,
                                ttl: ttl_of(seq),
                                from,
                                rtt_ms: Some((now - sent_at).as_secs_f64() * 1000.0),
                                reply: Some(reply),
                            });
                        }
                    };
                while let Some((seq, from, reply)) = self.read_error() {
                    answered(seq, from, reply, emit);
                }
                while let Some(seq) = self.read_reply() {
                    answered(seq, Some(self.target), HopReply::Reached, emit);
                }
            }
            let mut silent: Vec<u16> = pending.into_keys().collect();
            silent.sort_unstable();
            for seq in silent {
                emit(HopProbe {
                    round,
                    ttl: ttl_of(seq),
                    from: None,
                    rtt_ms: None,
                    reply: None,
                });
            }
            Ok(())
        }

        /// An echo reply (ICMP method only): its sequence number.
        fn read_reply(&self) -> Option<u16> {
            let mut buf = [MaybeUninit::<u8>::uninit(); 512];
            loop {
                match self.socket.recv(&mut buf) {
                    Ok(n) => {
                        // SAFETY: recv initialised the first n bytes.
                        let data: Vec<u8> = buf[..n.min(8)]
                            .iter()
                            .map(|b| unsafe { b.assume_init() })
                            .collect();
                        let (_, reply) = echo_types(self.target);
                        if self.method == TraceMethod::Icmp && data.len() >= 8 && data[0] == reply {
                            return self.seq_of(&data);
                        }
                        // UDP data or something else: ignore.
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    // WouldBlock, or a pending socket error (read via the
                    // error queue instead).
                    Err(_) => return None,
                }
            }
        }

        /// One ICMP error from the error queue: (our sequence number, the
        /// address that sent it, what it means).
        fn read_error(&self) -> Option<(u16, Option<IpAddr>, HopReply)> {
            let mut data = [0u8; 512];
            let mut control = [0u64; 64];
            let mut name: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            let mut iov = libc::iovec {
                iov_base: data.as_mut_ptr().cast(),
                iov_len: data.len(),
            };
            // SAFETY: zeroed msghdr, then every pointer set to live buffers.
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_name = (&mut name as *mut libc::sockaddr_storage).cast();
            msg.msg_namelen = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = size_of::<[u64; 64]>() as _;
            // SAFETY: msg points at buffers alive for the call.
            let n = unsafe {
                libc::recvmsg(
                    self.socket.as_raw_fd(),
                    &mut msg,
                    libc::MSG_ERRQUEUE | libc::MSG_DONTWAIT,
                )
            };
            if n < 0 {
                return None;
            }
            let seq = self.seq_of(&data[..n as usize]);
            // SAFETY: walking the control messages recvmsg filled in.
            let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
            while let Some(c) = unsafe { cmsg.as_ref() } {
                let is_err = (c.cmsg_level == libc::SOL_IP && c.cmsg_type == libc::IP_RECVERR)
                    || (c.cmsg_level == libc::SOL_IPV6 && c.cmsg_type == libc::IPV6_RECVERR);
                if is_err {
                    let ee_ptr = unsafe { libc::CMSG_DATA(c) } as *const libc::sock_extended_err;
                    let ee = unsafe { ee_ptr.read_unaligned() };
                    let from = unsafe { offender(ee_ptr) };
                    let reply = self.classify(&ee, from);
                    // A probe we no longer know (no sequence): read on.
                    return match (seq, reply) {
                        (Some(seq), Some(reply)) => Some((seq, from, reply)),
                        _ => self.read_error(),
                    };
                }
                cmsg = unsafe { libc::CMSG_NXTHDR(&msg, c) };
            }
            // An error queue entry without the error message: skip it.
            self.read_error()
        }

        fn classify(&self, ee: &libc::sock_extended_err, from: Option<IpAddr>) -> Option<HopReply> {
            let (time_exceeded, unreachable, port_unreachable) = match ee.ee_origin {
                libc::SO_EE_ORIGIN_ICMP => (11, 3, 3),
                libc::SO_EE_ORIGIN_ICMP6 => (3, 1, 4),
                // A local error (e.g. the interface went down).
                libc::SO_EE_ORIGIN_LOCAL => {
                    return Some(HopReply::Unreachable {
                        detail: std::io::Error::from_raw_os_error(ee.ee_errno as i32).to_string(),
                    })
                }
                _ => return None,
            };
            if ee.ee_type == time_exceeded {
                Some(HopReply::TimeExceeded)
            } else if ee.ee_type == unreachable {
                if self.method == TraceMethod::Udp
                    && ee.ee_code == port_unreachable
                    && from == Some(self.target)
                {
                    Some(HopReply::Reached)
                } else {
                    Some(HopReply::Unreachable {
                        detail: unreachable_text(
                            ee.ee_origin == libc::SO_EE_ORIGIN_ICMP6,
                            ee.ee_code,
                        ),
                    })
                }
            } else {
                None
            }
        }
    }

    /// SAFETY: `ee` points at a sock_extended_err from IP(V6)_RECVERR,
    /// followed by the offender's address.
    unsafe fn offender(ee: *const libc::sock_extended_err) -> Option<IpAddr> {
        let sa = unsafe { libc::SO_EE_OFFENDER(ee) } as *const libc::sockaddr;
        match i32::from(unsafe { (*sa).sa_family }) {
            libc::AF_INET => {
                let sin = unsafe { (sa as *const libc::sockaddr_in).read_unaligned() };
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                    sin.sin_addr.s_addr,
                ))))
            }
            libc::AF_INET6 => {
                let sin6 = unsafe { (sa as *const libc::sockaddr_in6).read_unaligned() };
                Some(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }

    fn unreachable_text(v6: bool, code: u8) -> String {
        let text = if v6 {
            match code {
                0 => "no route to destination",
                1 => "administratively prohibited",
                3 => "address unreachable",
                4 => "port unreachable",
                5 => "source address failed policy",
                6 => "reject route",
                _ => "destination unreachable",
            }
        } else {
            match code {
                0 => "network unreachable",
                1 => "host unreachable",
                2 => "protocol unreachable",
                3 => "port unreachable",
                4 => "fragmentation needed",
                9 | 10 | 13 => "administratively prohibited",
                _ => "destination unreachable",
            }
        };
        format!("{text} (code {code})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_numbers_round_trip() {
        for round in [0, 1, 1023, 1024, 5000] {
            for ttl in [1, 2, 30, 64] {
                assert_eq!(ttl_of(sequence(round, ttl)), ttl);
            }
        }
        assert_ne!(sequence(0, 1), sequence(1, 1));
    }

    #[test]
    fn hop_statistics() {
        let mut hop = Hop::default();
        let probe = |rtt: Option<f64>, from: Option<&str>| HopProbe {
            round: 0,
            ttl: 3,
            from: from.map(|f| f.parse().unwrap()),
            rtt_ms: rtt,
            reply: from.map(|_| HopReply::TimeExceeded),
        };
        hop.add(&probe(Some(10.0), Some("10.0.0.1")));
        hop.add(&probe(None, None));
        hop.add(&probe(Some(20.0), Some("10.0.0.2")));
        hop.add(&probe(Some(30.0), Some("10.0.0.2")));
        let mut names = HashMap::new();
        names.insert("10.0.0.2".parse().unwrap(), "core.isp.example".to_string());
        let s = hop.stats(3, &names);
        assert_eq!((s.sent, s.received, s.loss_percent), (4, 3, 25.0));
        assert_eq!(
            (s.best_ms, s.avg_ms, s.worst_ms, s.last_ms),
            (Some(10.0), Some(20.0), Some(30.0), Some(30.0))
        );
        assert!((s.stdev_ms.unwrap() - 8.165).abs() < 0.01);
        assert_eq!(s.addresses[0].name.as_deref(), Some("core.isp.example"));
        assert_eq!(s.addresses[0].replies, 2);
    }

    /// Loopback is one hop away: the first probe reaches it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn loopback_is_reached_at_hop_one() {
        let config = TraceConfig {
            max_hops: 4,
            timeout: Duration::from_millis(500),
            rounds: Some(2),
            interval: Duration::from_millis(10),
            method: TraceMethod::Icmp,
        };
        let r = traceroute(
            None,
            "127.0.0.1".parse().unwrap(),
            config,
            None,
            &Cancel::never(),
            Box::new(|_| {}),
        )
        .await
        .unwrap();
        assert_eq!(r.destination_ttl, Some(1), "{r:?}");
        assert_eq!(r.hops.len(), 1);
        assert_eq!(r.hops[0].received, 2, "{r:?}");
        assert_eq!(r.rounds, 2);
    }

    /// A real multi-hop trace (opt-in: contacts the network).
    /// `FRESNEL_TRACE_TARGET=1.1.1.1 cargo test -p fresnel-core real_trace -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_trace() {
        let target: IpAddr = std::env::var("FRESNEL_TRACE_TARGET")
            .expect("set FRESNEL_TRACE_TARGET")
            .parse()
            .unwrap();
        let ptr = dns::system_servers()
            .await
            .unwrap()
            .first()
            .map(|s| SocketAddr::new(s.address, dns::DNS_PORT));
        let method = match std::env::var("FRESNEL_TRACE_METHOD").as_deref() {
            Ok("udp") => TraceMethod::Udp,
            _ => TraceMethod::Icmp,
        };
        let r = traceroute(
            None,
            target,
            TraceConfig {
                method,
                ..TraceConfig::default()
            },
            ptr,
            &Cancel::never(),
            Box::new(|_| {}),
        )
        .await
        .unwrap();
        for h in &r.hops {
            let who = h
                .addresses
                .first()
                .map(|a| format!("{} {}", a.address, a.name.clone().unwrap_or_default()));
            println!(
                "{:>2} {:<50} {}/{} avg {:?}",
                h.ttl,
                who.unwrap_or("*".into()),
                h.received,
                h.sent,
                h.avg_ms
            );
        }
        println!(
            "{:?} fallback {:?} dest {:?}",
            r.method, r.fallback_reason, r.destination_ttl
        );
        assert!(r.destination_ttl.is_some());
    }
}
