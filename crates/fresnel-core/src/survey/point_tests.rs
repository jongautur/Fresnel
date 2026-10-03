//! "+ run tests": after a point's RF scan is stored, the active tests the
//! user configured run from the same spot, one after another, each stored
//! as its own row whether it worked or not:
//!
//! 1. ping the Wi-Fi gateway (always),
//! 2. ping the extra host (if set),
//! 3. iperf3 upload and/or download (if a server is set).
//!
//! Each test snapshots the link before and after (roamed = BSSID changed)
//! and is refused, with the reason stored, if its route would not use the
//! Wi-Fi interface. Cancelling stops the running test (stored as
//! cancelled) and skips the rest.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use tracing::{info, warn};

use super::models::{
    LinkSnapshot, NewPointTest, PointTest, PointTestKind, PointTestMethod, PointTestResults,
    PointTestRole, PointTestStatus,
};
use crate::database::Database;
use crate::error::{Result, WifiError};
use crate::nettools::settings::TestSettings;
use crate::nettools::{
    self, check_route, iperf3, wifi_binding, Cancel, Iperf3Config, Iperf3Direction, PingResult,
    ProbeMethod, TestError, WifiBinding,
};
use crate::wifi::scanner::Scanner;

/// Tests describe the spot the user stands on, so they only run for a
/// point measured moments ago.
pub const MAX_POINT_AGE_MIN: i64 = 10;

/// One planned test.
struct Planned {
    kind: PointTestKind,
    role: PointTestRole,
    /// `None`: the gateway isn't known.
    target: Option<IpAddr>,
}

/// How one test ended.
struct Outcome {
    method: PointTestMethod,
    status: PointTestStatus,
    results: Option<PointTestResults>,
    error: Option<WifiError>,
}

impl Outcome {
    fn failed(method: PointTestMethod, error: WifiError) -> Self {
        Self {
            method,
            status: PointTestStatus::Failed,
            results: None,
            error: Some(error),
        }
    }

    fn from_error(method: PointTestMethod, e: TestError) -> Self {
        match e {
            TestError::Cancelled => Self {
                method,
                status: PointTestStatus::Cancelled,
                results: None,
                error: None,
            },
            TestError::Failed(e) => Self::failed(method, e),
        }
    }
}

/// Run the configured tests for a point just measured. Errors only when
/// nothing could run at all (point gone or too old, not connected);
/// individual test failures are stored rows.
pub async fn run_point_tests(
    scanner: &Scanner,
    db: Arc<Database>,
    point_id: i64,
    settings: TestSettings,
    cancel: &Cancel,
) -> Result<Vec<PointTest>> {
    let settings = settings.validated()?;
    let lookup = db.clone();
    let (adapter_id, measured_at) =
        tokio::task::spawn_blocking(move || lookup.point_measurement(point_id))
            .await
            .map_err(|e| WifiError::Backend(format!("database task failed: {e}")))??
            .ok_or_else(|| WifiError::InvalidInput(format!("point {point_id} no longer exists")))?;
    if Utc::now() - measured_at > chrono::Duration::minutes(MAX_POINT_AGE_MIN) {
        return Err(WifiError::InvalidInput(format!(
            "tests describe the spot you stand on, so they run only right after measuring a \
             point (within {MAX_POINT_AGE_MIN} minutes)"
        )));
    }
    let connection = scanner
        .current_connection(&adapter_id)
        .await?
        .ok_or_else(|| WifiError::AdapterUnavailable {
            id: adapter_id.to_string(),
            reason: "not connected to a Wi-Fi network; active tests need a connection".into(),
        })?;
    let binding = wifi_binding(&connection).await?;
    let mut link = LinkSnapshot::from_connection(Some(&connection));

    let mut stored = Vec::new();
    for planned in plan(&settings, &binding) {
        if cancel.is_cancelled() {
            break;
        }
        let started_at = Utc::now();
        let clock = Instant::now();
        let outcome = execute(&planned, &binding, &settings, cancel).await;
        let duration_ms = clock.elapsed().as_millis() as i64;
        let after = match scanner.current_connection(&adapter_id).await {
            Ok(c) => Some(LinkSnapshot::from_connection(c.as_ref())),
            Err(e) => {
                warn!(error = %e, "cannot read the link after a test");
                None
            }
        };
        let target = describe_target(&planned, &settings);
        match &outcome.error {
            Some(e) => warn!(point = point_id, target, error = %e, "point test failed"),
            None => info!(point = point_id, target, status = ?outcome.status, "point test done"),
        }
        let cancelled = outcome.status == PointTestStatus::Cancelled;
        let new = NewPointTest {
            point_id,
            kind: planned.kind,
            target,
            role: planned.role,
            method: outcome.method,
            status: outcome.status,
            started_at,
            duration_ms,
            adapter_id: adapter_id.clone(),
            roamed: link.roamed_to(after.as_ref()),
            link: link.clone(),
            link_after: after.clone(),
            results: outcome.results,
            error_hint: outcome
                .error
                .as_ref()
                .and_then(|e| e.hint())
                .map(str::to_owned),
            error: outcome.error.map(|e| e.to_string()),
        };
        stored.push(insert(&db, new).await?);
        if let Some(after) = after {
            link = after;
        }
        if cancelled {
            break;
        }
    }
    Ok(stored)
}

