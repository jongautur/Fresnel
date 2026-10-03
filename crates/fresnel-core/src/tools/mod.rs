//! The Tools page's stored runs (ping, traceroute, DNS, port check,
//! iperf3): what was run, against what, over which interface, and the
//! result as versioned JSON. Separate from the per-point tests; a run can
//! be attached to a survey point afterwards.

pub mod run;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::survey::models::LinkSnapshot;

/// Runs kept per tool; older ones (not attached to a survey point) are
/// pruned when a new one is stored.
pub const KEEP_RUNS_PER_TOOL: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Ping,
    Traceroute,
    Dns,
    PortCheck,
    Iperf3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRunStatus {
    Ok,
    Failed,
    /// The user stopped it; the results cover what ran until then.
    Stopped,
}

/// A stored run. In listings `results` is left out (`None`); fetch the
/// run by ID for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRun {
    pub id: i64,
    pub kind: ToolKind,
    /// As the user typed it.
    pub target: String,
    /// The address actually used (after resolving a name).
    pub resolved_ip: Option<String>,
    /// The settings the run used, as the UI sent them (for "run again").
    pub params: serde_json::Value,
    pub status: ToolRunStatus,
    pub started_at: DateTime<Utc>,
    pub duration_ms: i64,
    /// The interface the traffic left through.
    pub route_iface: Option<String>,
    /// Set when the run was bound to a Wi-Fi adapter.
    pub adapter_id: Option<String>,
    /// The Wi-Fi link at the start and end (Wi-Fi-bound runs, or when the
    /// route used a Wi-Fi interface the provider knows).
    pub link: Option<LinkSnapshot>,
    pub link_after: Option<LinkSnapshot>,
    pub roamed: Option<bool>,
    /// One line for history lists ("4.1 ms avg, 0 % loss").
    pub summary: Option<String>,
    /// Versioned JSON (a `version` field inside). A layout this Fresnel
    /// doesn't know is still returned; the UI checks the version.
    pub results: Option<serde_json::Value>,
    pub error: Option<String>,
    pub error_hint: Option<String>,
    /// The survey point the run is attached to.
    pub point_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewToolRun {
    pub kind: ToolKind,
    pub target: String,
    pub resolved_ip: Option<String>,
    pub params: serde_json::Value,
    pub status: ToolRunStatus,
    pub started_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub route_iface: Option<String>,
    pub adapter_id: Option<String>,
    pub link: Option<LinkSnapshot>,
    pub link_after: Option<LinkSnapshot>,
    pub roamed: Option<bool>,
    pub summary: Option<String>,
    pub results: Option<serde_json::Value>,
    pub error: Option<String>,
    pub error_hint: Option<String>,
}
