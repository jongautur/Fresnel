//! Active tests per survey point (ping, iperf3).

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};

use super::survey::{enum_parse, enum_text};
use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::survey::models::*;
use crate::wifi::models::AdapterId;

const COLUMNS: &str = "t.id, t.point_id, t.kind, t.target, t.role, t.method, t.status, \
    t.started_at, t.duration_ms, t.adapter_id, t.connected, t.iface, t.bssid, t.freq, \
    t.signal_dbm, t.tx_kbps, t.rx_kbps, t.phy, t.mcs, t.nss, t.width_mhz, t.link_after_json, \
    t.roamed, t.results_json, t.error, t.error_hint";

impl Database {
    /// Store one test. Separate from the point's own insert, so an
    /// unreachable target can never lose a completed RF measurement.
    pub fn insert_point_test(&self, new: &NewPointTest) -> Result<PointTest> {
        let link_after = new.link_after.as_ref().map(to_json).transpose()?;
        let results = new.results.as_ref().map(to_json).transpose()?;
        let conn = self.conn();
        let l = &new.link;
        let inserted = conn.execute(
            "INSERT INTO point_tests (point_id, kind, target, role, method, status, started_at,
                 duration_ms, adapter_id, connected, iface, bssid, freq, signal_dbm, tx_kbps,
                 rx_kbps, phy, mcs, nss, width_mhz, link_after_json, roamed, results_json,
                 error, error_hint)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                    ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
             WHERE EXISTS (SELECT 1 FROM survey_points WHERE id = ?1)",
            params![
                new.point_id,
                enum_text(&new.kind),
                new.target,
                enum_text(&new.role),
                enum_text(&new.method),
                enum_text(&new.status),
                new.started_at.to_rfc3339(),
                new.duration_ms.max(0),
                new.adapter_id.as_str(),
                l.connected,
                l.iface,
                l.bssid,
                l.frequency_mhz,
                l.signal_dbm,
                l.tx_kbps,
                l.rx_kbps,
                l.phy,
                l.mcs,
                l.nss,
                l.width_mhz,
                link_after,
                new.roamed,
                results,
                new.error,
                new.error_hint,
            ],
        )?;
        if inserted == 0 {
            return Err(WifiError::InvalidInput(format!(
                "point {} no longer exists",
                new.point_id
            )));
        }
        let id = conn.last_insert_rowid();
        Ok(conn.query_row(
            &format!("SELECT {COLUMNS} FROM point_tests t WHERE t.id = ?1"),
            [id],
            row,
        )?)
    }

    /// The adapter that measured a point and when, if the point exists.
    pub fn point_measurement(&self, point_id: i64) -> Result<Option<(AdapterId, DateTime<Utc>)>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT adapter_id, measured_at FROM survey_points WHERE id = ?1",
                [point_id],
                |r| Ok((AdapterId(r.get(0)?), parse_ts(r, 1)?)),
            )
            .optional()?)
    }

    /// Every test on a floor's points, oldest first: what the point details,
    /// the floor's test table and the report show.
    pub fn list_floor_point_tests(&self, floor_id: i64) -> Result<Vec<PointTest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM point_tests t
             JOIN survey_points p ON p.id = t.point_id
             WHERE p.floor_id = ?1
             ORDER BY t.started_at, t.id"
        ))?;
        let rows = stmt
            .query_map([floor_id], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v)
        .map_err(|e| WifiError::Database(format!("cannot encode test data: {e}")))
}

fn row(r: &Row<'_>) -> rusqlite::Result<PointTest> {
    let id: i64 = r.get(0)?;
    // A newer Fresnel's layout (or a damaged value) is shown as "no
    // results", never as made-up numbers; the row itself stays visible.
    let decode = |idx: usize, what: &str| -> rusqlite::Result<Option<serde_json::Value>> {
        let text: Option<String> = r.get(idx)?;
        Ok(text.and_then(|t| match serde_json::from_str(&t) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(test = id, error = %e, "unreadable point test {what}");
                None
            }
        }))
    };
    Ok(PointTest {
        id,
        point_id: r.get(1)?,
        kind: enum_parse(&r.get::<_, String>(2)?, PointTestKind::Ping),
        target: r.get(3)?,
        role: enum_parse(&r.get::<_, String>(4)?, PointTestRole::ExtraHost),
        method: enum_parse(&r.get::<_, String>(5)?, PointTestMethod::Icmp),
        status: enum_parse(&r.get::<_, String>(6)?, PointTestStatus::Failed),
        started_at: parse_ts(r, 7)?,
        duration_ms: r.get(8)?,
        adapter_id: AdapterId(r.get(9)?),
        link: LinkSnapshot {
            connected: r.get(10)?,
            iface: r.get(11)?,
            bssid: r.get(12)?,
            frequency_mhz: r.get(13)?,
            signal_dbm: r.get(14)?,
            tx_kbps: r.get(15)?,
            rx_kbps: r.get(16)?,
            phy: r.get(17)?,
            mcs: r.get(18)?,
            nss: r.get(19)?,
            width_mhz: r.get(20)?,
        },
        link_after: typed(id, decode(21, "link")?, "link"),
        roamed: r.get(22)?,
        results: typed(id, decode(23, "results")?, "results"),
        error: r.get(24)?,
        error_hint: r.get(25)?,
    })
}