fn plan(settings: &TestSettings, binding: &WifiBinding) -> Vec<Planned> {
    let mut plan = vec![Planned {
        kind: PointTestKind::Ping,
        role: PointTestRole::Gateway,
        target: binding.gateway_v4.map(IpAddr::V4),
    }];
    if let Some(host) = settings.extra_host_ip() {
        plan.push(Planned {
            kind: PointTestKind::Ping,
            role: PointTestRole::ExtraHost,
            target: Some(host),
        });
    }
    if let Some(server) = settings
        .iperf3_server_ip()
        .filter(|_| settings.iperf3_in_point_tests)
    {
        for direction in settings.iperf3_directions.list() {
            plan.push(Planned {
                kind: PointTestKind::Iperf3,
                role: match direction {
                    Iperf3Direction::Upload => PointTestRole::Iperf3Upload,
                    Iperf3Direction::Download => PointTestRole::Iperf3Download,
                },
                target: Some(server),
            });
        }
    }
    plan
}

fn describe_target(planned: &Planned, settings: &TestSettings) -> String {
    match (planned.kind, planned.target) {
        (_, None) => "gateway".into(),
        (PointTestKind::Ping, Some(ip)) => ip.to_string(),
        (PointTestKind::Iperf3, Some(ip)) => SocketAddr::new(ip, settings.iperf3_port).to_string(),
    }
}

async fn execute(
    planned: &Planned,
    binding: &WifiBinding,
    settings: &TestSettings,
    cancel: &Cancel,
) -> Outcome {
    // A ping that can't start is labelled with the method it would try first.
    let method = match planned.kind {
        PointTestKind::Ping => PointTestMethod::Icmp,
        PointTestKind::Iperf3 => PointTestMethod::Iperf3Tcp,
    };
    let Some(target) = planned.target else {
        return Outcome::failed(
            method,
            WifiError::AdapterUnavailable {
                id: binding.iface.clone(),
                reason: "the Wi-Fi interface has no IPv4 gateway".into(),
            }
            .with_hint(
                "The network gave this computer no default gateway (static address, or DHCP \
                 without a router option). Set an extra host to test instead.",
            ),
        );
    };
    if let Err(e) = check_route(binding, target).await {
        return Outcome::failed(method, e);
    }
    match planned.kind {
        PointTestKind::Ping => {
            match nettools::ping::ping(binding, target, settings.ping_config(), cancel).await {
                Ok(r) => ping_outcome(r, planned.role, target),
                Err(e) => Outcome::from_error(method, e),
            }
        }
        PointTestKind::Iperf3 => {
            let config = Iperf3Config {
                server: SocketAddr::new(target, settings.iperf3_port),
                streams: settings.iperf3_streams,
                duration: settings.iperf3_duration(),
                omit: settings.iperf3_omit(),
                direction: if planned.role == PointTestRole::Iperf3Download {
                    Iperf3Direction::Download
                } else {
                    Iperf3Direction::Upload
                },
            };
            match iperf3::iperf3_tcp(binding, config, cancel).await {
                Ok(r) => Outcome {
                    method,
                    status: PointTestStatus::Ok,
                    results: Some(PointTestResults::Iperf3(r)),
                    error: None,
                },
                Err(e) => Outcome::from_error(method, e),
            }
        }
    }
}

