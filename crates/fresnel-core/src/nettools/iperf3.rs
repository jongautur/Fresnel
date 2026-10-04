//! Built-in iperf3 client, TCP only, compatible with standard iperf3 3.x
//! servers (`iperf3 -s`). The user always names the server; Fresnel never
//! picks or contacts one by itself.
//!
//! Protocol (iperf3 `iperf_api.h`, `iperf_client_api.c`): the client opens a
//! control connection and sends a 37-byte cookie (36 characters + NUL). The
//! server drives the test with one signed state byte at a time; JSON goes
//! as a 4-byte big-endian length then the text.
//!
//! ```text
//! server → PARAM_EXCHANGE      client → parameters JSON
//! server → CREATE_STREAMS      client opens N data connections, each sends the cookie
//! server → TEST_START, TEST_RUNNING
//!          data flows for omit + time seconds (client → server, or reverse)
//! client → TEST_END
//! server → EXCHANGE_RESULTS    client → its results JSON; server → its results JSON
//! server → DISPLAY_RESULTS     client → IPERF_DONE
//! ```
//!
//! ACCESS_DENIED (-1) at any point means the server is busy (it runs one
//! test at a time); SERVER_ERROR (-2) is followed by iperf3's error number
//! and errno. A client that gives up sends CLIENT_TERMINATE.
//!
//! Throughput is always the **receiver's** count after the omitted
//! slow-start seconds: in upload the server's received bytes from its
//! results, in reverse (download) our own received bytes.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::binding::{self, ConnectError, WifiBinding};
use super::{Cancel, TestError, TestResult};
use crate::WifiError;

pub const DEFAULT_PORT: u16 = 5201;
pub const COOKIE_LEN: usize = 37;
/// Results JSON layout version, stored with each test.
pub const IPERF3_RESULTS_VERSION: u32 = 1;
/// iperf3's default TCP block size.
const BLOCK_LEN: usize = 128 * 1024;
/// Larger control messages are refused (iperf3's own are a few kB).
pub const MAX_JSON_LEN: usize = 1024 * 1024;
pub const MAX_STREAMS: u8 = 16;
pub const MAX_DURATION_S: u64 = 120;
pub const MAX_OMIT_S: u64 = 10;

// Test states (`iperf_api.h`). Sent as a signed char.
const TEST_START: i8 = 1;
const TEST_RUNNING: i8 = 2;
const TEST_END: i8 = 4;
const PARAM_EXCHANGE: i8 = 9;
const CREATE_STREAMS: i8 = 10;
const SERVER_TERMINATE: i8 = 11;
const CLIENT_TERMINATE: i8 = 12;
const EXCHANGE_RESULTS: i8 = 13;
const DISPLAY_RESULTS: i8 = 14;
const IPERF_START: i8 = 15;
const IPERF_DONE: i8 = 16;
const ACCESS_DENIED: i8 = -1;
const SERVER_ERROR: i8 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Iperf3Direction {
    /// This computer sends (iperf3's default).
    Upload,
    /// The server sends (`iperf3 -R`).
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iperf3Config {
    pub server: SocketAddr,
    pub streams: u8,
    /// Measured time, after `omit`. Whole seconds (the protocol's unit).
    pub duration: Duration,
    /// Slow-start time not counted. Whole seconds.
    pub omit: Duration,
    pub direction: Iperf3Direction,
}

impl Iperf3Config {
    fn validate(&self) -> Result<(), WifiError> {
        if !(1..=MAX_STREAMS).contains(&self.streams) {
            return Err(WifiError::InvalidInput(format!(
                "iperf3 streams must be 1–{MAX_STREAMS}"
            )));
        }
        let whole = |d: Duration| d.subsec_nanos() == 0;
        if !whole(self.duration) || !(1..=MAX_DURATION_S).contains(&self.duration.as_secs()) {
            return Err(WifiError::InvalidInput(format!(
                "iperf3 duration must be 1–{MAX_DURATION_S} whole seconds"
            )));
        }
        if !whole(self.omit) || self.omit.as_secs() > MAX_OMIT_S {
            return Err(WifiError::InvalidInput(format!(
                "iperf3 omit must be 0–{MAX_OMIT_S} whole seconds"
            )));
        }
        if self.server.port() == 0 || self.server.ip().is_unspecified() {
            return Err(WifiError::InvalidInput(
                "the iperf3 server address is incomplete".into(),
            ));
        }
        Ok(())
    }

    /// Upper bound of a whole test: data time plus every phase's deadline.
    pub fn max_duration(&self) -> Duration {
        self.omit + self.duration + Duration::from_secs(60)
    }
}

/// Which side counted the received bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasuredBy {
    Server,
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetransmitSource {
    /// Our sockets' `TCP_INFO` (Linux), upload only.
    ClientTcpInfo,
    /// The server's report, reverse only.
    Server,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iperf3Result {
    pub version: u32,
    pub direction: Iperf3Direction,
    pub server: String,
    pub streams: u8,
    pub duration_s: u64,
    pub omit_s: u64,
    /// Receiver average over the measured (non-omitted) time.
    pub bits_per_second: f64,
    pub receiver_bytes: u64,
    pub receiver_seconds: f64,
    pub measured_by: MeasuredBy,
    /// Bytes the sender handed to TCP in the measured time.
    pub sender_bytes: Option<u64>,
    /// TCP retransmissions by the sender in the measured time; `None` where
    /// the sender doesn't report them.
    pub retransmits: Option<u64>,
    pub retransmits_source: Option<RetransmitSource>,
    /// Per-second throughput as this computer saw it (absent in results
    /// stored before it was recorded).
    #[serde(default)]
    pub intervals: Vec<Iperf3Interval>,
}

/// One second of a test, counted on this computer: in upload the bytes
/// TCP accepted from us (the sender's view; the final average comes from
/// the server's receiver count), in download the bytes we received.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iperf3Interval {
    /// Seconds since the data started flowing.
    pub start_s: f64,
    pub end_s: f64,
    pub bytes: u64,
    pub bits_per_second: f64,
    /// Inside the omitted slow-start time (not in the average).
    pub omitted: bool,
    /// Sender retransmissions in this second (upload on Linux only).
    pub retransmits: Option<u64>,
}

pub type OnInterval = Box<dyn FnMut(&Iperf3Interval) + Send>;

/// Per-phase deadlines; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    pub connect: Duration,
    /// Waiting for a state byte or JSON from the server.
    pub control: Duration,
    /// A data stream making no progress.
    pub stall: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: super::CONNECT_TIMEOUT,
            control: Duration::from_secs(10),
            stall: Duration::from_secs(10),
        }
    }
}

/// Run one TCP test against an iperf3 server over the Wi-Fi interface.
/// The caller has checked the route to the server.
pub async fn iperf3_tcp(
    binding: &WifiBinding,
    config: Iperf3Config,
    cancel: &Cancel,
) -> TestResult<Iperf3Result> {
    run(Some(binding), config, Timeouts::default(), cancel).await
}