fn typed<T: serde::de::DeserializeOwned>(
    id: i64,
    v: Option<serde_json::Value>,
    what: &str,
) -> Option<T> {
    v.and_then(|v| match serde_json::from_value(v) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(test = id, error = %e, "unreadable point test {what}");
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::projects::NewProject;
    use crate::nettools::{PingResult, ProbeMethod, ProbeOutcome};

    fn point(db: &Database) -> (i64, i64) {
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
        let point = db
            .insert_survey_point(&NewSurveyPoint {
                floor_id: f.id,
                x: 1.0,
                y: 2.0,
                measured_at: Utc::now(),
                scan_duration_ms: 3000,
                adapter: MeasuringAdapter {
                    id: AdapterId::linux("wlan0"),
                    provider: "networkmanager".into(),
                    model: None,
                    driver: None,
                    hw_id: None,
                },
                adapter_bands: None,
                anomalies: vec![],
                samples: vec![],
            })
            .unwrap();
        (f.id, point.id)
    }

    fn new_test(point_id: i64) -> NewPointTest {
        let link = LinkSnapshot {
            connected: true,
            iface: Some("wlan0".into()),
            bssid: Some("AA:BB:CC:00:00:01".into()),
            frequency_mhz: Some(5180),
            signal_dbm: Some(-55.0),
            tx_kbps: Some(866_700),
            rx_kbps: None,
            phy: Some("VHT".into()),
            mcs: Some(9),
            nss: Some(2),
            width_mhz: Some(80),
        };
        let after = LinkSnapshot {
            bssid: Some("AA:BB:CC:00:00:02".into()),
            ..link.clone()
        };
        NewPointTest {
            point_id,
            kind: PointTestKind::Ping,
            target: "192.168.1.1".into(),
            role: PointTestRole::Gateway,
            method: PointTestMethod::Icmp,
            status: PointTestStatus::Ok,
            started_at: Utc::now(),
            duration_ms: 2500,
            adapter_id: AdapterId::linux("wlan0"),
            roamed: link.roamed_to(Some(&after)),
            link_after: Some(after),
            link,
            results: Some(PointTestResults::Ping(PingResult::from_probes(
                ProbeMethod::Icmp,
                None,
                None,
                vec![ProbeOutcome::Reply { rtt_ms: 3.5 }, ProbeOutcome::Timeout],
            ))),
            error: None,
            error_hint: None,
        }
    }

    #[test]
    fn round_trip_listing_and_cascade() {
        let db = Database::open_in_memory().unwrap();
        let (floor, point_id) = point(&db);
        let stored = db.insert_point_test(&new_test(point_id)).unwrap();
        assert_eq!(stored.roamed, Some(true));
        assert_eq!(stored.link.mcs, Some(9));
        let Some(PointTestResults::Ping(p)) = &stored.results else {
            panic!("{stored:?}")
        };
        assert_eq!((p.sent, p.received, p.loss_percent), (2, 1, 50.0));

        let failed = NewPointTest {
            kind: PointTestKind::Iperf3,
            role: PointTestRole::Iperf3Download,
            method: PointTestMethod::Iperf3Tcp,
            status: PointTestStatus::Failed,
            target: "192.168.1.10:5201".into(),
            link_after: None,
            roamed: None,
            results: None,
            error: Some("the iperf3 server is busy with another test".into()),
            error_hint: Some("try again".into()),
            ..new_test(point_id)
        };
        db.insert_point_test(&failed).unwrap();
        let all = db.list_floor_point_tests(floor).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], stored);
        assert_eq!(all[1].roamed, None);
        assert_eq!(all[1].error_hint.as_deref(), Some("try again"));

        // The serialised shape the UI and the report rely on.
        let json = serde_json::to_value(&all[0]).unwrap();
        assert_eq!(json["role"], "gateway");
        assert_eq!(json["results"]["type"], "ping");
        assert_eq!(json["results"]["lossPercent"], 50.0);
        assert_eq!(json["linkAfter"]["bssid"], "AA:BB:CC:00:00:02");

        assert!(db.delete_survey_point(point_id).unwrap());
        assert!(db.list_floor_point_tests(floor).unwrap().is_empty());
        let e = db.insert_point_test(&new_test(point_id)).unwrap_err();
        assert!(e.to_string().contains("no longer exists"));
    }

    #[test]
    fn unreadable_results_are_dropped_not_invented() {
        let db = Database::open_in_memory().unwrap();
        let (floor, point_id) = point(&db);
        let t = db.insert_point_test(&new_test(point_id)).unwrap();
        db.conn()
            .execute(
                "UPDATE point_tests SET results_json = '{\"type\":\"ping\",\"version\":99}' WHERE id = ?1",
                [t.id],
            )
            .unwrap();
        let all = db.list_floor_point_tests(floor).unwrap();
        assert_eq!(all[0].results, None);
        assert_eq!(all[0].status, PointTestStatus::Ok);
    }
}
