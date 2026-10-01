//! Exercise the core against real hardware without the GUI.
//!
//!   cargo run -p fresnel-core --example probe            # list + cached results
//!   cargo run -p fresnel-core --example probe -- --scan  # trigger a fresh scan
//!   cargo run -p fresnel-core --example probe -- --scan --json  # scan result as JSON
//!   RUST_LOG=fresnel_core=debug cargo run -p fresnel-core --example probe

use std::sync::Arc;

use fresnel_core::wifi::models::ScanRequest;
use fresnel_core::wifi::scanner::Scanner;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fresnel_core=info".into()),
        )
        .init();

    let trigger = std::env::args().any(|a| a == "--scan");
    let scanner = Scanner::new(Arc::new(fresnel_core::default_registry()));

    let listing = scanner.registry().list_adapters().await;
    for issue in &listing.issues {
        eprintln!(
            "provider {} failed: [{}] {}",
            issue.provider,
            issue.error.kind(),
            issue.error
        );
    }
    if listing.adapters.is_empty() {
        eprintln!("no adapters");
        return;
    }

    for a in &listing.adapters {
        println!("{}", serde_json::to_string_pretty(a).unwrap());

        match scanner.current_connection(&a.id).await {
            Ok(Some(c)) => println!("connection: {}", serde_json::to_string_pretty(&c).unwrap()),
            Ok(None) => println!("connection: none"),
            Err(e) => println!("connection error: [{}] {e}", e.kind()),
        }

        match scanner
            .scan(
                &a.id,
                &ScanRequest {
                    trigger,
                    ssids: vec![],
                },
            )
            .await
        {
            Ok(r) if std::env::args().any(|a| a == "--json") => {
                println!("{}", serde_json::to_string(&r).unwrap());
            }
            Ok(r) => {
                println!(
                    "\nscan: triggered={} notice={:?} {} BSSIDs in {} ms",
                    r.scan_triggered,
                    r.notice,
                    r.access_points.len(),
                    (r.completed_at - r.started_at).num_milliseconds()
                );
                println!(
                    "{:<24} {:<17} {:>6} {:>4} {:>4} {:>5} {:>5} {:>6} {:<9} {:<18} {:>5} {:>4} {:>4} Conn",
                    "SSID", "BSSID", "dBm", "Sig%", "Ch", "MHz", "Width", "Centre", "PHY", "Security", "Age", "Util", "STAs",
                );
                let opt = |v: Option<String>| v.unwrap_or_else(|| "-".into());
                for ap in &r.access_points {
                    println!(
                        "{:<24} {:<17} {:>6} {:>4} {:>4} {:>5} {:>5} {:>6} {:<9} {:<18} {:>5} {:>4} {:>4} {}",
                        ap.ssid.as_deref().unwrap_or("<hidden>"),
                        ap.bssid,
                        opt(ap.signal.dbm.map(|d| format!("{d:.0}"))),
                        opt(ap.signal.quality_percent.map(|q| q.to_string())),
                        opt(ap.channel.map(|c| c.to_string())),
                        ap.frequency_mhz,
                        opt(ap.channel_width_mhz.map(|w| w.to_string())),
                        opt(ap.channel_center_mhz.map(|c| c.to_string())),
                        opt(ap.phy_type.clone()),
                        format!("{:?}", ap.security.kind),
                        opt(ap.last_seen_age_ms.map(|m| format!("{:.1}s", m as f64 / 1000.0))),
                        opt(ap.channel_utilization_pct.map(|u| format!("{u:.0}%"))),
                        opt(ap.station_count.map(|c| c.to_string())),
                        if ap.is_connected { "*" } else { "" },
                    );
                }
            }
            Err(e) => println!("scan error: [{}] {e}", e.kind()),
        }
    }
}
