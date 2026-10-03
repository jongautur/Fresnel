//! Stored Tools page runs.

use rusqlite::{params, OptionalExtension, Row};

use super::survey::{enum_parse, enum_text};
use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::tools::{NewToolRun, ToolKind, ToolRun, ToolRunStatus, KEEP_RUNS_PER_TOOL};

const COLUMNS: &str = "t.id, t.kind, t.target, t.resolved_ip, t.params_json, t.status, \
    t.started_at, t.duration_ms, t.route_iface, t.adapter_id, t.link_json, t.link_after_json, \
    t.roamed, t.summary, t.error, t.error_hint, t.point_id";

impl Database {
    /// Store a run, then prune the oldest runs of that tool beyond
    /// [`KEEP_RUNS_PER_TOOL`] (runs attached to a survey point are kept).
    pub fn insert_tool_run(&self, new: &NewToolRun) -> Result<ToolRun> {
        let link = new.link.as_ref().map(to_json).transpose()?;
        let link_after = new.link_after.as_ref().map(to_json).transpose()?;
        let results = new.results.as_ref().map(to_json).transpose()?;
        let params = to_json(&new.params)?;
        let kind = enum_text(&new.kind);
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tool_runs (kind, target, resolved_ip, params_json, status, started_at,
                 duration_ms, route_iface, adapter_id, link_json, link_after_json, roamed,
                 summary, results_json, error, error_hint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                kind,
                new.target,
                new.resolved_ip,
                params,
                enum_text(&new.status),
                new.started_at.to_rfc3339(),
                new.duration_ms.max(0),
                new.route_iface,
                new.adapter_id,
                link,
                link_after,
                new.roamed,
                new.summary,
                results,
                new.error,
                new.error_hint,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "DELETE FROM tool_runs WHERE kind = ?1 AND point_id IS NULL AND id NOT IN (
                 SELECT id FROM tool_runs WHERE kind = ?1 AND point_id IS NULL
                 ORDER BY started_at DESC, id DESC LIMIT ?2)",
            params![kind, KEEP_RUNS_PER_TOOL as i64],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_tool_run(id)?
            .ok_or_else(|| WifiError::Database(format!("tool run {id} vanished after insert")))
    }

    /// One run with its results.
    pub fn get_tool_run(&self, id: i64) -> Result<Option<ToolRun>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {COLUMNS}, t.results_json FROM tool_runs t WHERE t.id = ?1"),
                [id],
                |r| {
                    let mut run = row(r)?;
                    run.results = decode(run.id, r.get(17)?, "results");
                    Ok(run)
                },
            )
            .optional()?)
    }

    /// Recent runs of one tool (or all), newest first, without results.
    pub fn list_tool_runs(&self, kind: Option<ToolKind>, limit: u32) -> Result<Vec<ToolRun>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM tool_runs t
             WHERE ?1 IS NULL OR t.kind = ?1
             ORDER BY t.started_at DESC, t.id DESC LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(
                params![kind.as_ref().map(enum_text), i64::from(limit.min(1000))],
                row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Runs attached to the points of a floor, oldest first, with results:
    /// for point details and the report.
    pub fn list_floor_tool_runs(&self, floor_id: i64) -> Result<Vec<ToolRun>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS}, t.results_json FROM tool_runs t
             JOIN survey_points p ON p.id = t.point_id
             WHERE p.floor_id = ?1
             ORDER BY t.started_at, t.id"
        ))?;
        let rows = stmt
            .query_map([floor_id], |r| {
                let mut run = row(r)?;
                run.results = decode(run.id, r.get(17)?, "results");
                Ok(run)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Attach a run to a survey point, or detach it (`None`).
    pub fn attach_tool_run(&self, id: i64, point_id: Option<i64>) -> Result<ToolRun> {
        {
            let conn = self.conn();
            if let Some(point) = point_id {
                if !super::survey::exists(&conn, "survey_points", point)? {
                    return Err(WifiError::InvalidInput(format!(
                        "point {point} no longer exists"
                    )));
                }
            }
            if conn.execute(
                "UPDATE tool_runs SET point_id = ?2 WHERE id = ?1",
                params![id, point_id],
            )? == 0
            {
                return Err(WifiError::InvalidInput(format!(
                    "tool run {id} no longer exists"
                )));
            }
        }
        self.get_tool_run(id)?
            .ok_or_else(|| WifiError::InvalidInput(format!("tool run {id} no longer exists")))
    }

    pub fn delete_tool_run(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM tool_runs WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Delete a tool's history (all tools: `None`), except runs attached to
    /// survey points. Returns how many were deleted.
    pub fn clear_tool_runs(&self, kind: Option<ToolKind>) -> Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM tool_runs WHERE point_id IS NULL AND (?1 IS NULL OR kind = ?1)",
            [kind.as_ref().map(enum_text)],
        )?)
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|e| WifiError::Database(format!("cannot encode tool run data: {e}")))
}