/// A ping that got no reply at all is a failed test (results kept), with
/// the likely reasons.
fn ping_outcome(r: PingResult, role: PointTestRole, target: IpAddr) -> Outcome {
    let method = match r.method {
        ProbeMethod::Icmp => PointTestMethod::Icmp,
        ProbeMethod::TcpConnect => PointTestMethod::TcpConnect,
    };
    let error = (r.received == 0).then(|| {
        let hint = match (r.method, role) {
            (ProbeMethod::TcpConnect, _) => format!(
                "ICMP isn't allowed for this user, so TCP port {} was timed instead and nothing \
                 answered on it. Pick a port the host listens on in Settings, or allow \
                 unprivileged ping (net.ipv4.ping_group_range).",
                r.port.unwrap_or_default()
            ),
            (ProbeMethod::Icmp, PointTestRole::Gateway) => {
                "Some routers and access points don't answer ping, or the Wi-Fi link dropped."
                    .into()
            }
            (ProbeMethod::Icmp, _) => {
                "Windows computers block inbound ping by default (Windows Defender Firewall rule \
                 “File and Printer Sharing (Echo Request – ICMPv4-In)”). The host may also be \
                 off, or a firewall may filter ICMP."
                    .into()
            }
        };
        WifiError::Timeout(format!("no replies from {target} ({} sent)", r.sent)).with_hint(hint)
    });
    Outcome {
        method,
        status: if error.is_some() {
            PointTestStatus::Failed
        } else {
            PointTestStatus::Ok
        },
        results: Some(PointTestResults::Ping(r)),
        error,
    }
}

