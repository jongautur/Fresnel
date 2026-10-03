//! Running a tool: check the settings, resolve the target, pick the
//! interface (the system route, or bound to a Wi-Fi adapter), stream live
//! events, then store the run — failed and stopped runs too, so the history
//! shows what happened. Only mistakes in what the user typed are returned
//! as errors without a stored run.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{NewToolRun, ToolKind, ToolRun, ToolRunStatus};
use crate::database::Database;
use crate::nettools::dns::{self, DnsResult, DnsTransport};
use crate::nettools::iperf3::{self, Iperf3Config, Iperf3Direction, Iperf3Interval, Iperf3Result};
use crate::nettools::portcheck::{self, PortCheckResult, PortOutcome};
use crate::nettools::settings::Iperf3Directions;
use crate::nettools::traceroute::{self, TraceConfig, TraceEvent, TraceMethod, TraceResult};
use crate::nettools::{
    check_route, egress, ping, resolve, wifi_binding, Cancel, IpFamily, PingConfig, PingResult,
    ProbeMethod, ProbeOutcome, ResolvedTarget, TestError, WifiBinding,
};
use crate::survey::models::LinkSnapshot;
use crate::wifi::models::AdapterId;
use crate::wifi::scanner::Scanner;
use crate::{Result, WifiError};

/// Shortest gap between pings and between MTR rounds on the Tools page.
pub const MIN_INTERVAL_MS: u64 = 200;

/// Which interface a run uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Via {
    /// Whatever the OS routes the target through, like the system's own
    /// tools.
    System,
    /// Bound to this Wi-Fi adapter's interface; refused if the route to the
    /// target would leave through another interface.
    Wifi { adapter_id: AdapterId },
}

/// Live output, in order: `started`, then the tool's own events.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "event",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolEvent {
    Started {
        target: Option<ResolvedTarget>,
        route_iface: Option<String>,
        adapter_id: Option<AdapterId>,
        link: Option<LinkSnapshot>,
    },
    Probe {
        seq: u32,
        outcome: ProbeOutcome,
    },
    Trace {
        #[serde(flatten)]
        trace: TraceEvent,
    },
    Port {
        #[serde(flatten)]
        port: PortOutcome,
    },
    /// An iperf3 test (one direction) is starting.
    Iperf3Test {
        direction: Iperf3Direction,
    },
    Interval {
        direction: Iperf3Direction,
        #[serde(flatten)]
        interval: Iperf3Interval,
    },
}

pub type Events = Arc<dyn Fn(ToolEvent) + Send + Sync>;

