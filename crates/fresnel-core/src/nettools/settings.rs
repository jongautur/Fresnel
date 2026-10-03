//! What "+ run tests" runs: a small JSON file in the app data directory,
//! replaced atomically. The user names every target; nothing has a default
//! server.

use std::fs;
use std::io::{self, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::iperf3::{self, Iperf3Direction};
use super::ping::PingConfig;
use crate::{Result, WifiError};

pub const SETTINGS_FILE: &str = "active-tests.json";
const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Iperf3Directions {
    Upload,
    Download,
    Both,
}

impl Iperf3Directions {
    pub fn list(self) -> &'static [Iperf3Direction] {
        match self {
            Self::Upload => &[Iperf3Direction::Upload],
            Self::Download => &[Iperf3Direction::Download],
            Self::Both => &[Iperf3Direction::Upload, Iperf3Direction::Download],
        }
    }
}

/// The gateway is always pinged; the rest is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TestSettings {
    pub version: u32,
    /// Echo requests per target.
    pub ping_count: u32,
    /// Port for TCP-connect timing when ICMP isn't allowed.
    pub tcp_port: u16,
    /// Another host to ping (IP address).
    pub extra_host: Option<String>,
    /// The iperf3 server (IP address), shared by the Tools page and point
    /// tests; `None`: none set up.
    pub iperf3_server: Option<String>,
    /// "+ run tests" includes iperf3 (when a server is set). Files from
    /// before this setting ran it whenever a server was set: default on.
    pub iperf3_in_point_tests: bool,
    pub iperf3_port: u16,
    pub iperf3_streams: u8,
    pub iperf3_duration_s: u64,
    pub iperf3_omit_s: u64,
    pub iperf3_directions: Iperf3Directions,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self {
            version: FILE_VERSION,
            ping_count: 10,
            tcp_port: 80,
            extra_host: None,
            iperf3_server: None,
            iperf3_in_point_tests: true,
            iperf3_port: iperf3::DEFAULT_PORT,
            iperf3_streams: 4,
            iperf3_duration_s: 5,
            iperf3_omit_s: 1,
            iperf3_directions: Iperf3Directions::Both,
        }
    }
}

impl TestSettings {
    /// Trimmed and checked; blank optional addresses become `None`.
    pub fn validated(mut self) -> Result<Self> {
        self.version = FILE_VERSION;
        self.extra_host = ip_text("extra host", self.extra_host)?;
        self.iperf3_server = ip_text("iperf3 server", self.iperf3_server)?;
        if !(1..=100).contains(&self.ping_count) {
            return Err(WifiError::InvalidInput("ping count must be 1–100".into()));
        }
        if self.tcp_port == 0 || self.iperf3_port == 0 {
            return Err(WifiError::InvalidInput("ports must be 1–65535".into()));
        }
        if !(1..=iperf3::MAX_STREAMS).contains(&self.iperf3_streams) {
            return Err(WifiError::InvalidInput(format!(
                "iperf3 streams must be 1–{}",
                iperf3::MAX_STREAMS
            )));
        }
        if !(1..=iperf3::MAX_DURATION_S).contains(&self.iperf3_duration_s) {
            return Err(WifiError::InvalidInput(format!(
                "iperf3 duration must be 1–{} s",
                iperf3::MAX_DURATION_S
            )));
        }
        if self.iperf3_omit_s > iperf3::MAX_OMIT_S {
            return Err(WifiError::InvalidInput(format!(
                "iperf3 omit must be 0–{} s",
                iperf3::MAX_OMIT_S
            )));
        }
        Ok(self)
    }

    pub fn ping_config(&self) -> PingConfig {
        PingConfig {
            count: Some(self.ping_count),
            tcp_port: self.tcp_port,
            ..PingConfig::default()
        }
    }

    pub fn extra_host_ip(&self) -> Option<IpAddr> {
        self.extra_host.as_deref().and_then(|h| h.parse().ok())
    }

    pub fn iperf3_server_ip(&self) -> Option<IpAddr> {
        self.iperf3_server.as_deref().and_then(|h| h.parse().ok())
    }