/// The Tools page's iperf3: bound to the Wi-Fi interface or (`None`) along
/// the system's route, reporting each second through `on_interval`.
pub async fn iperf3_tcp_live(
    binding: Option<&WifiBinding>,
    config: Iperf3Config,
    cancel: &Cancel,
    on_interval: OnInterval,
) -> TestResult<Iperf3Result> {
    run_with(
        binding,
        config,
        Timeouts::default(),
        cancel,
        Some(on_interval),
    )
    .await
}

pub(crate) async fn run(
    binding: Option<&WifiBinding>,
    config: Iperf3Config,
    timeouts: Timeouts,
    cancel: &Cancel,
) -> TestResult<Iperf3Result> {
    run_with(binding, config, timeouts, cancel, None).await
}

async fn run_with(
    binding: Option<&WifiBinding>,
    config: Iperf3Config,
    timeouts: Timeouts,
    cancel: &Cancel,
    on_interval: Option<OnInterval>,
) -> TestResult<Iperf3Result> {
    config.validate()?;
    cancel.check()?;
    let deadline = Instant::now() + config.max_duration() + START_RETRY_TOTAL;
    let (mut control, cookie) = start(binding, &config, timeouts, cancel).await?;
    let outcome = tokio::select! {
        _ = cancel.cancelled() => Err(TestError::Cancelled),
        r = tokio::time::timeout_at(
            deadline,
            session(&mut control, binding, &config, &cookie, timeouts, on_interval),
        ) => r.unwrap_or_else(|_| Err(WifiError::Timeout(
            "the iperf3 test didn't finish in time".into(),
        ).into())),
    };
    if outcome.is_err() {
        // Free the server for the next test at once (harmless if it's gone).
        // Close gracefully: dropping a socket with unread input sends a
        // reset, and Windows discards whatever the peer hadn't read yet —
        // including this byte. So send it, shut down our side (FIN), and
        // drain until the server closes, all under a short deadline.
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            control.write_all(&[CLIENT_TERMINATE as u8]).await?;
            control.shutdown().await?;
            let mut sink = [0u8; 4096];
            while control.read(&mut sink).await? > 0 {}
            Ok::<_, std::io::Error>(())
        })
        .await;
    }
    outcome
}

/// Waits before each new attempt to start a test.
const START_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_millis(750),
    Duration::from_millis(1500),
];
const START_RETRY_TOTAL: Duration = Duration::from_millis(2500);

/// Why one attempt to start a test failed, and whether another is worth it.
enum StartFailure {
    /// Before the server took the test: refused, closed right after
    /// connecting, or busy. Nothing ran; trying again is safe.
    Retry {
        error: WifiError,
        refused: bool,
    },
    Fatal(WifiError),
}

/// Connect, send the cookie and wait for PARAM_EXCHANGE, retrying a few
/// times when the server wasn't ready. An iperf3 server closes its
/// listening socket after every test (and after any connection that sends
/// no cookie, such as a port check) and opens a new one for the next: a
/// connection that lands in between is refused, or accepted by the old
/// socket and then reset. Back-to-back tests (upload then download) hit
/// this regularly. Returns the control connection and the test's cookie.
async fn start(
    binding: Option<&WifiBinding>,
    config: &Iperf3Config,
    t: Timeouts,
    cancel: &Cancel,
) -> TestResult<(TcpStream, [u8; COOKIE_LEN])> {
    let mut delays = START_RETRY_DELAYS.iter();
    // What to report if every attempt fails: a "busy" or "closed" says more
    // than the "refused" of a server that went away while we retried.
    let mut reported: Option<WifiError> = None;
    loop {
        let attempt = async {
            let mut control = binding::connect(binding, config.server, t.connect)
                .await
                .map_err(|e| match e {
                    ConnectError::Connect(io)
                        if io.kind() == std::io::ErrorKind::ConnectionRefused =>
                    {
                        StartFailure::Retry {
                            error: connect_error(config.server, ConnectError::Connect(io)),
                            refused: true,
                        }
                    }
                    other => StartFailure::Fatal(connect_error(config.server, other)),
                })?;
            let cookie = make_cookie();
            write_all(&mut control, &cookie, t.control, "the start of the test")
                .await
                .map_err(|error| StartFailure::Retry {
                    error,
                    refused: false,
                })?;
            match read_state(&mut control, t.control).await {
                StateRead::State(PARAM_EXCHANGE) => Ok((control, cookie)),
                StateRead::State(SERVER_ERROR) => {
                    Err(StartFailure::Fatal(server_error(&mut control).await))
                }
                read @ (StateRead::Closed | StateRead::State(ACCESS_DENIED)) => {
                    Err(StartFailure::Retry {
                        error: unexpected(read, "the start of the test"),
                        refused: false,
                    })
                }
                other => Err(StartFailure::Fatal(unexpected(
                    other,
                    "the start of the test",
                ))),
            }
        };
        let failure = tokio::select! {
            _ = cancel.cancelled() => return Err(TestError::Cancelled),
            r = attempt => match r {
                Ok(started) => return Ok(started),
                Err(f) => f,
            },
        };
        match failure {
            StartFailure::Fatal(e) => return Err(e.into()),
            StartFailure::Retry { error, refused } => {
                if reported.is_none() || !refused {
                    reported = Some(error);
                }
                match delays.next() {
                    Some(delay) => cancel.sleep(*delay).await?,
                    None => {
                        return Err(reported
                            .unwrap_or_else(|| closed("the start of the test"))
                            .into())
                    }
                }
            }
        }
    }
}