/// What a run needs from the app.
pub struct ToolContext<'a> {
    pub scanner: &'a Scanner,
    pub db: Arc<Database>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingParams {
    pub target: String,
    #[serde(default)]
    pub family: IpFamily,
    pub via: Via,
    /// `None`: until stopped.
    pub count: Option<u32>,
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub payload_len: u16,
    pub method: ProbeMethod,
    /// For TCP-connect timing (and the fallback when ICMP isn't allowed).
    pub tcp_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceParams {
    pub target: String,
    #[serde(default)]
    pub family: IpFamily,
    pub via: Via,
    pub max_hops: u8,
    pub timeout_ms: u64,
    /// `None`: until stopped (MTR).
    pub rounds: Option<u32>,
    pub interval_ms: u64,
    pub method: TraceMethod,
    /// Reverse-DNS names for the hops.
    pub names: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsParams {
    pub name: String,
    pub record_type: String,
    /// "system" (the first DNS server the OS is configured with) or an
    /// address (`1.1.1.1`, `[2606:4700::1111]:53`). Several: compare.
    pub servers: Vec<String>,
    pub transport: DnsTransport,
    pub timeout_ms: u64,
    pub via: Via,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortCheckParams {
    pub target: String,
    #[serde(default)]
    pub family: IpFamily,
    pub via: Via,
    /// `22, 80, 443, 8000-8100`.
    pub ports: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iperf3Params {
    pub server: String,
    pub port: u16,
    #[serde(default)]
    pub family: IpFamily,
    pub via: Via,
    pub streams: u8,
    pub duration_s: u64,
    pub omit_s: u64,
    pub directions: Iperf3Directions,
}

/// Results of a DNS run: one lookup per server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsRunResults {
    pub version: u32,
    pub lookups: Vec<DnsLookup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsLookup {
    /// `ip:port`, or what the user typed if it didn't parse.
    pub server: String,
    /// "system (/etc/resolv.conf)", "system (Wi-Fi)", or "custom".
    pub source: String,
    pub result: Option<DnsResult>,
    pub error: Option<String>,
    pub error_hint: Option<String>,
}

/// Results of an iperf3 run: one test per direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iperf3RunResults {
    pub version: u32,
    pub tests: Vec<Iperf3Test>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iperf3Test {
    pub direction: Iperf3Direction,
    pub result: Option<Iperf3Result>,
    /// What was measured live before a failure or stop.
    #[serde(default)]
    pub intervals: Vec<Iperf3Interval>,
    pub error: Option<String>,
    pub error_hint: Option<String>,
    pub stopped: bool,
}

/// Where a run's traffic goes and the Wi-Fi link it describes.
struct Session {
    binding: Option<WifiBinding>,
    route_iface: Option<String>,
    /// The adapter whose link is snapshotted (bound, or the Wi-Fi adapter
    /// the system route happens to use).
    link_adapter: Option<AdapterId>,
    bound_adapter: Option<AdapterId>,
    link: Option<LinkSnapshot>,
}

async fn open_session(scanner: &Scanner, via: &Via, target: IpAddr) -> Result<Session> {
    match via {
        Via::System => {
            let route = egress(target).await?;
            let link_adapter = match &route.iface {
                Some(iface) => wifi_adapter_for(scanner, iface).await,
                None => None,
            };
            let link = match &link_adapter {
                Some(id) => snapshot(scanner, id).await,
                None => None,
            };
            Ok(Session {
                binding: None,
                route_iface: route.iface,
                link_adapter,
                bound_adapter: None,
                link,
            })
        }
        Via::Wifi { adapter_id } => {
            let connection = scanner
                .current_connection(adapter_id)
                .await?
                .ok_or_else(|| {
                    WifiError::AdapterUnavailable {
                        id: adapter_id.to_string(),
                        reason: "not connected to a Wi-Fi network".into(),
                    }
                    .with_hint("Connect to a network, or run the tool over the system route.")
                })?;
            let binding = wifi_binding(&connection).await?;
            check_route(&binding, target).await.map_err(|e| {
                let hint = e.hint().map(str::to_owned);
                e.with_hint(format!(
                    "{}Or choose “System route” to test whatever path the computer uses.",
                    hint.map(|h| format!("{h} ")).unwrap_or_default()
                ))
            })?;
            Ok(Session {
                route_iface: Some(binding.iface.clone()),
                binding: Some(binding),
                link_adapter: Some(adapter_id.clone()),
                bound_adapter: Some(adapter_id.clone()),
                link: Some(LinkSnapshot::from_connection(Some(&connection))),
            })
        }
    }
}

/// The Wi-Fi adapter behind an interface name, if the provider knows it.
async fn wifi_adapter_for(scanner: &Scanner, iface: &str) -> Option<AdapterId> {
    scanner
        .registry()
        .list_adapters()
        .await
        .adapters
        .into_iter()
        .find(|a| a.interface_name.as_deref() == Some(iface))
        .map(|a| a.id)
}

async fn snapshot(scanner: &Scanner, id: &AdapterId) -> Option<LinkSnapshot> {
    match scanner.current_connection(id).await {
        Ok(c) => Some(LinkSnapshot::from_connection(c.as_ref())),
        Err(e) => {
            warn!(error = %e, "cannot read the Wi-Fi link for a tool run");
            None
        }
    }
}

/// How a run ended.
struct Outcome {
    status: ToolRunStatus,
    results: Option<serde_json::Value>,
    summary: Option<String>,
    error: Option<WifiError>,
}

impl Outcome {
    fn failed(error: WifiError) -> Self {
        Self {
            status: ToolRunStatus::Failed,
            results: None,
            summary: None,
            error: Some(error),
        }
    }

    fn from_test_error(e: TestError) -> Self {
        match e {
            TestError::Cancelled => Self {
                status: ToolRunStatus::Stopped,
                results: None,
                summary: Some("stopped before any result".into()),
                error: None,
            },
            TestError::Failed(e) => Self::failed(e),
        }
    }
}

/// Collects what is known about a run as it goes, then stores it.
struct Recorder {
    kind: ToolKind,
    target: String,
    params: serde_json::Value,
    started_at: DateTime<Utc>,
    clock: Instant,
    resolved_ip: Option<String>,
    session: Option<Session>,
}

impl Recorder {
    fn new<P: Serialize>(kind: ToolKind, target: &str, params: &P) -> Result<Self> {
        Ok(Self {
            kind,
            target: target.trim().to_owned(),
            params: serde_json::to_value(params)
                .map_err(|e| WifiError::Backend(format!("cannot record the settings: {e}")))?,
            started_at: Utc::now(),
            clock: Instant::now(),
            resolved_ip: None,
            session: None,
        })
    }

    async fn store(self, ctx: &ToolContext<'_>, outcome: Outcome) -> Result<ToolRun> {
        let duration_ms = self.clock.elapsed().as_millis() as i64;
        let (route_iface, adapter_id, link, link_after) = match self.session {
            Some(s) => {
                let after = match &s.link_adapter {
                    Some(id) => snapshot(ctx.scanner, id).await,
                    None => None,
                };
                (s.route_iface, s.bound_adapter, s.link, after)
            }
            None => (None, None, None, None),
        };
        let roamed = link.as_ref().and_then(|l| l.roamed_to(link_after.as_ref()));
        match &outcome.error {
            Some(e) => {
                warn!(kind = ?self.kind, target = %self.target, error = %e, "tool run failed")
            }
            None => {
                info!(kind = ?self.kind, target = %self.target, status = ?outcome.status, "tool run done")
            }
        }
        let new = NewToolRun {
            kind: self.kind,
            target: self.target,
            resolved_ip: self.resolved_ip,
            params: self.params,
            status: outcome.status,
            started_at: self.started_at,
            duration_ms,
            route_iface,
            adapter_id: adapter_id.map(|a| a.0),
            link,
            link_after,
            roamed,
            summary: outcome.summary,
            results: outcome.results,
            error_hint: outcome
                .error
                .as_ref()
                .and_then(|e| e.hint())
                .map(str::to_owned),
            error: outcome.error.map(|e| e.to_string()),
        };
        let db = ctx.db.clone();
        tokio::task::spawn_blocking(move || db.insert_tool_run(&new))
            .await
            .map_err(|e| WifiError::Backend(format!("database task failed: {e}")))?
    }

    /// Resolve the target and open the session, then announce the start.
    async fn prepare(
        &mut self,
        ctx: &ToolContext<'_>,
        host: &str,
        family: IpFamily,
        via: &Via,
        cancel: &Cancel,
        events: &Events,
    ) -> Prepared {
        let resolved = match resolve(host, family, cancel).await {
            Ok(r) => r,
            Err(TestError::Failed(e)) if e.kind() == "invalid_input" => {
                return Prepared::Invalid(e)
            }
            Err(e) => return Prepared::Failed(Outcome::from_test_error(e)),
        };
        self.resolved_ip = Some(resolved.ip.to_string());
        self.open(ctx, via, resolved.ip, Some(resolved.clone()), events)
            .await
            .map_or_else(Prepared::Failed, |()| Prepared::Ready(resolved.ip))
    }

    async fn open(
        &mut self,
        ctx: &ToolContext<'_>,
        via: &Via,
        ip: IpAddr,
        target: Option<ResolvedTarget>,
        events: &Events,
    ) -> std::result::Result<(), Outcome> {
        let session = open_session(ctx.scanner, via, ip)
            .await
            .map_err(Outcome::failed)?;
        events(ToolEvent::Started {
            target,
            route_iface: session.route_iface.clone(),
            adapter_id: session.bound_adapter.clone(),
            link: session.link.clone(),
        });
        self.session = Some(session);
        Ok(())
    }

    fn binding(&self) -> Option<&WifiBinding> {
        self.session.as_ref().and_then(|s| s.binding.as_ref())
    }
}

enum Prepared {
    Ready(IpAddr),
    /// Store the run as failed (or stopped) with this outcome.
    Failed(Outcome),
    /// A mistake in what the user typed: nothing to store.
    Invalid(WifiError),
}

macro_rules! prepare_or_return {
    ($rec:ident, $ctx:expr, $host:expr, $family:expr, $via:expr, $cancel:expr, $events:expr) => {
        match $rec
            .prepare($ctx, $host, $family, $via, $cancel, $events)
            .await
        {
            Prepared::Ready(ip) => ip,
            Prepared::Failed(outcome) => return $rec.store($ctx, outcome).await,
            Prepared::Invalid(e) => return Err(e),
        }
    };
}

fn ms(v: u64, what: &str, min: u64, max: u64) -> Result<Duration> {
    if !(min..=max).contains(&v) {
        return Err(WifiError::InvalidInput(format!(
            "{what} must be {}–{} s",
            fmt_s(min),
            fmt_s(max)
        )));
    }
    Ok(Duration::from_millis(v))
}

fn fmt_s(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s.fract() == 0.0 {
        format!("{s:.0}")
    } else {
        format!("{s}")
    }
}

fn fmt_ms(v: f64) -> String {
    if v < 10.0 {
        format!("{v:.1} ms")
    } else {
        format!("{v:.0} ms")
    }
}

pub fn fmt_bps(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.2} Gbit/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.0} Mbit/s", bps / 1e6)
    } else {
        format!("{:.0} kbit/s", bps / 1e3)
    }
}

fn to_value<T: Serialize>(v: &T) -> Option<serde_json::Value> {
    serde_json::to_value(v).ok()
}

// ---------------------------------------------------------------- ping

pub fn ping_config(p: &PingParams) -> Result<PingConfig> {
    Ok(PingConfig {
        count: p.count,
        interval: ms(p.interval_ms, "the ping interval", MIN_INTERVAL_MS, 10_000)?,
        timeout: ms(p.timeout_ms, "the ping timeout", 100, 10_000)?,
        tcp_port: match p.tcp_port {
            0 => {
                return Err(WifiError::InvalidInput(
                    "the TCP port must be 1–65535".into(),
                ))
            }
            port => port,
        },
        payload_len: p.payload_len,
    })
}

pub async fn run_ping(
    ctx: &ToolContext<'_>,
    params: PingParams,
    cancel: &Cancel,
    events: Events,
) -> Result<ToolRun> {
    let config = ping_config(&params)?;
    let mut rec = Recorder::new(ToolKind::Ping, &params.target, &params)?;
    let ip = prepare_or_return!(
        rec,
        ctx,
        &params.target,
        params.family,
        &params.via,
        cancel,
        &events
    );
    let live = events.clone();
    let outcome = match ping::ping_live(
        rec.binding(),
        ip,
        config,
        params.method,
        cancel,
        Box::new(move |seq, outcome| {
            live(ToolEvent::Probe {
                seq,
                outcome: outcome.clone(),
            })
        }),
    )
    .await
    {
        Ok(r) => ping_outcome(r, ip),
        Err(e) => Outcome::from_test_error(e),
    };
    rec.store(ctx, outcome).await
}

fn ping_outcome(r: PingResult, ip: IpAddr) -> Outcome {
    let summary = match r.avg_ms {
        Some(avg) => format!(
            "{} avg, {:.0} % loss ({} sent)",
            fmt_ms(avg),
            r.loss_percent,
            r.sent
        ),
        None => format!("no replies ({} sent)", r.sent),
    };
    let error = (r.received == 0 && r.sent > 0).then(|| {
        let hint = match r.method {
            ProbeMethod::TcpConnect => format!(
                "Nothing answered on TCP port {}. Pick a port the host listens on.",
                r.port.unwrap_or_default()
            ),
            ProbeMethod::Icmp => "The host may be off, or a firewall may drop ping (Windows \
                computers block inbound ping by default; some routers don't answer it)."
                .into(),
        };
        WifiError::Timeout(format!("no replies from {ip} ({} sent)", r.sent)).with_hint(hint)
    });
    Outcome {
        status: if r.stopped {
            ToolRunStatus::Stopped
        } else if error.is_some() {
            ToolRunStatus::Failed
        } else {
            ToolRunStatus::Ok
        },
        results: to_value(&r),
        summary: Some(summary),
        error,
    }
}

// ---------------------------------------------------------------- traceroute

pub fn trace_config(p: &TraceParams) -> Result<TraceConfig> {
    Ok(TraceConfig {
        max_hops: p.max_hops,
        timeout: ms(p.timeout_ms, "the hop timeout", 100, 10_000)?,
        rounds: p.rounds,
        interval: ms(p.interval_ms, "the round interval", 0, 60_000)?,
        method: p.method,
    })
}

pub async fn run_traceroute(
    ctx: &ToolContext<'_>,
    params: TraceParams,
    cancel: &Cancel,
    events: Events,
) -> Result<ToolRun> {
    let mut config = trace_config(&params)?;
    if config.rounds.is_none() {
        config.interval = config.interval.max(Duration::from_millis(MIN_INTERVAL_MS));
    }
    let mut rec = Recorder::new(ToolKind::Traceroute, &params.target, &params)?;
    let ip = prepare_or_return!(
        rec,
        ctx,
        &params.target,
        params.family,
        &params.via,
        cancel,
        &events
    );
    let ptr_server = if params.names {
        first_system_server().await
    } else {
        None
    };
    let live = events.clone();
    let outcome = match traceroute::traceroute(
        rec.binding(),
        ip,
        config,
        ptr_server,
        cancel,
        Box::new(move |event| {
            live(ToolEvent::Trace {
                trace: event.clone(),
            })
        }),
    )
    .await
    {
        Ok(r) => trace_outcome(r),
        Err(e) => Outcome::from_test_error(e),
    };
    rec.store(ctx, outcome).await
}

fn trace_outcome(r: TraceResult) -> Outcome {
    let summary = match r.destination_ttl {
        Some(hops) => {
            let avg = r.hops.last().and_then(|h| h.avg_ms);
            format!(
                "reached in {hops} hop{}{}",
                if hops == 1 { "" } else { "s" },
                avg.map(|a| format!(", {}", fmt_ms(a))).unwrap_or_default()
            )
        }
        None => format!("not reached within {} hops", r.max_hops),
    };
    Outcome {
        status: if r.stopped {
            ToolRunStatus::Stopped
        } else {
            ToolRunStatus::Ok
        },
        results: to_value(&r),
        summary: Some(summary),
        error: None,
    }
}

async fn first_system_server() -> Option<SocketAddr> {
    dns::system_servers()
        .await
        .ok()?
        .first()
        .map(|s| SocketAddr::new(s.address, dns::DNS_PORT))
}

// ---------------------------------------------------------------- DNS

pub const MAX_DNS_SERVERS: usize = 5;

pub async fn run_dns(
    ctx: &ToolContext<'_>,
    params: DnsParams,
    cancel: &Cancel,
    events: Events,
) -> Result<ToolRun> {
    dns::record_type(&params.record_type)?;
    dns::query_name(&params.name, dns::record_type(&params.record_type)?)?;
    let timeout = ms(params.timeout_ms, "the DNS timeout", 200, 30_000)?;
    if params.servers.is_empty() || params.servers.len() > MAX_DNS_SERVERS {
        return Err(WifiError::InvalidInput(format!(
            "choose 1–{MAX_DNS_SERVERS} DNS servers"
        )));
    }
    let system = dns::system_servers().await.unwrap_or_default();
    let mut servers: Vec<(SocketAddr, String)> = Vec::new();
    for text in &params.servers {
        let entry = if text.trim().eq_ignore_ascii_case("system") {
            let s = system.first().ok_or_else(|| {
                WifiError::Backend("this computer has no DNS server configured".into())
            })?;
            (
                SocketAddr::new(s.address, dns::DNS_PORT),
                format!("system ({})", s.source),
            )
        } else {
            (dns::parse_server(text)?, "custom".to_owned())
        };
        if !servers.iter().any(|(a, _)| *a == entry.0) {
            servers.push(entry);
        }
    }
    let mut rec = Recorder::new(ToolKind::Dns, &params.name, &params)?;
    if let Err(outcome) = rec
        .open(ctx, &params.via, servers[0].0.ip(), None, &events)
        .await
    {
        return rec.store(ctx, outcome).await;
    }
    let binding = rec.binding().cloned();
    let query = |server: SocketAddr| dns::DnsQuery {
        name: params.name.clone(),
        record_type: params.record_type.clone(),
        server,
        transport: params.transport,
        timeout,
    };
    let lookups = futures::future::join_all(servers.iter().map(|(server, source)| {
        let binding = binding.clone();
        let q = query(*server);
        async move {
            let attempt = async {
                if let Some(b) = &binding {
                    check_route(b, server.ip()).await?;
                }
                dns::lookup(binding.as_ref(), &q, cancel).await
            };
            let (result, error) = match attempt.await {
                Ok(r) => (Some(r), None),
                Err(TestError::Cancelled) => (None, None),
                Err(TestError::Failed(e)) => (None, Some(e)),
            };
            DnsLookup {
                server: server.to_string(),
                source: source.clone(),
                result,
                error_hint: error.as_ref().and_then(|e| e.hint()).map(str::to_owned),
                error: error.map(|e| e.to_string()),
            }
        }
    }))
    .await;
    let answered: Vec<&DnsLookup> = lookups.iter().filter(|l| l.result.is_some()).collect();
    let summary = match (lookups.len(), answered.first()) {
        (_, None) => "no answer".to_owned(),
        (1, Some(l)) => {
            let r = l.result.as_ref().expect("answered");
            let answers: Vec<&str> = r
                .records
                .iter()
                .filter(|rec| rec.section == dns::DnsSection::Answer)
                .map(|rec| rec.data.as_str())
                .collect();
            if answers.is_empty() {
                format!("{}, no records, {}", r.response_code, fmt_ms(r.query_ms))
            } else {
                let mut list = answers[..answers.len().min(3)].join(", ");
                if answers.len() > 3 {
                    list.push_str(&format!(" +{}", answers.len() - 3));
                }
                format!("{list} ({})", fmt_ms(r.query_ms))
            }
        }
        (n, Some(_)) => {
            let fastest = answered
                .iter()
                .min_by(|a, b| {
                    let t = |l: &&DnsLookup| l.result.as_ref().map_or(f64::MAX, |r| r.query_ms);
                    t(a).total_cmp(&t(b))
                })
                .expect("non-empty");
            format!(
                "{}/{n} servers answered, fastest {} ({})",
                answered.len(),
                fastest.server,
                fmt_ms(fastest.result.as_ref().map_or(0.0, |r| r.query_ms))
            )
        }
    };
    let first_error = lookups.iter().find_map(|l| l.error.clone());
    let first_hint = lookups.iter().find_map(|l| l.error_hint.clone());
    let status = if cancel.is_cancelled() {
        ToolRunStatus::Stopped
    } else if answered.is_empty() {
        ToolRunStatus::Failed
    } else {
        ToolRunStatus::Ok
    };
    let error = (status == ToolRunStatus::Failed).then(|| {
        let e = WifiError::Backend(first_error.unwrap_or_else(|| "no server answered".into()));
        match first_hint {
            Some(h) => e.with_hint(h),
            None => e,
        }
    });
    let results = DnsRunResults {
        version: dns::DNS_RESULTS_VERSION,
        lookups,
    };
    rec.store(
        ctx,
        Outcome {
            status,
            results: to_value(&results),
            summary: Some(summary),
            error,
        },
    )
    .await
}

// ---------------------------------------------------------------- port check

pub async fn run_port_check(
    ctx: &ToolContext<'_>,
    params: PortCheckParams,
    cancel: &Cancel,
    events: Events,
) -> Result<ToolRun> {
    let ports = portcheck::parse_ports(&params.ports)?;
    let timeout = ms(params.timeout_ms, "the port timeout", 100, 10_000)?;
    let mut rec = Recorder::new(ToolKind::PortCheck, &params.target, &params)?;
    let ip = prepare_or_return!(
        rec,
        ctx,
        &params.target,
        params.family,
        &params.via,
        cancel,
        &events
    );
    let live = events.clone();
    let outcome = match portcheck::port_check(
        rec.binding(),
        ip,
        &ports,
        timeout,
        cancel,
        Box::new(move |port| live(ToolEvent::Port { port: port.clone() })),
    )
    .await
    {
        Ok(r) => port_outcome(r),
        Err(e) => Outcome::from_test_error(e),
    };
    rec.store(ctx, outcome).await
}

fn port_outcome(r: PortCheckResult) -> Outcome {
    let mut parts = vec![format!("{} open", r.open)];
    if r.closed > 0 {
        parts.push(format!("{} closed", r.closed));
    }
    if r.filtered > 0 {
        parts.push(format!("{} filtered", r.filtered));
    }
    if r.other > 0 {
        parts.push(format!("{} unreachable", r.other));
    }
    let summary = format!(
        "{} of {} port{}",
        parts.join(", "),
        r.requested,
        if r.requested == 1 { "" } else { "s" }
    );
    Outcome {
        status: if r.stopped {
            ToolRunStatus::Stopped
        } else {
            ToolRunStatus::Ok
        },
        results: to_value(&r),
        summary: Some(summary),
        error: None,
    }
}

// ---------------------------------------------------------------- iperf3

pub fn iperf3_check(p: &Iperf3Params) -> Result<()> {
    if p.port == 0 {
        return Err(WifiError::InvalidInput(
            "the iperf3 port must be 1–65535".into(),
        ));
    }
    if !(1..=iperf3::MAX_STREAMS).contains(&p.streams) {
        return Err(WifiError::InvalidInput(format!(
            "iperf3 streams must be 1–{}",
            iperf3::MAX_STREAMS
        )));
    }
    if !(1..=iperf3::MAX_DURATION_S).contains(&p.duration_s) {
        return Err(WifiError::InvalidInput(format!(
            "iperf3 duration must be 1–{} s",
            iperf3::MAX_DURATION_S
        )));
    }
    if p.omit_s > iperf3::MAX_OMIT_S {
        return Err(WifiError::InvalidInput(format!(
            "iperf3 omit must be 0–{} s",
            iperf3::MAX_OMIT_S
        )));
    }
    Ok(())
}

pub async fn run_iperf3(
    ctx: &ToolContext<'_>,
    params: Iperf3Params,
    cancel: &Cancel,
    events: Events,
) -> Result<ToolRun> {
    iperf3_check(&params)?;
    let mut rec = Recorder::new(ToolKind::Iperf3, &params.server, &params)?;
    let ip = prepare_or_return!(
        rec,
        ctx,
        &params.server,
        params.family,
        &params.via,
        cancel,
        &events
    );
    let mut tests = Vec::new();
    for &direction in params.directions.list() {
        if cancel.is_cancelled() {
            break;
        }
        events(ToolEvent::Iperf3Test { direction });
        let config = Iperf3Config {
            server: SocketAddr::new(ip, params.port),
            streams: params.streams,
            duration: Duration::from_secs(params.duration_s),
            omit: Duration::from_secs(params.omit_s),
            direction,
        };
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (live, record) = (events.clone(), seen.clone());
        let r = iperf3::iperf3_tcp_live(
            rec.binding(),
            config,
            cancel,
            Box::new(move |interval| {
                record
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(interval.clone());
                live(ToolEvent::Interval {
                    direction,
                    interval: interval.clone(),
                });
            }),
        )
        .await;
        let intervals = std::mem::take(&mut *seen.lock().unwrap_or_else(|p| p.into_inner()));
        tests.push(match r {
            Ok(result) => Iperf3Test {
                direction,
                result: Some(result),
                intervals: Vec::new(),
                error: None,
                error_hint: None,
                stopped: false,
            },
            Err(TestError::Cancelled) => Iperf3Test {
                direction,
                result: None,
                intervals,
                error: None,
                error_hint: None,
                stopped: true,
            },
            Err(TestError::Failed(e)) => Iperf3Test {
                direction,
                result: None,
                intervals,
                error_hint: e.hint().map(str::to_owned),
                error: Some(e.to_string()),
                stopped: false,
            },
        });
        // A server that is down or refusing won't do better the other way.
        if tests.last().is_some_and(|t| t.error.is_some()) {
            break;
        }
    }
    let summary = tests
        .iter()
        .map(|t| {
            let arrow = match t.direction {
                Iperf3Direction::Upload => "↑",
                Iperf3Direction::Download => "↓",
            };
            match (&t.result, t.stopped) {
                (Some(r), _) => format!("{arrow} {}", fmt_bps(r.bits_per_second)),
                (None, true) => format!("{arrow} stopped"),
                (None, false) => format!("{arrow} failed"),
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let failed = tests.iter().find(|t| t.error.is_some());
    let error = failed.map(|t| {
        let e = WifiError::Backend(t.error.clone().unwrap_or_default());
        match &t.error_hint {
            Some(h) => e.with_hint(h.clone()),
            None => e,
        }
    });
    let status = if tests.iter().any(|t| t.stopped) || cancel.is_cancelled() {
        ToolRunStatus::Stopped
    } else if error.is_some() {
        ToolRunStatus::Failed
    } else {
        ToolRunStatus::Ok
    };
    let results = Iperf3RunResults {
        version: iperf3::IPERF3_RESULTS_VERSION,
        tests,
    };
    rec.store(
        ctx,
        Outcome {
            status,
            results: to_value(&results),
            summary: Some(summary),
            error,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::adapters::AdapterRegistry;
    use crate::nettools::portcheck::PortState;

    fn context(scanner: &Scanner) -> ToolContext<'_> {
        ToolContext {
            scanner,
            db: Arc::new(Database::open_in_memory().unwrap()),
        }
    }

    fn scanner() -> Scanner {
        Scanner::new(Arc::new(AdapterRegistry::new(Vec::new())))
    }

    fn recorder() -> (Events, Arc<Mutex<Vec<serde_json::Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (
            Arc::new(move |e: ToolEvent| {
                sink.lock().unwrap().push(serde_json::to_value(&e).unwrap())
            }),
            seen,
        )
    }

    fn ping_params(target: &str, via: Via) -> PingParams {
        PingParams {
            target: target.into(),
            family: IpFamily::Any,
            via,
            count: Some(2),
            interval_ms: 200,
            timeout_ms: 1000,
            payload_len: 32,
            method: ProbeMethod::TcpConnect,
            tcp_port: 9,
        }
    }

    #[tokio::test]
    async fn ping_over_the_system_route_is_stored_with_events() {
        let scanner = scanner();
        let ctx = context(&scanner);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                drop(s);
            }
        });
        let (events, seen) = recorder();
        let run = run_ping(
            &ctx,
            PingParams {
                tcp_port: port,
                // The listener is IPv4; "localhost" may resolve to ::1 first.
                family: IpFamily::V4,
                ..ping_params("localhost", Via::System)
            },
            &Cancel::never(),
            events,
        )
        .await
        .unwrap();
        assert_eq!(run.status, ToolRunStatus::Ok, "{run:?}");
        assert!(
            run.resolved_ip.as_deref().unwrap().starts_with("127.")
                || run.resolved_ip.as_deref() == Some("::1")
        );
        assert_eq!(run.results.as_ref().unwrap()["received"], 2);
        assert!(run.summary.unwrap().contains("0 % loss"));
        assert_eq!(run.params["tcpPort"], port);
        if cfg!(target_os = "linux") {
            assert_eq!(run.route_iface.as_deref(), Some("lo"));
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["event"], "started");
        assert_eq!(seen[1]["event"], "probe");
        assert_eq!(seen[1]["outcome"]["outcome"], "reply");
        // The UI reads camelCase: a reply without `rttMs` would show as no time.
        assert!(seen[1]["outcome"]["rttMs"].is_number(), "{}", seen[1]);
        assert_eq!(seen.len(), 3);
        let stored = ctx.db.list_tool_runs(Some(ToolKind::Ping), 10).unwrap();
        assert_eq!(stored.len(), 1);
    }

    #[tokio::test]
    async fn typing_mistakes_are_not_stored_but_failures_are() {
        let scanner = scanner();
        let ctx = context(&scanner);
        let (events, _) = recorder();
        let e = run_ping(
            &ctx,
            ping_params("not a host", Via::System),
            &Cancel::never(),
            events.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.kind(), "invalid_input");
        let e = run_ping(
            &ctx,
            PingParams {
                interval_ms: 10,
                ..ping_params("127.0.0.1", Via::System)
            },
            &Cancel::never(),
            events.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.kind(), "invalid_input");
        assert!(ctx.db.list_tool_runs(None, 10).unwrap().is_empty());

        // A Wi-Fi adapter that doesn't exist: a stored, failed run.
        let run = run_ping(
            &ctx,
            ping_params(
                "127.0.0.1",
                Via::Wifi {
                    adapter_id: AdapterId::linux("wlan9"),
                },
            ),
            &Cancel::never(),
            events,
        )
        .await
        .unwrap();
        assert_eq!(run.status, ToolRunStatus::Failed);
        assert!(run.error.is_some());
        assert_eq!(ctx.db.list_tool_runs(None, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn port_check_events_are_flat() {
        let scanner = scanner();
        let ctx = context(&scanner);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                drop(s);
            }
        });
        let (events, seen) = recorder();
        let run = run_port_check(
            &ctx,
            PortCheckParams {
                target: "127.0.0.1".into(),
                family: IpFamily::Any,
                via: Via::System,
                ports: port.to_string(),
                timeout_ms: 1000,
            },
            &Cancel::never(),
            events,
        )
        .await
        .unwrap();
        assert_eq!(run.summary.as_deref(), Some("1 open of 1 port"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1]["event"], "port");
        assert_eq!(seen[1]["port"], port);
        assert_eq!(
            seen[1]["state"],
            serde_json::to_value(PortState::Open).unwrap()
        );
    }

    #[test]
    fn trace_events_are_flat_and_via_parses() {
        let e = ToolEvent::Trace {
            trace: TraceEvent::Name {
                address: "10.0.0.1".parse().unwrap(),
                name: "gw".into(),
            },
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            (v["event"].as_str(), v["type"].as_str(), v["name"].as_str()),
            (Some("trace"), Some("name"), Some("gw"))
        );
        let via: Via = serde_json::from_value(
            serde_json::json!({ "mode": "wifi", "adapterId": "linux:wlan0" }),
        )
        .unwrap();
        assert_eq!(
            via,
            Via::Wifi {
                adapter_id: AdapterId::linux("wlan0")
            }
        );
        let via: Via = serde_json::from_value(serde_json::json!({ "mode": "system" })).unwrap();
        assert_eq!(via, Via::System);
    }
}
