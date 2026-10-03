//! Active network tests, run from a survey point over the Wi-Fi interface:
//! latency/jitter/loss (`ping`) and TCP throughput (`iperf3`, a built-in
//! client for any standard iperf3 server).
//!
//! Every test is bound to the Wi-Fi interface (Linux `SO_BINDTODEVICE`,
//! Windows the interface's source address) and refused when the route to
//! the target leaves through another interface (Ethernet, VPN). Every
//! network phase has its own deadline, and every test checks a
//! cancellation token at each probe or data-loop boundary.
//!
//! `ping-async` was evaluated: it fits echo packets, but doesn't expose the
//! interface binding, source address or cancellation boundaries we need, so
//! the platform code lives here (Linux ping sockets, Windows `IcmpSendEcho2Ex`
//! / `Icmp6SendEcho2`).

mod binding;
pub mod dns;
pub mod iperf3;
pub mod ping;
pub mod portcheck;
pub mod route;
pub mod settings;
pub mod target;
pub mod traceroute;
#[cfg(windows)]
mod windows;

use std::time::Duration;

use tokio::sync::watch;

use crate::WifiError;

pub use binding::{wifi_binding, WifiBinding};
pub use iperf3::{Iperf3Config, Iperf3Direction, Iperf3Result};
pub use ping::{PingConfig, PingResult, ProbeMethod, ProbeOutcome};
pub use route::check_route;
pub use target::{egress, resolve, Egress, IpFamily, ResolvedTarget};

/// Connecting a TCP socket (control, data stream or TCP-connect probe).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Reading a route table or interface list, or creating a socket.
pub const OS_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How a test ended without a result.
#[derive(Debug, Clone)]
pub enum TestError {
    /// The user cancelled it.
    Cancelled,
    Failed(WifiError),
}

impl From<WifiError> for TestError {
    fn from(e: WifiError) -> Self {
        Self::Failed(e)
    }
}

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed(e) => e.fmt(f),
        }
    }
}

pub type TestResult<T> = std::result::Result<T, TestError>;

/// A cancellation token: cheap to clone, checked synchronously in blocking
/// probe loops and awaited in async ones.
#[derive(Debug, Clone)]
pub struct Cancel(watch::Receiver<bool>);

impl Cancel {
    /// A token and the sender that cancels it.
    pub fn new() -> (watch::Sender<bool>, Self) {
        let (tx, rx) = watch::channel(false);
        (tx, Self(rx))
    }

    /// A token nobody can cancel (tests, internal calls).
    pub fn never() -> Self {
        Self::new().1
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub fn check(&self) -> TestResult<()> {
        if self.is_cancelled() {
            Err(TestError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Resolves once cancelled; never if the sender is dropped uncancelled.
    pub async fn cancelled(&self) {
        let mut rx = self.0.clone();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Sleep, returning early (with `Cancelled`) if the test is cancelled.
    pub async fn sleep(&self, d: Duration) -> TestResult<()> {
        tokio::select! {
            _ = self.cancelled() => Err(TestError::Cancelled),
            _ = tokio::time::sleep(d) => Ok(()),
        }
    }

    /// Blocking-thread sleep in short slices so cancellation is noticed.
    pub(crate) fn sleep_blocking(&self, d: Duration) -> TestResult<()> {
        let end = std::time::Instant::now() + d;
        loop {
            self.check()?;
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
    }
}

/// Run blocking OS work on the blocking pool with a deadline. A call that
/// overruns is abandoned (its thread finishes on its own) and reported.
pub(crate) async fn blocking<T, F>(what: &str, deadline: Duration, f: F) -> Result<T, WifiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, WifiError> + Send + 'static,
{
    match tokio::time::timeout(deadline, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(WifiError::Backend(format!("{what} failed: {e}"))),
        Err(_) => Err(WifiError::Timeout(format!(
            "{what} took longer than {} s",
            deadline.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_token_wakes_waiters_and_sleeps() {
        let (tx, cancel) = Cancel::new();
        assert!(cancel.check().is_ok());
        let waiter = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.sleep(Duration::from_secs(60)).await }
        });
        tx.send(true).unwrap();
        assert!(matches!(waiter.await.unwrap(), Err(TestError::Cancelled)));
        assert!(cancel.is_cancelled());
        assert!(matches!(
            cancel.sleep_blocking(Duration::from_secs(60)),
            Err(TestError::Cancelled)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_sender_never_cancels() {
        let (tx, cancel) = Cancel::new();
        drop(tx);
        assert!(cancel.sleep(Duration::from_millis(10)).await.is_ok());
    }

    #[tokio::test]
    async fn blocking_work_has_a_deadline() {
        let e = blocking("slow call", Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        })
        .await
        .unwrap_err();
        assert_eq!(e.kind(), "timeout");
    }
}
