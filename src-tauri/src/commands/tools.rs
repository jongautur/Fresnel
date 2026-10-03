//! The Tools page: ping, traceroute, DNS, port check and iperf3, each
//! streaming live events over a channel and returning the stored run;
//! plus the run history.

use std::sync::Arc;

use fresnel_core::nettools::dns::{self, SystemDnsServer};
use fresnel_core::nettools::Cancel;
use fresnel_core::tools::run::{
    self, DnsParams, Events, Iperf3Params, PingParams, PortCheckParams, ToolContext, ToolEvent,
    TraceParams, Via,
};
use fresnel_core::tools::{ToolKind, ToolRun};
use fresnel_core::WifiError;
use tauri::ipc::Channel;
use tauri::State;

use super::with_db;
use crate::state::AppState;

/// Unregisters a run however its command ends.
struct RunGuard<'a> {
    state: &'a AppState,
    id: String,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.state.finish_tool(&self.id);
    }
}

/// Register the run and forward its events to the channel. `run_id`
/// (chosen by the UI) is what `cancel_tool` takes.
fn start<'a>(
    state: &'a AppState,
    run_id: &str,
    via: &Via,
    on_event: Channel<ToolEvent>,
) -> Result<(RunGuard<'a>, ToolContext<'a>, Cancel, Events), WifiError> {
    let db = state.db()?;
    let cancel = state.begin_tool(run_id, matches!(via, Via::Wifi { .. }))?;
    let guard = RunGuard {
        state,
        id: run_id.to_owned(),
    };
    let events: Events = Arc::new(move |e: ToolEvent| {
        // The window may have gone; the run is stored either way.
        let _ = on_event.send(e);
    });
    let ctx = ToolContext {
        scanner: &state.scanner,
        db,
    };
    Ok((guard, ctx, cancel, events))
}

fn logged(result: Result<ToolRun, WifiError>) -> Result<ToolRun, WifiError> {
    result.inspect_err(|e| tracing::info!(kind = e.kind(), error = %e, "tool not run"))
}

#[tauri::command]
pub async fn tool_ping(
    state: State<'_, AppState>,
    run_id: String,
    params: PingParams,
    on_event: Channel<ToolEvent>,
) -> Result<ToolRun, WifiError> {
    let (_guard, ctx, cancel, events) = start(&state, &run_id, &params.via, on_event)?;
    logged(run::run_ping(&ctx, params, &cancel, events).await)
}

#[tauri::command]
pub async fn tool_traceroute(
    state: State<'_, AppState>,
    run_id: String,
    params: TraceParams,
    on_event: Channel<ToolEvent>,
) -> Result<ToolRun, WifiError> {
    let (_guard, ctx, cancel, events) = start(&state, &run_id, &params.via, on_event)?;
    logged(run::run_traceroute(&ctx, params, &cancel, events).await)
}

#[tauri::command]
pub async fn tool_dns(
    state: State<'_, AppState>,
    run_id: String,
    params: DnsParams,
    on_event: Channel<ToolEvent>,
) -> Result<ToolRun, WifiError> {
    let (_guard, ctx, cancel, events) = start(&state, &run_id, &params.via, on_event)?;
    logged(run::run_dns(&ctx, params, &cancel, events).await)
}

#[tauri::command]
pub async fn tool_port_check(
    state: State<'_, AppState>,
    run_id: String,
    params: PortCheckParams,
    on_event: Channel<ToolEvent>,
) -> Result<ToolRun, WifiError> {
    let (_guard, ctx, cancel, events) = start(&state, &run_id, &params.via, on_event)?;
    logged(run::run_port_check(&ctx, params, &cancel, events).await)
}

#[tauri::command]
pub async fn tool_iperf3(
    state: State<'_, AppState>,
    run_id: String,
    params: Iperf3Params,
    on_event: Channel<ToolEvent>,
) -> Result<ToolRun, WifiError> {
    let (_guard, ctx, cancel, events) = start(&state, &run_id, &params.via, on_event)?;
    logged(run::run_iperf3(&ctx, params, &cancel, events).await)
}

/// Stop a run; it is stored with what it measured so far.
#[tauri::command]
pub fn cancel_tool(state: State<'_, AppState>, run_id: String) -> bool {
    state.cancel_tool(&run_id)
}

#[tauri::command]
pub async fn dns_system_servers() -> Result<Vec<SystemDnsServer>, WifiError> {
    dns::system_servers().await
}

#[tauri::command]
pub async fn list_tool_runs(
    state: State<'_, AppState>,
    kind: Option<ToolKind>,
    limit: Option<u32>,
) -> Result<Vec<ToolRun>, WifiError> {
    with_db(&state, move |db| {
        db.list_tool_runs(kind, limit.unwrap_or(100))
    })
    .await
}

#[tauri::command]
pub async fn get_tool_run(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Option<ToolRun>, WifiError> {
    with_db(&state, move |db| db.get_tool_run(id)).await
}

#[tauri::command]
pub async fn delete_tool_run(state: State<'_, AppState>, id: i64) -> Result<(), WifiError> {
    with_db(&state, move |db| db.delete_tool_run(id)).await
}

/// Runs attached to survey points are kept.
#[tauri::command]
pub async fn clear_tool_runs(
    state: State<'_, AppState>,
    kind: Option<ToolKind>,
) -> Result<usize, WifiError> {
    with_db(&state, move |db| db.clear_tool_runs(kind)).await
}

#[tauri::command]
pub async fn attach_tool_run(
    state: State<'_, AppState>,
    id: i64,
    point_id: Option<i64>,
) -> Result<ToolRun, WifiError> {
    with_db(&state, move |db| db.attach_tool_run(id, point_id)).await
}

#[tauri::command]
pub async fn list_floor_tool_runs(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<Vec<ToolRun>, WifiError> {
    with_db(&state, move |db| db.list_floor_tool_runs(floor_id)).await
}
