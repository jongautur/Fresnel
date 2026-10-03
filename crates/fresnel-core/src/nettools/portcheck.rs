//! TCP port check: a timed connect to each port the user listed. Open (the
//! handshake completed), closed (the host answered with a reset), filtered
//! (no answer before the timeout: a firewall dropped it, or the host is
//! down), unreachable (an ICMP error or no route).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use super::binding::WifiBinding;
use super::ping::tcp_probe;
use super::{Cancel, ProbeOutcome, TestError, TestResult};
use crate::{Result, WifiError};

/// Results JSON layout version, stored with each run.
pub const PORT_CHECK_RESULTS_VERSION: u32 = 1;
pub const MAX_PORTS: usize = 1024;
/// Connects in flight at once.
pub const CONCURRENCY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortState {
    Open,
    Closed,
    Filtered,
    Unreachable,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOutcome {
    pub port: u16,
    pub state: PortState,
    /// Connect time for open ports, and for closed ones where the reset
    /// came back as one round trip (not on Windows, which retries first).
    pub connect_ms: Option<f64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortCheckResult {
    pub version: u32,
    pub timeout_ms: u64,
    /// Ascending by port; with `stopped`, only the ports checked so far.
    pub ports: Vec<PortOutcome>,
    pub open: u32,
    pub closed: u32,
    pub filtered: u32,
    pub other: u32,
    /// Ports asked for (the list may be shorter when stopped).
    pub requested: u32,
    pub stopped: bool,
}

/// Parse `22, 80,443 8000-8010`: commas or spaces, ranges with `-`.
/// Sorted, without duplicates, at most [`MAX_PORTS`].
pub fn parse_ports(spec: &str) -> Result<Vec<u16>> {
    let bad = |part: &str| {
        WifiError::InvalidInput(format!(
            "“{part}” is not a port or range (use e.g. 22, 80, 8000-8100)"
        ))
    };
    let port = |text: &str, part: &str| -> Result<u16> {
        match text.trim().parse::<u16>() {
            Ok(0) | Err(_) => Err(bad(part)),
            Ok(p) => Ok(p),
        }
    };
    let mut ports = Vec::new();
    for part in spec
        .split([',', ' ', '\n', '\t'])
        .filter(|p| !p.trim().is_empty())
    {
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (port(a, part)?, port(b, part)?);
                if a > b {
                    return Err(bad(part));
                }
                if usize::from(b - a) >= MAX_PORTS {
                    return Err(too_many());
                }
                ports.extend(a..=b);
            }
            None => ports.push(port(part, part)?),
        }
        if ports.len() > MAX_PORTS * 2 {
            return Err(too_many());
        }
    }
    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() {
        return Err(WifiError::InvalidInput("enter at least one port".into()));
    }
    if ports.len() > MAX_PORTS {
        return Err(too_many());
    }
    Ok(ports)
}

fn too_many() -> WifiError {
    WifiError::InvalidInput(format!("at most {MAX_PORTS} ports per check"))
}

fn outcome(port: u16, probe: ProbeOutcome) -> PortOutcome {
    let (state, connect_ms, detail) = match probe {
        ProbeOutcome::Reply { rtt_ms } => (PortState::Open, Some(rtt_ms), None),
        ProbeOutcome::Refused { rtt_ms } => (PortState::Closed, rtt_ms, None),
        ProbeOutcome::Timeout => (PortState::Filtered, None, None),
        ProbeOutcome::Unreachable { detail } => (PortState::Unreachable, None, Some(detail)),
        ProbeOutcome::Error { detail } => (PortState::Error, None, Some(detail)),
    };
    PortOutcome {
        port,
        state,
        connect_ms,
        detail,
    }
}

/// Check `ports` on `target`, [`CONCURRENCY`] at a time, reporting each as
/// it finishes. Cancelling returns the ports checked so far (`stopped`).
pub async fn port_check(
    binding: Option<&WifiBinding>,
    target: IpAddr,
    ports: &[u16],
    timeout: Duration,
    cancel: &Cancel,
    mut on_port: Box<dyn FnMut(&PortOutcome) + Send>,
) -> TestResult<PortCheckResult> {
    if ports.is_empty() || ports.len() > MAX_PORTS {
        return Err(too_many().into());
    }
    if timeout.is_zero() || timeout > Duration::from_secs(10) {
        return Err(WifiError::InvalidInput("port timeout must be up to 10 s".into()).into());
    }
    cancel.check()?;
    let mut checks = stream::iter(ports.iter().copied())
        .map(|port| async move {
            let probe = tcp_probe(binding, SocketAddr::new(target, port), timeout).await;
            (port, probe)
        })
        .buffer_unordered(CONCURRENCY);
    let mut done = Vec::with_capacity(ports.len());
    let mut stopped = false;
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => { stopped = true; break; }
            next = checks.next() => next,
        };
        let Some((port, probe)) = next else { break };
        let o = match probe {
            Ok(probe) => outcome(port, probe),
            // The socket couldn't be created or bound: nothing else will work.
            Err(TestError::Failed(e)) => return Err(e.into()),
            Err(TestError::Cancelled) => {
                stopped = true;
                break;
            }
        };
        on_port(&o);
        done.push(o);
    }
    done.sort_by_key(|o| o.port);
    let count = |s: PortState| done.iter().filter(|o| o.state == s).count() as u32;
    let (open, closed, filtered) = (
        count(PortState::Open),
        count(PortState::Closed),
        count(PortState::Filtered),
    );
    Ok(PortCheckResult {
        version: PORT_CHECK_RESULTS_VERSION,
        timeout_ms: timeout.as_millis() as u64,
        other: done.len() as u32 - open - closed - filtered,
        open,
        closed,
        filtered,
        requested: ports.len() as u32,
        stopped,
        ports: done,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_lists_parse() {
        assert_eq!(parse_ports("443, 22 80,22").unwrap(), vec![22, 80, 443]);
        assert_eq!(
            parse_ports("8000-8003").unwrap(),
            vec![8000, 8001, 8002, 8003]
        );
        for bad in ["", "0", "65536", "80-79", "http", "1-2000", "1-"] {
            assert!(parse_ports(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_ports("1-1024").unwrap().len(), 1024);
        assert!(parse_ports("1-1024, 2000").is_err());
    }

    #[tokio::test]
    async fn open_and_closed_ports() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                drop(s);
            }
        });
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let seen = std::sync::Arc::new(std::sync::Mutex::new(0));
        let counter = seen.clone();
        let r = port_check(
            None,
            "127.0.0.1".parse().unwrap(),
            &[open, closed],
            // Windows retries a reset connect for about 2 s first.
            Duration::from_secs(5),
            &Cancel::never(),
            Box::new(move |_| *counter.lock().unwrap() += 1),
        )
        .await
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), 2);
        assert_eq!((r.open, r.closed, r.requested, r.stopped), (1, 1, 2, false));
        let state = |p| r.ports.iter().find(|o| o.port == p).unwrap().state;
        assert_eq!(
            (state(open), state(closed)),
            (PortState::Open, PortState::Closed)
        );
    }
}