async fn session(
    control: &mut TcpStream,
    binding: Option<&WifiBinding>,
    config: &Iperf3Config,
    cookie: &[u8; COOKIE_LEN],
    t: Timeouts,
    mut on_interval: Option<OnInterval>,
) -> TestResult<Iperf3Result> {
    let reverse = config.direction == Iperf3Direction::Download;
    let mut params = serde_json::json!({
        "tcp": true,
        "omit": config.omit.as_secs(),
        "time": config.duration.as_secs(),
        "num": 0,
        "blockcount": 0,
        "parallel": config.streams,
        "len": BLOCK_LEN,
        "pacing_timer": 1000,
        "client_version": concat!("Fresnel ", env!("CARGO_PKG_VERSION")),
    });
    if reverse {
        params["reverse"] = true.into();
    }
    write_json(control, &params, t.control).await?;
    expect_state(control, CREATE_STREAMS, "parameter exchange", t.control).await?;
    let mut streams = Vec::with_capacity(config.streams.into());
    for _ in 0..config.streams {
        let mut s = binding::connect(binding, config.server, t.connect)
            .await
            .map_err(|e| connect_error(config.server, e))?;
        write_all(&mut s, cookie, t.control, "opening a data stream").await?;
        streams.push(s);
    }
    expect_state(control, TEST_START, "stream setup", t.control).await?;
    expect_state(control, TEST_RUNNING, "stream setup", t.control).await?;

    let start = Instant::now();
    let omit_end = start + config.omit;
    let end = omit_end + config.duration;
    let mut data = JoinSet::new();
    let counted = Arc::new(AtomicU64::new(0));
    let retrans_probe = RetransProbe::new(&streams, reverse);
    for stream in streams {
        if reverse {
            data.spawn(receive(stream, omit_end, end, t.stall, counted.clone()));
        } else {
            data.spawn(send(stream, omit_end, end, t.stall, counted.clone()));
        }
    }
    let mut clock = IntervalClock::new(start, omit_end, end, &counted, &retrans_probe);
    let ticker = clock.tick_until_end(on_interval.as_mut());
    // The server says nothing while data flows; anything it does say (an
    // error, being stopped, closing) ends the test.
    let running = tokio::select! {
        biased;
        state = read_state(control, end + t.stall - Instant::now()) => Err(state),
        r = join_streams(&mut data, config.streams) => Ok(r),
        never = ticker => match never {},
    };
    let outcomes = match running {
        Ok(r) => r?,
        Err(StateRead::State(SERVER_ERROR)) => return Err(server_error(control).await.into()),
        Err(state) => return Err(unexpected(state, "the test").into()),
    };
    // The last second ends when the streams do.
    clock.finish(on_interval.as_mut());
    let intervals = clock.intervals;
    // Reverse: keep reading until the end so the server's sends never block
    // while it processes TEST_END (dropping the set stops these).
    let mut drains = JoinSet::new();
    let mut kept = Vec::new();
    let mut client_bytes = 0u64;
    let mut retrans = Some(0u64);
    for o in outcomes {
        client_bytes += o.bytes;
        retrans = retrans.zip(o.retransmits).map(|(a, b)| a + b);
        if reverse {
            drains.spawn(drain(o.stream));
        } else {
            kept.push(o.stream);
        }
    }

    write_all(control, &[TEST_END as u8], t.control, "ending the test").await?;
    expect_state(control, EXCHANGE_RESULTS, "the end of the test", t.control).await?;
    let ours = serde_json::json!({
        // Required by the protocol. Fresnel doesn't measure CPU load; these
        // only appear in the server's own console output.
        "cpu_util_total": 0,
        "cpu_util_user": 0,
        "cpu_util_system": 0,
        "sender_has_retransmits": if reverse { -1 } else { i32::from(retrans.is_some()) },
        // Per-stream numbers are only for the server's display, and their
        // IDs would have to match the server's numbering: sent empty.
        "streams": [],
    });
    write_json(control, &ours, t.control).await?;
    let theirs = read_json(control, t.control).await?;
    expect_state(control, DISPLAY_RESULTS, "the results exchange", t.control).await?;
    // The server only waits for this; its absence isn't a test failure.
    let _ = write_all(control, &[IPERF_DONE as u8], t.control, "finishing").await;
    drop((drains, kept));

    let server = parse_server_results(&theirs, config.streams)?;
    let duration_s = config.duration.as_secs();
    let result = if reverse {
        let seconds = config.duration.as_secs_f64();
        Iperf3Result {
            version: IPERF3_RESULTS_VERSION,
            direction: config.direction,
            server: config.server.to_string(),
            streams: config.streams,
            duration_s,
            omit_s: config.omit.as_secs(),
            bits_per_second: client_bytes as f64 * 8.0 / seconds,
            receiver_bytes: client_bytes,
            receiver_seconds: seconds,
            measured_by: MeasuredBy::Client,
            sender_bytes: Some(server.bytes),
            retransmits: server.retransmits,
            retransmits_source: server.retransmits.map(|_| RetransmitSource::Server),
            intervals,
        }
    } else {
        let seconds = server.seconds.unwrap_or(config.duration.as_secs_f64());
        Iperf3Result {
            version: IPERF3_RESULTS_VERSION,
            direction: config.direction,
            server: config.server.to_string(),
            streams: config.streams,
            duration_s,
            omit_s: config.omit.as_secs(),
            bits_per_second: server.bytes as f64 * 8.0 / seconds,
            receiver_bytes: server.bytes,
            receiver_seconds: seconds,
            measured_by: MeasuredBy::Server,
            sender_bytes: Some(client_bytes),
            retransmits: retrans,
            retransmits_source: retrans.map(|_| RetransmitSource::ClientTcpInfo),
            intervals,
        }
    };
    Ok(result)
}

struct StreamOutcome {
    stream: TcpStream,
    /// Sent or received after the omitted time.
    bytes: u64,
    retransmits: Option<u64>,
}

async fn join_streams(
    set: &mut JoinSet<Result<StreamOutcome, WifiError>>,
    n: u8,
) -> Result<Vec<StreamOutcome>, WifiError> {
    let mut out = Vec::with_capacity(n.into());
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(o)) => out.push(o),
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(WifiError::Backend(format!("data stream task failed: {e}"))),
        }
    }
    Ok(out)
}

/// Cuts the test into seconds (all streams together).
struct IntervalClock<'a> {
    start: Instant,
    omit_end: Instant,
    end: Instant,
    counted: &'a AtomicU64,
    retrans: &'a RetransProbe,
    last_bytes: u64,
    last_retrans: Option<u64>,
    last_at: Instant,
    intervals: Vec<Iperf3Interval>,
}

impl<'a> IntervalClock<'a> {
    fn new(
        start: Instant,
        omit_end: Instant,
        end: Instant,
        counted: &'a AtomicU64,
        retrans: &'a RetransProbe,
    ) -> Self {
        Self {
            start,
            omit_end,
            end,
            counted,
            retrans,
            last_bytes: 0,
            last_retrans: retrans.total(),
            last_at: start,
            intervals: Vec::new(),
        }
    }

    /// Report each whole second before `end`, then wait forever (the
    /// caller's other branches end the select; `finish` does the last one).
    async fn tick_until_end(
        &mut self,
        mut on_interval: Option<&mut OnInterval>,
    ) -> std::convert::Infallible {
        let mut tick = self.start + Duration::from_secs(1);
        while tick < self.end {
            tokio::time::sleep_until(tick).await;
            self.cut(on_interval.as_deref_mut());
            tick += Duration::from_secs(1);
        }
        std::future::pending().await
    }

    /// The last interval, up to now (the streams have ended).
    fn finish(&mut self, on_interval: Option<&mut OnInterval>) {
        if Instant::now() > self.last_at + Duration::from_millis(100) {
            self.cut(on_interval);
        }
    }