/// Damaged JSON is shown as missing, never as made-up values.
fn decode<T: serde::de::DeserializeOwned>(id: i64, text: Option<String>, what: &str) -> Option<T> {
    text.and_then(|t| match serde_json::from_str(&t) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(run = id, error = %e, "unreadable tool run {what}");
            None
        }
    })
}

fn row(r: &Row<'_>) -> rusqlite::Result<ToolRun> {
    let id: i64 = r.get(0)?;
    Ok(ToolRun {
        id,
        kind: enum_parse(&r.get::<_, String>(1)?, ToolKind::Ping),
        target: r.get(2)?,
        resolved_ip: r.get(3)?,
        params: decode(id, r.get(4)?, "params").unwrap_or(serde_json::Value::Null),
        status: enum_parse(&r.get::<_, String>(5)?, ToolRunStatus::Failed),
        started_at: parse_ts(r, 6)?,
        duration_ms: r.get(7)?,
        route_iface: r.get(8)?,
        adapter_id: r.get(9)?,
        link: decode(id, r.get(10)?, "link"),
        link_after: decode(id, r.get(11)?, "link"),
        roamed: r.get(12)?,
        summary: r.get(13)?,
        results: None,
        error: r.get(14)?,
        error_hint: r.get(15)?,
        point_id: r.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::database::point_tests::tests::point;

    fn new_run(kind: ToolKind) -> NewToolRun {
        NewToolRun {
            kind,
            target: "router.lan".into(),
            resolved_ip: Some("192.168.1.1".into()),
            params: serde_json::json!({ "count": 10 }),
            status: ToolRunStatus::Ok,
            started_at: Utc::now(),
            duration_ms: 2500,
            route_iface: Some("wlan0".into()),
            adapter_id: None,
            link: None,
            link_after: None,
            roamed: None,
            summary: Some("2.1 ms avg, 0 % loss".into()),
            results: Some(serde_json::json!({ "version": 1, "sent": 10 })),
            error: None,
            error_hint: None,
        }
    }

    #[test]
    fn store_list_attach_and_clear() {
        let db = Database::open_in_memory().unwrap();
        let run = db.insert_tool_run(&new_run(ToolKind::Ping)).unwrap();
        assert_eq!(run.results.as_ref().unwrap()["sent"], 10);
        assert_eq!(run.params["count"], 10);
        db.insert_tool_run(&new_run(ToolKind::Dns)).unwrap();

        let pings = db.list_tool_runs(Some(ToolKind::Ping), 50).unwrap();
        assert_eq!(pings.len(), 1);
        assert!(pings[0].results.is_none(), "listings leave results out");
        assert_eq!(pings[0].summary.as_deref(), Some("2.1 ms avg, 0 % loss"));
        assert_eq!(db.list_tool_runs(None, 50).unwrap().len(), 2);

        let (floor, point_id) = point(&db);
        let attached = db.attach_tool_run(run.id, Some(point_id)).unwrap();
        assert_eq!(attached.point_id, Some(point_id));
        assert!(db.attach_tool_run(run.id, Some(point_id + 100)).is_err());
        let on_floor = db.list_floor_tool_runs(floor).unwrap();
        assert_eq!(on_floor.len(), 1);
        assert!(on_floor[0].results.is_some());

        // Clearing keeps runs attached to the survey.
        assert_eq!(db.clear_tool_runs(None).unwrap(), 1);
        assert_eq!(db.list_tool_runs(None, 50).unwrap().len(), 1);

        // Deleting the point detaches the run.
        db.delete_survey_point(point_id).unwrap();
        assert_eq!(db.get_tool_run(run.id).unwrap().unwrap().point_id, None);
        db.delete_tool_run(run.id).unwrap();
        assert!(db.get_tool_run(run.id).unwrap().is_none());
    }

    #[test]
    fn history_is_pruned_per_tool() {
        let db = Database::open_in_memory().unwrap();
        let (_, point_id) = point(&db);
        let first = db.insert_tool_run(&new_run(ToolKind::Ping)).unwrap();
        db.attach_tool_run(first.id, Some(point_id)).unwrap();
        for _ in 0..KEEP_RUNS_PER_TOOL + 5 {
            db.insert_tool_run(&new_run(ToolKind::Ping)).unwrap();
        }
        db.insert_tool_run(&new_run(ToolKind::Dns)).unwrap();
        let pings = db.list_tool_runs(Some(ToolKind::Ping), 1000).unwrap();
        // The attached one is kept on top of the limit.
        assert_eq!(pings.len(), KEEP_RUNS_PER_TOOL + 1);
        assert!(pings.iter().any(|r| r.id == first.id));
        assert_eq!(db.list_tool_runs(Some(ToolKind::Dns), 10).unwrap().len(), 1);
    }
}