    pub fn iperf3_duration(&self) -> Duration {
        Duration::from_secs(self.iperf3_duration_s)
    }

    pub fn iperf3_omit(&self) -> Duration {
        Duration::from_secs(self.iperf3_omit_s)
    }

    /// Missing file: defaults. A damaged file or invalid values are logged
    /// and replaced by defaults; settings are never a reason to fail.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(SETTINGS_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot read test settings; using defaults");
                return Self::default();
            }
        };
        match serde_json::from_slice::<Self>(&bytes).map_err(|e| e.to_string()) {
            Ok(s) => s.validated().unwrap_or_else(|e| {
                warn!(path = %path.display(), error = %e, "invalid test settings; using defaults");
                Self::default()
            }),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "test settings file is damaged; using defaults");
                Self::default()
            }
        }
    }

    /// Validate and store; returns what was stored.
    pub fn save(self, dir: &Path) -> Result<Self> {
        let settings = self.validated()?;
        let json = serde_json::to_vec_pretty(&settings)
            .map_err(|e| WifiError::Backend(format!("cannot encode test settings: {e}")))?;
        fs::create_dir_all(dir)
            .map_err(|e| WifiError::Backend(format!("cannot create {}: {e}", dir.display())))?;
        let path = dir.join(SETTINGS_FILE);
        replace(&path, &json)
            .map_err(|e| WifiError::Backend(format!("cannot save {}: {e}", path.display())))?;
        Ok(settings)
    }
}

fn ip_text(what: &str, value: Option<String>) -> Result<Option<String>> {
    let Some(v) = value else { return Ok(None) };
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let ip: IpAddr = v.parse().map_err(|_| {
        WifiError::InvalidInput(format!(
            "{what} must be an IP address such as 192.168.1.10 (host names aren't looked up)"
        ))
    })?;
    if ip.is_unspecified() || ip.is_multicast() {
        return Err(WifiError::InvalidInput(format!(
            "{what} must be a single host's address"
        )));
    }
    Ok(Some(ip.to_string()))
}

/// Write a sibling temporary file, sync it, rename it over `path`: readers
/// see the old file or the new one, never a part.
fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp: PathBuf = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let write = || -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fresnel-test-settings-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn round_trip_defaults_and_damage() {
        let dir = temp_dir("rt");
        assert_eq!(TestSettings::load(&dir), TestSettings::default());
        let saved = TestSettings {
            extra_host: Some(" 1.1.1.1 ".into()),
            iperf3_server: Some("".into()),
            iperf3_directions: Iperf3Directions::Upload,
            ..TestSettings::default()
        }
        .save(&dir)
        .unwrap();
        assert_eq!(saved.extra_host.as_deref(), Some("1.1.1.1"));
        assert_eq!(saved.iperf3_server, None);
        assert_eq!(TestSettings::load(&dir), saved);
        // No temporary files left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::write(dir.join(SETTINGS_FILE), b"{ not json").unwrap();
        assert_eq!(TestSettings::load(&dir), TestSettings::default());
        // Unknown fields from a newer version and missing fields are tolerated.
        fs::write(dir.join(SETTINGS_FILE), br#"{"pingCount": 3, "future": 1}"#).unwrap();
        assert_eq!(TestSettings::load(&dir).ping_count, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validation() {
        let bad = |s: TestSettings| s.validated().is_err();
        assert!(bad(TestSettings {
            iperf3_server: Some("iperf.example.com".into()),
            ..Default::default()
        }));
        assert!(bad(TestSettings {
            extra_host: Some("0.0.0.0".into()),
            ..Default::default()
        }));
        assert!(bad(TestSettings {
            iperf3_streams: 0,
            ..Default::default()
        }));
        assert!(bad(TestSettings {
            iperf3_duration_s: 0,
            ..Default::default()
        }));
        assert!(bad(TestSettings {
            ping_count: 0,
            ..Default::default()
        }));
        let ok = TestSettings {
            iperf3_server: Some("fd00::10".into()),
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert_eq!(ok.iperf3_server_ip(), Some("fd00::10".parse().unwrap()));
        assert_eq!(Iperf3Directions::Both.list().len(), 2);
    }
}