    fn cut(&mut self, on_interval: Option<&mut OnInterval>) {
        let now = Instant::now().min(self.end + Duration::from_secs(1));
        let total = self.counted.load(Ordering::Relaxed);
        let bytes = total.saturating_sub(self.last_bytes);
        let total_retrans = self.retrans.total();
        let seconds = (now - self.last_at).as_secs_f64().max(1e-3);
        let interval = Iperf3Interval {
            start_s: (self.last_at - self.start).as_secs_f64(),
            end_s: (now - self.start).as_secs_f64(),
            bytes,
            bits_per_second: bytes as f64 * 8.0 / seconds,
            // Mostly inside the omitted time (seconds straddle it only if
            // a tick was late).
            omitted: self.last_at + (now - self.last_at) / 2 < self.omit_end,
            retransmits: self
                .last_retrans
                .zip(total_retrans)
                .map(|(a, b)| b.saturating_sub(a)),
        };
        if let Some(f) = on_interval {
            f(&interval);
        }
        self.intervals.push(interval);
        (self.last_bytes, self.last_retrans, self.last_at) = (total, total_retrans, now);
    }
}

/// Reads the upload streams' retransmission counters while they are owned
/// by their tasks (Linux: `TCP_INFO` on the raw descriptors, which stay
/// open until the results are exchanged).
struct RetransProbe {
    #[cfg(target_os = "linux")]
    fds: Vec<std::os::fd::RawFd>,
}

impl RetransProbe {
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    fn new(streams: &[TcpStream], reverse: bool) -> Self {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            Self {
                fds: if reverse {
                    Vec::new()
                } else {
                    streams.iter().map(|s| s.as_raw_fd()).collect()
                },
            }
        }
        #[cfg(not(target_os = "linux"))]
        Self {}
    }

    fn total(&self) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            if self.fds.is_empty() {
                return None;
            }
            self.fds
                .iter()
                .map(|fd| fd_total_retrans(*fd))
                .sum::<Option<u64>>()
        }
        #[cfg(not(target_os = "linux"))]
        None
    }
}

/// Upload: write blocks until `end`; count what TCP accepted after `omit_end`.
async fn send(
    mut stream: TcpStream,
    omit_end: Instant,
    end: Instant,
    stall: Duration,
    counted: Arc<AtomicU64>,
) -> Result<StreamOutcome, WifiError> {
    let payload = payload();
    let mut bytes = 0u64;
    let mut baseline = None;
    loop {
        let now = Instant::now();
        if now >= end {
            break;
        }
        if now >= omit_end && baseline.is_none() {
            baseline = Some(total_retrans(&stream));
        }
        match tokio::time::timeout(stall, stream.write(&payload)).await {
            Ok(Ok(0)) => return Err(closed("the test")),
            Ok(Ok(n)) => {
                counted.fetch_add(n as u64, Ordering::Relaxed);
                if now >= omit_end {
                    bytes += n as u64;
                }
            }
            Ok(Err(e)) => return Err(data_error(e)),
            Err(_) => return Err(stalled()),
        }
    }
    let retransmits = match (baseline, total_retrans(&stream)) {
        (Some(Some(a)), Some(b)) => Some(b.saturating_sub(a)),
        _ => None,
    };
    Ok(StreamOutcome {
        stream,
        bytes,
        retransmits,
    })
}

/// Download: read until `end`; count what arrived after `omit_end`.
async fn receive(
    mut stream: TcpStream,
    omit_end: Instant,
    end: Instant,
    stall: Duration,
    counted: Arc<AtomicU64>,
) -> Result<StreamOutcome, WifiError> {
    let mut buf = vec![0u8; BLOCK_LEN];
    let mut bytes = 0u64;
    loop {
        let now = Instant::now();
        if now >= end {
            break;
        }
        let wait = stall.min(end - now);
        match tokio::time::timeout(wait, stream.read(&mut buf)).await {
            Ok(Ok(0)) => return Err(closed("the test")),
            Ok(Ok(n)) => {
                counted.fetch_add(n as u64, Ordering::Relaxed);
                if Instant::now() >= omit_end {
                    bytes += n as u64;
                }
            }
            Ok(Err(e)) => return Err(data_error(e)),
            // Hit `end`, or a stall (checked against `stall` below).
            Err(_) if Instant::now() >= end => break,
            Err(_) => return Err(stalled()),
        }
    }
    Ok(StreamOutcome {
        stream,
        bytes,
        retransmits: None,
    })
}

async fn drain(mut stream: TcpStream) {
    let mut buf = vec![0u8; BLOCK_LEN];
    while let Ok(n) = stream.read(&mut buf).await {
        if n == 0 {
            break;
        }
    }
}

/// Not compressible, in case anything on the path compresses.
fn payload() -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..BLOCK_LEN)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Sender retransmissions so far, from `TCP_INFO` (Linux).
#[cfg(target_os = "linux")]
fn total_retrans(stream: &TcpStream) -> Option<u64> {
    use std::os::fd::AsRawFd;
    fd_total_retrans(stream.as_raw_fd())
}

#[cfg(target_os = "linux")]
fn fd_total_retrans(fd: std::os::fd::RawFd) -> Option<u64> {
    // SAFETY: tcp_info is plain data; getsockopt writes at most `len` bytes.
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            (&mut info as *mut libc::tcp_info).cast(),
            &mut len,
        )
    };
    let needed = std::mem::offset_of!(libc::tcp_info, tcpi_total_retrans) + 4;
    (rc == 0 && len as usize >= needed).then_some(u64::from(info.tcpi_total_retrans))
}

/// Windows' `SIO_TCP_INFO` isn't read yet: upload retransmits are unknown.
#[cfg(not(target_os = "linux"))]
fn total_retrans(_stream: &TcpStream) -> Option<u64> {
    None
}

#[derive(Debug)]
struct ServerResults {
    bytes: u64,
    seconds: Option<f64>,
    retransmits: Option<u64>,
}

/// The server's results: per stream, the bytes it counted after the omitted
/// time, its stream time, and (as sender, if it can) retransmits.
fn parse_server_results(v: &serde_json::Value, streams: u8) -> Result<ServerResults, WifiError> {
    let bad = |what: &str| {
        WifiError::Backend(format!(
            "the iperf3 server's results are in a format Fresnel doesn't understand ({what})"
        ))
    };
    for key in ["cpu_util_total", "sender_has_retransmits"] {
        if v.get(key).is_none() {
            return Err(bad(&format!("no {key}")));
        }
    }
    let list = v
        .get("streams")
        .and_then(|s| s.as_array())
        .ok_or_else(|| bad("no streams"))?;
    if list.len() != usize::from(streams) {
        return Err(bad(&format!(
            "{} streams reported, {streams} requested",
            list.len()
        )));
    }
    let has_retransmits = v["sender_has_retransmits"].as_i64() == Some(1);
    let mut bytes = 0u64;
    let mut seconds: Option<f64> = None;
    let mut retransmits = Some(0u64);
    for s in list {
        let b = s
            .get("bytes")
            .and_then(number_u64)
            .ok_or_else(|| bad("stream bytes"))?;
        bytes = bytes.saturating_add(b);
        if let (Some(a), Some(z)) = (
            s.get("start_time").and_then(|t| t.as_f64()),
            s.get("end_time").and_then(|t| t.as_f64()),
        ) {
            if z - a > 0.0 && (z - a).is_finite() {
                seconds = Some(seconds.map_or(z - a, |m| m.max(z - a)));
            }
        }
        let r = s.get("retransmits").and_then(number_u64);
        retransmits = retransmits.zip(r).map(|(a, b)| a + b);
    }
    Ok(ServerResults {
        bytes,
        seconds,
        retransmits: if has_retransmits { retransmits } else { None },
    })
}