async fn insert(db: &Arc<Database>, new: NewPointTest) -> Result<PointTest> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || db.insert_point_test(&new))
        .await
        .map_err(|e| WifiError::Backend(format!("database task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nettools::ProbeOutcome;

    #[test]
    fn plan_follows_settings() {
        let binding = WifiBinding::linux("wlan0", None);
        let settings = TestSettings::default();
        let p = plan(&settings, &binding);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].role, p[0].target), (PointTestRole::Gateway, None));
        assert_eq!(describe_target(&p[0], &settings), "gateway");
        let settings = TestSettings {
            extra_host: Some("1.1.1.1".into()),
            iperf3_server: Some("192.168.1.10".into()),
            ..TestSettings::default()
        };
        let roles: Vec<_> = plan(&settings, &binding).iter().map(|p| p.role).collect();
        let without_iperf3 = TestSettings {
            iperf3_in_point_tests: false,
            ..settings.clone()
        };
        assert_eq!(plan(&without_iperf3, &binding).len(), 2);
        assert_eq!(
            roles,
            [
                PointTestRole::Gateway,
                PointTestRole::ExtraHost,
                PointTestRole::Iperf3Upload,
                PointTestRole::Iperf3Download
            ]
        );
        let p = plan(&settings, &binding);
        assert_eq!(describe_target(&p[2], &settings), "192.168.1.10:5201");
    }

    #[test]
    fn total_loss_is_a_failure_with_reasons() {
        let r = PingResult::from_probes(ProbeMethod::Icmp, None, None, vec![ProbeOutcome::Timeout]);
        let o = ping_outcome(r, PointTestRole::ExtraHost, "10.0.0.5".parse().unwrap());
        assert_eq!(o.status, PointTestStatus::Failed);
        assert!(o.results.is_some(), "the probes are kept");
        assert!(o.error.unwrap().hint().unwrap().contains("Windows"));
        let r = PingResult::from_probes(
            ProbeMethod::TcpConnect,
            Some(80),
            None,
            vec![ProbeOutcome::Timeout],
        );
        let o = ping_outcome(r, PointTestRole::Gateway, "10.0.0.1".parse().unwrap());
        assert_eq!(o.method, PointTestMethod::TcpConnect);
        assert!(o.error.unwrap().hint().unwrap().contains("TCP port 80"));
    }

    mod with_provider {
        use std::sync::Arc;

        use super::super::*;
        use crate::adapters::fake::{FakeProvider, Step};
        use crate::adapters::AdapterRegistry;
        use crate::database::projects::NewProject;
        use crate::survey::models::*;
        use crate::wifi::models::{AdapterId, ConnectionInfo, Signal};

        fn connection(id: &AdapterId, iface: &str, bssid: &str) -> ConnectionInfo {
            ConnectionInfo {
                adapter_id: id.clone(),
                interface_name: Some(iface.into()),
                ssid: Some("Net".into()),
                bssid: Some(bssid.into()),
                frequency_mhz: Some(5180),
                channel: Some(36),
                band: None,
                channel_width_mhz: Some(80),
                signal: Signal {
                    dbm: Some(-60.0),
                    quality_percent: None,
                },
                bitrate_kbps: None,
                tx_rate: None,
                rx_rate: None,
                security: None,
                ipv4_addresses: vec![],
                ipv4_gateway: Some("127.0.0.1".into()),
            }
        }

        struct Rig {
            fake: Arc<FakeProvider>,
            scanner: Scanner,
            db: Arc<Database>,
            floor: i64,
            point: i64,
            adapter: AdapterId,
        }

        fn rig(measured_at: chrono::DateTime<Utc>) -> Rig {
            let fake = Arc::new(FakeProvider::new(&["lo"]));
            let scanner = Scanner::new(Arc::new(AdapterRegistry::new(vec![fake.clone()])));
            let db = Database::open_in_memory().unwrap();
            let p = db
                .create_project(&NewProject {
                    name: "HQ".into(),
                    customer_name: None,
                })
                .unwrap();
            let b = db
                .create_building(&NewBuilding {
                    project_id: p.id,
                    name: "Main".into(),
                })
                .unwrap();
            let f = db
                .create_floor(&NewFloor {
                    building_id: b.id,
                    name: "Ground".into(),
                    level: 0,
                })
                .unwrap();
            db.set_floor_plan(
                f.id,
                &FloorPlan {
                    file: "plan-1.png".into(),
                    mime: "image/png".into(),
                    width: 100.0,
                    height: 100.0,
                },
            )
            .unwrap();
            let adapter = FakeProvider::adapter_id("lo");
            let point = db
                .insert_survey_point(&NewSurveyPoint {
                    floor_id: f.id,
                    x: 1.0,
                    y: 1.0,
                    measured_at,
                    scan_duration_ms: 3000,
                    adapter: MeasuringAdapter {
                        id: adapter.clone(),
                        provider: "fake".into(),
                        model: None,
                        driver: None,
                        hw_id: None,
                    },
                    adapter_bands: None,
                    anomalies: vec![],
                    samples: vec![],
                })
                .unwrap();
            Rig {
                fake,
                scanner,
                db: Arc::new(db),
                floor: f.id,
                point: point.id,
                adapter,
            }
        }

        #[tokio::test]
        async fn refuses_old_points_and_disconnected_adapters() {
            let r = rig(Utc::now() - chrono::Duration::minutes(30));
            let e = run_point_tests(
                &r.scanner,
                r.db.clone(),
                r.point,
                TestSettings::default(),
                &Cancel::never(),
            )
            .await
            .unwrap_err();
            assert!(e.to_string().contains("right after measuring"), "{e}");

            let r = rig(Utc::now());
            r.fake.push_connection(Step::Return(None));
            let e = run_point_tests(
                &r.scanner,
                r.db.clone(),
                r.point,
                TestSettings::default(),
                &Cancel::never(),
            )
            .await
            .unwrap_err();
            assert_eq!(e.kind(), "adapter_unavailable", "{e}");
            let e = run_point_tests(
                &r.scanner,
                r.db.clone(),
                999,
                TestSettings::default(),
                &Cancel::never(),
            )
            .await
            .unwrap_err();
            assert!(e.to_string().contains("no longer exists"));
        }

        /// Loopback isn't in the main route table, so every test is refused
        /// on the route check and stored as failed, with link snapshots.
        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn every_test_is_stored_even_when_refused() {
            let r = rig(Utc::now());
            for bssid in [
                "AA:00:00:00:00:01",
                "AA:00:00:00:00:02",
                "AA:00:00:00:00:02",
                "AA:00:00:00:00:02",
            ] {
                r.fake
                    .push_connection(Step::Return(Some(connection(&r.adapter, "lo", bssid))));
            }
            let settings = TestSettings {
                iperf3_server: Some("127.0.0.1".into()),
                ..TestSettings::default()
            };
            let tests = run_point_tests(
                &r.scanner,
                r.db.clone(),
                r.point,
                settings,
                &Cancel::never(),
            )
            .await
            .unwrap();
            assert_eq!(tests.len(), 3);
            for t in &tests {
                assert_eq!(t.status, PointTestStatus::Failed, "{t:?}");
                assert!(t.error.as_deref().unwrap().contains("127.0.0.1"), "{t:?}");
            }
            assert_eq!(tests[0].role, PointTestRole::Gateway);
            assert_eq!(tests[0].method, PointTestMethod::Icmp);
            assert_eq!(tests[0].roamed, Some(true));
            assert_eq!(tests[1].roamed, Some(false));
            assert_eq!(tests[2].target, "127.0.0.1:5201");
            assert_eq!(tests[2].method, PointTestMethod::Iperf3Tcp);
            assert_eq!(r.db.list_floor_point_tests(r.floor).unwrap(), tests);
        }

        #[tokio::test]
        async fn cancelled_before_start_runs_nothing() {
            let r = rig(Utc::now());
            r.fake.push_connection(Step::Return(Some(connection(
                &r.adapter,
                "lo",
                "AA:00:00:00:00:01",
            ))));
            let (tx, cancel) = Cancel::new();
            tx.send(true).unwrap();
            let result = run_point_tests(
                &r.scanner,
                r.db.clone(),
                r.point,
                TestSettings::default(),
                &cancel,
            )
            .await;
            // Windows can't bind the fake adapter (no such interface).
            if cfg!(windows) {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_empty());
            }
            assert!(r.db.list_floor_point_tests(r.floor).unwrap().is_empty());
        }
    }
}