/// iperf3 writes numbers as JSON doubles; negative means "not available".
fn number_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| *f >= 0.0 && f.is_finite())
            .map(|f| f as u64)
    })
}

fn make_cookie() -> [u8; COOKIE_LEN] {
    // iperf3's own alphabet; uniqueness only needs to hold per server.
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    let mut x = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut cookie = [0u8; COOKIE_LEN];
    for c in cookie.iter_mut().take(COOKIE_LEN - 1) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *c = ALPHABET[(x % 32) as usize];
    }
    cookie
}

async fn write_all<W: AsyncWrite + Unpin>(
    w: &mut W,
    bytes: &[u8],
    timeout: Duration,
    phase: &str,
) -> Result<(), WifiError> {
    match tokio::time::timeout(timeout, w.write_all(bytes)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) if is_closed(&e) => Err(closed(phase)),
        Ok(Err(e)) => Err(WifiError::Backend(format!(
            "iperf3 control connection: {e}"
        ))),
        Err(_) => Err(WifiError::Timeout(format!(
            "the iperf3 server stopped accepting data during {phase}"
        ))),
    }
}

/// What the server said, or why nothing usable arrived.
enum StateRead {
    State(i8),
    Closed,
    TimedOut,
    Failed(std::io::Error),
}

async fn read_state<R: AsyncRead + Unpin>(r: &mut R, timeout: Duration) -> StateRead {
    let mut b = [0u8; 1];
    match tokio::time::timeout(timeout, r.read_exact(&mut b)).await {
        Ok(Ok(_)) => StateRead::State(b[0] as i8),
        Ok(Err(e)) if is_closed(&e) => StateRead::Closed,
        Ok(Err(e)) => StateRead::Failed(e),
        Err(_) => StateRead::TimedOut,
    }
}

async fn expect_state<R: AsyncRead + Unpin>(
    r: &mut R,
    want: i8,
    phase: &str,
    timeout: Duration,
) -> Result<(), WifiError> {
    match read_state(r, timeout).await {
        StateRead::State(s) if s == want => Ok(()),
        StateRead::State(SERVER_ERROR) => Err(server_error(r).await),
        other => Err(unexpected(other, phase)),
    }
}

fn unexpected(read: StateRead, phase: &str) -> WifiError {
    match read {
        StateRead::State(ACCESS_DENIED) => {
            WifiError::Backend("the iperf3 server is busy with another test".into()).with_hint(
                "An iperf3 server runs one test at a time. Try again in a few seconds, or start \
             another server on a different port.",
            )
        }
        StateRead::State(SERVER_ERROR) => WifiError::Backend(format!(
            "the iperf3 server reported an error during {phase}"
        )),
        StateRead::State(SERVER_TERMINATE) => {
            WifiError::Backend(format!("the iperf3 server was stopped during {phase}"))
        }
        StateRead::State(s) => WifiError::Backend(format!(
            "unexpected iperf3 message {s} during {phase}{}",
            if s == IPERF_START || s == TEST_START {
                ""
            } else {
                " (is this an iperf3 3.x server?)"
            }
        )),
        StateRead::Closed => closed(phase),
        StateRead::TimedOut => {
            WifiError::Timeout(format!("the iperf3 server didn't answer during {phase}")).with_hint(
                "Check the address and port, that iperf3 -s is still running, and that nothing \
             between here and the server drops the connection.",
            )
        }
        StateRead::Failed(e) => WifiError::Backend(format!(
            "iperf3 control connection failed during {phase}: {e}"
        )),
    }
}

/// After SERVER_ERROR the server sends iperf3's error number and errno
/// (each 4 bytes, big-endian); some paths send only the first.
async fn server_error<R: AsyncRead + Unpin>(r: &mut R) -> WifiError {
    let mut codes = Vec::new();
    for _ in 0..2 {
        let mut b = [0u8; 4];
        match tokio::time::timeout(Duration::from_secs(1), r.read_exact(&mut b)).await {
            Ok(Ok(_)) => codes.push(i32::from_be_bytes(b)),
            _ => break,
        }
    }
    let detail = match codes.as_slice() {
        [] => String::new(),
        [code] => format!(" (iperf3 error {code})"),
        [code, errno, ..] => format!(" (iperf3 error {code}, errno {errno})"),
    };
    WifiError::Backend(format!("the iperf3 server refused the test{detail}")).with_hint(
        "Fresnel's client doesn't support servers that require authentication \
         (--rsa-private-key-path) or limit the bitrate (--server-bitrate-limit). The server's \
         console shows the reason.",
    )
}

async fn write_json<W: AsyncWrite + Unpin>(
    w: &mut W,
    value: &serde_json::Value,
    timeout: Duration,
) -> Result<(), WifiError> {
    let body = serde_json::to_vec(value)
        .map_err(|e| WifiError::Backend(format!("cannot encode iperf3 JSON: {e}")))?;
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    write_all(w, &frame, timeout, "the parameter or results exchange").await
}

async fn read_json<R: AsyncRead + Unpin>(
    r: &mut R,
    timeout: Duration,
) -> Result<serde_json::Value, WifiError> {
    let phase = "the results exchange";
    let read = async {
        let mut n = [0u8; 4];
        r.read_exact(&mut n).await?;
        let len = u32::from_be_bytes(n) as usize;
        if len == 0 || len > MAX_JSON_LEN {
            return Ok(Err(WifiError::Backend(format!(
                "the iperf3 server sent a {len}-byte message; at most {MAX_JSON_LEN} is accepted \
                 (is this an iperf3 server?)"
            ))));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).await?;
        Ok(Ok(body))
    };
    let body = match tokio::time::timeout(timeout, read).await {
        Ok(Ok(Ok(body))) => body,
        Ok(Ok(Err(e))) => return Err(e),
        Ok(Err(e)) if is_closed(&e) => return Err(closed(phase)),
        Ok(Err(e)) => {
            return Err(WifiError::Backend(format!(
                "iperf3 control connection: {e}"
            )))
        }
        Err(_) => return Err(unexpected(StateRead::TimedOut, phase)),
    };
    serde_json::from_slice(&body)
        .map_err(|e| WifiError::Backend(format!("the iperf3 server sent invalid JSON: {e}")))
}

fn is_closed(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe
    )
}

fn closed(phase: &str) -> WifiError {
    WifiError::Backend(format!(
        "the iperf3 server closed the connection during {phase}"
    ))
    .with_hint(
        "The server may have been stopped or restarted, or another client took it over. \
         Check the server's console.",
    )
}

fn stalled() -> WifiError {
    WifiError::Timeout("an iperf3 data stream made no progress".into())
        .with_hint("The Wi-Fi link may have dropped or roamed, or the server stopped reading.")
}

fn data_error(e: std::io::Error) -> WifiError {
    if is_closed(&e) {
        closed("the test")
    } else {
        WifiError::Backend(format!("iperf3 data stream failed: {e}"))
    }
}

fn connect_error(addr: SocketAddr, e: ConnectError) -> WifiError {
    use std::io::ErrorKind::*;
    let e = match e {
        ConnectError::Bind(e) => return e,
        ConnectError::Connect(e) => e,
    };
    let firewall = format!(
        "Start iperf3 -s on the server and allow inbound TCP port {} in the server's firewall \
         (Windows Defender Firewall, ufw, firewalld). This computer needs no inbound rule, \
         not even for download tests.",
        addr.port()
    );
    match e.kind() {
        ConnectionRefused => {
            WifiError::Backend(format!("nothing accepted the connection on {addr}"))
                .with_hint(firewall)
        }
        TimedOut => WifiError::Timeout(format!("no answer from {addr}")).with_hint(format!(
            "Check the address. Otherwise a firewall is probably dropping the connection. {firewall}"
        )),
        HostUnreachable | NetworkUnreachable => {
            WifiError::Backend(format!("{addr} is unreachable: {e}"))
                .with_hint("Check the address and that the server is on a network the Wi-Fi reaches.")
        }
        _ => WifiError::Backend(format!("cannot connect to {addr}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::Arc;

    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    use super::*;

    const SHORT: Timeouts = Timeouts {
        connect: Duration::from_secs(2),
        control: Duration::from_millis(400),
        stall: Duration::from_secs(2),
    };

    fn config(addr: SocketAddr, direction: Iperf3Direction, streams: u8) -> Iperf3Config {
        Iperf3Config {
            server: addr,
            streams,
            duration: Duration::from_secs(1),
            omit: Duration::ZERO,
            direction,
        }
    }

    async fn listen() -> (TcpListener, SocketAddr) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let a = l.local_addr().unwrap();
        (l, a)
    }

    async fn cookie(s: &mut TcpStream) -> [u8; COOKIE_LEN] {
        let mut c = [0u8; COOKIE_LEN];
        s.read_exact(&mut c).await.unwrap();
        c
    }

    async fn state(s: &mut TcpStream, v: i8) {
        s.write_all(&[v as u8]).await.unwrap();
    }

    async fn read_state_byte(s: &mut TcpStream) -> i8 {
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await.unwrap();
        b[0] as i8
    }

    async fn json_in(s: &mut TcpStream) -> serde_json::Value {
        let mut n = [0u8; 4];
        s.read_exact(&mut n).await.unwrap();
        let mut b = vec![0u8; u32::from_be_bytes(n) as usize];
        s.read_exact(&mut b).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    async fn json_out(s: &mut TcpStream, v: &serde_json::Value) {
        let b = serde_json::to_vec(v).unwrap();
        s.write_all(&(b.len() as u32).to_be_bytes()).await.unwrap();
        s.write_all(&b).await.unwrap();
    }

    fn server_results(per_stream: &[(u64, i64)], has_retransmits: bool) -> serde_json::Value {
        let streams: Vec<_> = per_stream
            .iter()
            .enumerate()
            .map(|(i, (bytes, retr))| {
                serde_json::json!({
                    "id": if i == 0 { 1 } else { i + 2 }, "bytes": *bytes as f64,
                    "retransmits": retr, "jitter": 0, "errors": 0, "packets": 0,
                    "start_time": 0, "end_time": 1.0,
                })
            })
            .collect();
        serde_json::json!({
            "cpu_util_total": 1.5, "cpu_util_user": 1.0, "cpu_util_system": 0.5,
            "sender_has_retransmits": i32::from(has_retransmits), "streams": streams,
        })
    }

    /// A scripted iperf3 server for one test. Records the parameters and the
    /// client's results.
    #[derive(Default)]
    struct Seen {
        params: Option<serde_json::Value>,
        results: Option<serde_json::Value>,
        received: u64,
        done: bool,
    }

    async fn fake_server(
        listener: TcpListener,
        reverse_payload: bool,
        results: serde_json::Value,
        seen: Arc<Mutex<Seen>>,
    ) {
        let (mut c, _) = listener.accept().await.unwrap();
        let ck = cookie(&mut c).await;
        assert_eq!(ck[COOKIE_LEN - 1], 0, "cookie is NUL-terminated");
        state(&mut c, PARAM_EXCHANGE).await;
        let params = json_in(&mut c).await;
        let n = params["parallel"].as_u64().unwrap();
        seen.lock().await.params = Some(params);
        state(&mut c, CREATE_STREAMS).await;
        let mut data = Vec::new();
        for _ in 0..n {
            let (mut s, _) = listener.accept().await.unwrap();
            assert_eq!(
                cookie(&mut s).await,
                ck,
                "data streams send the same cookie"
            );
            data.push(s);
        }
        state(&mut c, TEST_START).await;
        state(&mut c, TEST_RUNNING).await;
        let mut tasks = JoinSet::new();
        for mut s in data {
            tasks.spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut total = 0u64;
                if reverse_payload {
                    let block = vec![7u8; 65536];
                    while s.write_all(&block).await.is_ok() {}
                } else {
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        total += n as u64;
                    }
                }
                total
            });
        }
        assert_eq!(read_state_byte(&mut c).await, TEST_END);
        state(&mut c, EXCHANGE_RESULTS).await;
        seen.lock().await.results = Some(json_in(&mut c).await);
        json_out(&mut c, &results).await;
        state(&mut c, DISPLAY_RESULTS).await;
        assert_eq!(read_state_byte(&mut c).await, IPERF_DONE);
        let mut received = 0;
        while let Some(Ok(n)) = tasks.join_next().await {
            received += n;
        }
        let mut s = seen.lock().await;
        s.received = received;
        s.done = true;
    }

    #[tokio::test]
    async fn upload_uses_the_servers_receiver_bytes() {
        let (listener, addr) = listen().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let server = tokio::spawn(fake_server(
            listener,
            false,
            server_results(&[(1_000_000, -1), (500_000, -1)], false),
            seen.clone(),
        ));
        let live = Arc::new(std::sync::Mutex::new(0usize));
        let counter = live.clone();
        let r = run_with(
            None,
            config(addr, Iperf3Direction::Upload, 2),
            SHORT,
            &Cancel::never(),
            Some(Box::new(move |_| *counter.lock().unwrap() += 1)),
        )
        .await
        .unwrap();
        server.await.unwrap();
        // One second, nothing omitted: one interval, reported live too.
        assert_eq!(r.intervals.len(), 1, "{:?}", r.intervals);
        assert_eq!(*live.lock().unwrap(), 1);
        assert!(r.intervals[0].bytes > 0 && !r.intervals[0].omitted);
        let seen = seen.lock().await;
        let params = seen.params.as_ref().unwrap();
        assert_eq!(params["tcp"], true);
        assert_eq!(params["parallel"], 2);
        assert_eq!(params["time"], 1);
        assert!(params.get("reverse").is_none());
        let ours = seen.results.as_ref().unwrap();
        for key in [
            "cpu_util_total",
            "cpu_util_user",
            "cpu_util_system",
            "sender_has_retransmits",
        ] {
            assert!(ours.get(key).is_some(), "{key} missing from client results");
        }
        assert!(ours["streams"].is_array());
        assert!(seen.done && seen.received > 0);
        assert_eq!(r.measured_by, MeasuredBy::Server);
        assert_eq!(r.receiver_bytes, 1_500_000);
        assert_eq!(r.bits_per_second, 12_000_000.0);
        // Our socket writes are the sender side, not the result.
        assert!(r.sender_bytes.unwrap() > 0);
        if cfg!(target_os = "linux") {
            assert_eq!(r.retransmits_source, Some(RetransmitSource::ClientTcpInfo));
            assert_eq!(ours["sender_has_retransmits"], 1);
        } else {
            assert_eq!(r.retransmits, None);
        }
    }

    #[tokio::test]
    async fn download_counts_received_bytes_and_server_retransmits() {
        let (listener, addr) = listen().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let server = tokio::spawn(fake_server(
            listener,
            true,
            server_results(&[(9_000_000, 5), (9_000_000, 7)], true),
            seen.clone(),
        ));
        let r = run(
            None,
            config(addr, Iperf3Direction::Download, 2),
            SHORT,
            &Cancel::never(),
        )
        .await
        .unwrap();
        server.await.unwrap();
        let seen = seen.lock().await;
        assert_eq!(seen.params.as_ref().unwrap()["reverse"], true);
        assert_eq!(seen.results.as_ref().unwrap()["sender_has_retransmits"], -1);
        assert_eq!(r.measured_by, MeasuredBy::Client);
        assert!(r.receiver_bytes > 0);
        assert_eq!(r.bits_per_second, r.receiver_bytes as f64 * 8.0);
        assert_eq!(r.sender_bytes, Some(18_000_000));
        assert_eq!(r.retransmits, Some(12));
        assert_eq!(r.retransmits_source, Some(RetransmitSource::Server));
    }

    #[tokio::test]
    async fn omitted_seconds_are_requested() {
        let (listener, addr) = listen().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let server = tokio::spawn(fake_server(
            listener,
            false,
            server_results(&[(100, -1)], false),
            seen.clone(),
        ));
        let mut c = config(addr, Iperf3Direction::Upload, 1);
        c.omit = Duration::from_secs(1);
        let started = std::time::Instant::now();
        run(None, c, SHORT, &Cancel::never()).await.unwrap();
        server.await.unwrap();
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert_eq!(seen.lock().await.params.as_ref().unwrap()["omit"], 1);
    }

    /// Run a client against a server script; return the error.
    async fn fails_with<F, Fut>(script: F) -> TestError
    where
        F: FnOnce(TcpListener) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let (listener, addr) = listen().await;
        let server = tokio::spawn(script(listener));
        let e = run(
            None,
            config(addr, Iperf3Direction::Upload, 1),
            SHORT,
            &Cancel::never(),
        )
        .await
        .unwrap_err();
        server.abort();
        e
    }

    fn message(e: &TestError) -> String {
        match e {
            TestError::Failed(e) => e.to_string(),
            TestError::Cancelled => "cancelled".into(),
        }
    }

    #[tokio::test]
    async fn busy_server_is_explained() {
        let e = fails_with(|l| async move {
            let (mut c, _) = l.accept().await.unwrap();
            cookie(&mut c).await;
            state(&mut c, ACCESS_DENIED).await;
        })
        .await;
        assert!(message(&e).contains("busy"), "{e}");
        let TestError::Failed(e) = e else { panic!() };
        assert!(e.hint().unwrap().contains("one test at a time"));
    }

    /// Between tests an iperf3 server closes and reopens its listening
    /// socket; a connection caught in between is reset after the cookie, or
    /// told "busy". Starting again shortly after succeeds.
    #[tokio::test]
    async fn start_is_retried_while_the_server_resets() {
        let (listener, addr) = listen().await;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let server = tokio::spawn({
            let seen = seen.clone();
            async move {
                let (mut c, _) = listener.accept().await.unwrap();
                cookie(&mut c).await;
                drop(c);
                let (mut c, _) = listener.accept().await.unwrap();
                cookie(&mut c).await;
                state(&mut c, ACCESS_DENIED).await;
                drop(c);
                fake_server(
                    listener,
                    false,
                    server_results(&[(1_000_000, -1)], false),
                    seen,
                )
                .await;
            }
        });
        let r = run(
            None,
            config(addr, Iperf3Direction::Upload, 1),
            SHORT,
            &Cancel::never(),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(r.receiver_bytes, 1_000_000);
        assert!(seen.lock().await.done);
    }

    #[tokio::test]
    async fn server_error_reports_its_code() {
        let e = fails_with(|l| async move {
            let (mut c, _) = l.accept().await.unwrap();
            cookie(&mut c).await;
            state(&mut c, PARAM_EXCHANGE).await;
            json_in(&mut c).await;
            state(&mut c, SERVER_ERROR).await;
            c.write_all(&156i32.to_be_bytes()).await.unwrap();
            c.write_all(&0i32.to_be_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
        .await;
        assert!(message(&e).contains("iperf3 error 156, errno 0"), "{e}");
        let TestError::Failed(e) = e else { panic!() };
        assert!(e.hint().unwrap().contains("authentication"));
    }

    /// The server goes away at each point of the protocol.
    #[tokio::test]
    async fn abrupt_close_in_every_phase() {
        for phase in 0..6 {
            let e = fails_with(move |l| async move {
                let (mut c, _) = l.accept().await.unwrap();
                let ck = cookie(&mut c).await;
                if phase == 0 {
                    return;
                }
                state(&mut c, PARAM_EXCHANGE).await;
                json_in(&mut c).await;
                if phase == 1 {
                    return;
                }
                state(&mut c, CREATE_STREAMS).await;
                let (mut s, _) = l.accept().await.unwrap();
                assert_eq!(cookie(&mut s).await, ck);
                if phase == 2 {
                    return;
                }
                state(&mut c, TEST_START).await;
                state(&mut c, TEST_RUNNING).await;
                if phase == 3 {
                    // Mid-test: both control and data go away.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    return;
                }
                let reader = tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {}
                });
                assert_eq!(read_state_byte(&mut c).await, TEST_END);
                if phase == 4 {
                    return;
                }
                state(&mut c, EXCHANGE_RESULTS).await;
                json_in(&mut c).await;
                if phase == 5 {
                    return;
                }
                reader.abort();
            })
            .await;
            assert!(message(&e).contains("closed"), "phase {phase}: {e}");
        }
    }

    #[tokio::test]
    async fn silent_server_times_out() {
        let e = fails_with(|l| async move {
            let (_c, _) = l.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        })
        .await;
        let TestError::Failed(e) = e else { panic!() };
        assert_eq!(e.kind(), "timeout", "{e}");
    }

    #[tokio::test]
    async fn malformed_and_oversize_json_are_refused() {
        for oversize in [false, true] {
            let e = fails_with(move |l| async move {
                let (mut c, _) = l.accept().await.unwrap();
                let ck = cookie(&mut c).await;
                state(&mut c, PARAM_EXCHANGE).await;
                json_in(&mut c).await;
                state(&mut c, CREATE_STREAMS).await;
                let (mut s, _) = l.accept().await.unwrap();
                assert_eq!(cookie(&mut s).await, ck);
                state(&mut c, TEST_START).await;
                state(&mut c, TEST_RUNNING).await;
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {}
                });
                assert_eq!(read_state_byte(&mut c).await, TEST_END);
                state(&mut c, EXCHANGE_RESULTS).await;
                json_in(&mut c).await;
                if oversize {
                    c.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
                } else {
                    c.write_all(&5u32.to_be_bytes()).await.unwrap();
                    c.write_all(b"{nope").await.unwrap();
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            })
            .await;
            let m = message(&e);
            if oversize {
                assert!(m.contains("at most"), "{m}");
            } else {
                assert!(m.contains("invalid JSON"), "{m}");
            }
        }
    }

    #[test]
    fn server_results_are_validated() {
        assert!(parse_server_results(&serde_json::json!({}), 1).is_err());
        let one = server_results(&[(10, -1)], false);
        assert!(parse_server_results(&one, 2)
            .unwrap_err()
            .to_string()
            .contains("2 requested"));
        let r = parse_server_results(&one, 1).unwrap();
        assert_eq!((r.bytes, r.seconds, r.retransmits), (10, Some(1.0), None));
        let mut bad = one.clone();
        bad["streams"][0]["bytes"] = "lots".into();
        assert!(parse_server_results(&bad, 1).is_err());
        // Sender retransmits reported, but one stream says "unknown" (-1).
        let mixed = server_results(&[(10, 3), (10, -1)], true);
        assert_eq!(parse_server_results(&mixed, 2).unwrap().retransmits, None);
    }

    #[tokio::test]
    async fn not_an_iperf3_server() {
        let e = fails_with(|l| async move {
            let (mut c, _) = l.accept().await.unwrap();
            c.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
        .await;
        assert!(message(&e).contains("iperf3 3.x server"), "{e}");
    }

    #[tokio::test]
    async fn refused_connection_blames_the_server_firewall() {
        let (listener, addr) = listen().await;
        drop(listener);
        let e = run(
            None,
            config(addr, Iperf3Direction::Upload, 1),
            SHORT,
            &Cancel::never(),
        )
        .await
        .unwrap_err();
        let TestError::Failed(e) = e else { panic!() };
        assert!(e.hint().unwrap().contains("firewall"), "{e}");
        assert!(e.hint().unwrap().contains("no inbound rule"));
    }

    #[tokio::test]
    async fn cancel_mid_test_tells_the_server() {
        let (listener, addr) = listen().await;
        let (tx, cancel) = Cancel::new();
        let server = tokio::spawn(async move {
            let (mut c, _) = listener.accept().await.unwrap();
            let ck = cookie(&mut c).await;
            state(&mut c, PARAM_EXCHANGE).await;
            json_in(&mut c).await;
            state(&mut c, CREATE_STREAMS).await;
            let (mut s, _) = listener.accept().await.unwrap();
            assert_eq!(cookie(&mut s).await, ck);
            state(&mut c, TEST_START).await;
            state(&mut c, TEST_RUNNING).await;
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {}
            });
            tx.send(true).unwrap();
            read_state_byte(&mut c).await
        });
        let mut c = config(addr, Iperf3Direction::Upload, 1);
        c.duration = Duration::from_secs(30);
        let e = run(None, c, SHORT, &cancel).await.unwrap_err();
        assert!(matches!(e, TestError::Cancelled));
        assert_eq!(server.await.unwrap(), CLIENT_TERMINATE);
    }

    #[test]
    fn config_limits_and_cookie() {
        let addr: SocketAddr = "192.0.2.1:5201".parse().unwrap();
        let ok = config(addr, Iperf3Direction::Upload, 1);
        assert!(ok.validate().is_ok());
        for bad in [
            Iperf3Config {
                streams: 0,
                ..ok.clone()
            },
            Iperf3Config {
                streams: 17,
                ..ok.clone()
            },
            Iperf3Config {
                duration: Duration::ZERO,
                ..ok.clone()
            },
            Iperf3Config {
                duration: Duration::from_millis(1500),
                ..ok.clone()
            },
            Iperf3Config {
                omit: Duration::from_secs(11),
                ..ok.clone()
            },
            Iperf3Config {
                server: "0.0.0.0:5201".parse().unwrap(),
                ..ok.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let (a, b) = (make_cookie(), make_cookie());
        assert_ne!(a, b);
        assert!(a[..COOKIE_LEN - 1]
            .iter()
            .all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(a[COOKIE_LEN - 1], 0);
    }

    /// Against a real `iperf3 -s`, when iperf3 is installed (skipped otherwise).
    #[tokio::test]
    async fn real_iperf3_server() {
        let Some(exe) = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| {
                    d.join(if cfg!(windows) {
                        "iperf3.exe"
                    } else {
                        "iperf3"
                    })
                })
                .find(|f| f.is_file())
        }) else {
            eprintln!("iperf3 isn't on PATH; skipping the real-server test");
            return;
        };
        let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut child = tokio::process::Command::new(exe)
            .args(["-s", "-B", "127.0.0.1", "-p", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        for _ in 0..50 {
            if TcpStream::connect(addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // The probe connection above makes the server log an error; give it
        // a moment to be ready for a real test.
        tokio::time::sleep(Duration::from_millis(300)).await;
        for direction in [Iperf3Direction::Upload, Iperf3Direction::Download] {
            let r = run(
                None,
                config(addr, direction, 2),
                Timeouts::default(),
                &Cancel::never(),
            )
            .await
            .unwrap_or_else(|e| panic!("{direction:?}: {e}"));
            assert!(r.bits_per_second > 0.0, "{r:?}");
            assert!(
                r.receiver_seconds > 0.5 && r.receiver_seconds < 2.0,
                "{r:?}"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let _ = child.kill().await;
    }
}
