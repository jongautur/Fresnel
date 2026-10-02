//! Loading a project's survey data for rogue / evil-twin detection, and the
//! readings Measure Here set aside (`point_anomalies`).

use rusqlite::{params, Connection, OptionalExtension};

use super::survey::{enum_parse, enum_text, query_points};
use super::Database;
use crate::error::{Result, WifiError};
use crate::survey::findings::{
    self, AnomalyKind, FindingScope, Findings, FloorRef, LinkOptions, PointAnomaly, ProjectSurvey,
};
use crate::wifi::models::{Band, Signal};

/// Store a point's anomalies; part of the point's insert transaction.
pub(super) fn insert_anomalies(
    conn: &Connection,
    point_id: i64,
    anomalies: &[PointAnomaly],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO point_anomalies (point_id, bssid, kind, frequency_mhz, channel, band,
             ssid_raw, signal_dbm, signal_percent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for a in anomalies {
        stmt.execute(params![
            point_id,
            a.bssid,
            enum_text(&a.kind),
            a.frequency_mhz,
            a.channel,
            enum_text(&a.band),
            a.ssid_raw,
            a.signal.dbm,
            a.signal.quality_percent,
        ])?;
    }
    Ok(())
}

/// SQL condition on `p` (survey_points) for "in project ?1".
const IN_PROJECT: &str = "p.floor_id IN (SELECT f.id FROM floors f
    JOIN buildings b ON b.id = f.building_id WHERE b.project_id = ?1)";

fn project_of(conn: &Connection, scope: &FindingScope) -> Result<i64> {
    let (sql, id, what) = match *scope {
        FindingScope::Project { id } => ("SELECT id FROM projects WHERE id = ?1", id, "project"),
        FindingScope::Building { id } => (
            "SELECT project_id FROM buildings WHERE id = ?1",
            id,
            "building",
        ),
        FindingScope::Floor { id } => (
            "SELECT b.project_id FROM floors f JOIN buildings b ON b.id = f.building_id
             WHERE f.id = ?1",
            id,
            "floor",
        ),
    };
    conn.query_row(sql, [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| WifiError::InvalidInput(format!("{what} {id} no longer exists")))
}

fn load(conn: &Connection, project_id: i64) -> Result<ProjectSurvey> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.name, b.id, b.name FROM floors f
         JOIN buildings b ON b.id = f.building_id
         WHERE b.project_id = ?1 ORDER BY b.name COLLATE NOCASE, b.id, f.level, f.id",
    )?;
    let floors = stmt
        .query_map([project_id], |r| {
            Ok(FloorRef {
                id: r.get(0)?,
                name: r.get(1)?,
                building_id: r.get(2)?,
                building_name: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let points = query_points(conn, IN_PROJECT, project_id)?;
    let aps = super::aps::query(
        conn,
        "f.building_id IN (SELECT id FROM buildings WHERE project_id = ?1)",
        project_id,
    )?;
    let marks = super::marks::list(conn, project_id)?;

    let mut stmt = conn.prepare(&format!(
        "SELECT a.point_id, a.bssid, a.kind, a.frequency_mhz, a.channel, a.band, a.ssid_raw,
             a.signal_dbm, a.signal_percent
         FROM point_anomalies a JOIN survey_points p ON p.id = a.point_id
         WHERE {IN_PROJECT} ORDER BY a.point_id, a.bssid, a.frequency_mhz"
    ))?;
    let anomalies = stmt
        .query_map([project_id], |r| {
            let kind: String = r.get(2)?;
            let band: String = r.get(5)?;
            Ok((
                r.get(0)?,
                kind,
                PointAnomaly {
                    bssid: r.get(1)?,
                    kind: AnomalyKind::MultiFrequency,
                    frequency_mhz: r.get(3)?,
                    channel: r.get(4)?,
                    band: enum_parse(&band, Band::Unknown),
                    ssid_raw: r.get(6)?,
                    signal: Signal {
                        dbm: r.get(7)?,
                        quality_percent: r.get(8)?,
                    },
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String, PointAnomaly)>>>()?
        .into_iter()
        // Only kinds this build knows (the CHECK allows nothing else yet).
        .filter_map(|(point, kind, mut a)| {
            a.kind = serde_json::from_value(serde_json::Value::String(kind)).ok()?;
            Some((point, a))
        })
        .collect();

    Ok(ProjectSurvey {
        project_id,
        floors,
        points,
        aps,
        marks,
        anomalies,
    })
}

impl Database {
    /// Everything rogue detection reads for one project.
    pub fn project_survey(&self, project_id: i64) -> Result<ProjectSurvey> {
        let mut conn = self.conn();
        // One snapshot: a measurement landing mid-load can't half-appear.
        let tx = conn.transaction()?;
        let data = load(&tx, project_id)?;
        tx.commit()?;
        Ok(data)
    }

    /// Rogue / evil-twin findings for a project, building or floor. Known
    /// BSSIDs and project SSIDs are always decided project-wide. This is
    /// also what the report uses.
    pub fn findings(&self, scope: FindingScope) -> Result<Findings> {
        let project_id = project_of(&self.conn(), &scope)?;
        let data = self.project_survey(project_id)?;
        Ok(findings::analyse(&data, scope))
    }

    /// What the UI needs to place or link `bssid` as one of yours.
    pub fn bssid_link_options(&self, project_id: i64, bssid: &str) -> Result<LinkOptions> {
        let bssid = super::aps::normalize_bssid(bssid)
            .ok_or_else(|| WifiError::InvalidInput(format!("'{bssid}' is not a BSSID")))?;
        project_of(&self.conn(), &FindingScope::Project { id: project_id })?;
        let data = self.project_survey(project_id)?;
        Ok(findings::link_options(&data, &bssid))
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::database::projects::NewProject;
    use crate::survey::findings::{FindingKind, MarkStatus};
    use crate::survey::models::*;
    use crate::wifi::models::{AccessPointObservation, AdapterId, Akm, Cipher, Pmf, SecurityKind};

    fn setup(db: &Database) -> (i64, i64) {
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
                width: 1000.0,
                height: 500.0,
            },
        )
        .unwrap();
        (p.id, f.id)
    }

    fn sample(bssid: &str, ssid: &str, kind: SecurityKind, detail: Option<SampleDetail>) -> Sample {
        Sample {
            bssid: bssid.into(),
            ssid: Some(ssid.into()),
            ssid_raw: ssid.as_bytes().to_vec(),
            frequency_mhz: 5180,
            channel: Some(36),
            band: Band::Band5GHz,
            channel_width_mhz: None,
            channel_center_mhz: None,
            signal: Signal::from_dbm(-60.0),
            security: kind,
            phy_type: None,
            wifi_generation: None,
            noise_dbm: None,
            snr_db: None,
            channel_utilization_pct: None,
            station_count: None,
            last_seen_age_ms: Some(10),
            is_connected: false,
            detail,
        }
    }

    fn point(floor_id: i64, samples: Vec<Sample>, anomalies: Vec<PointAnomaly>) -> NewSurveyPoint {
        NewSurveyPoint {
            adapter_bands: None,
            floor_id,
            x: 10.0,
            y: 10.0,
            measured_at: Utc::now(),
            scan_duration_ms: 3000,
            adapter: MeasuringAdapter {
                id: AdapterId::linux("wlan0"),
                provider: "test".into(),
                model: None,
                driver: None,
                hw_id: None,
            },
            samples,
            anomalies,
        }
    }

    #[test]
    fn detail_round_trips_and_old_rows_read_as_not_recorded() {
        let db = Database::open_in_memory().unwrap();
        let (_, floor) = setup(&db);
        let detail = SampleDetail {
            akms: vec![Akm::Psk, Akm::Sae, Akm::Unknown(0x000F_AC15)],
            pairwise_ciphers: vec![Cipher::Ccmp, Cipher::Gcmp256],
            group_ciphers: vec![Cipher::Tkip],
            group_mgmt_cipher: Some(Cipher::BipCmac128),
            pmf: Some(Pmf::Capable),
            hidden: true,
            mld_address: Some("AA:BB:CC:DD:EE:FF".into()),
        };
        let p = db
            .insert_survey_point(&point(
                floor,
                vec![
                    sample(
                        "AA:00:00:00:00:01",
                        "Corp",
                        SecurityKind::Wpa2Wpa3Personal,
                        Some(detail.clone()),
                    ),
                    sample(
                        "AA:00:00:00:00:02",
                        "Corp",
                        SecurityKind::Wpa2Personal,
                        None,
                    ),
                ],
                vec![],
            ))
            .unwrap();
        let by = |b: &str| {
            p.samples
                .iter()
                .find(|s| s.bssid == b)
                .unwrap()
                .detail
                .clone()
        };
        assert_eq!(by("AA:00:00:00:00:01"), Some(detail));
        assert_eq!(by("AA:00:00:00:00:02"), None);

        // A real observation stores what it carried, nothing invented.
        let mut obs: AccessPointObservation = serde_json::from_value(serde_json::json!({
            "timestamp": "2026-10-01T09:00:00Z", "adapterId": "linux:wlan0",
            "bssid": "AA:00:00:00:00:03", "mldAddress": null, "ssid": "Corp", "ssidRaw": [67,111,114,112],
            "hidden": false, "frequencyMhz": 2412, "channel": 1, "band": "2.4ghz",
            "channelWidthMhz": null, "channelCenterMhz": null,
            "signal": { "dbm": -50.0, "qualityPercent": null },
            "security": { "kind": "wpa2_personal", "privacy": true, "wpa": false, "rsn": true,
                          "akms": ["psk"], "pairwiseCiphers": [], "groupCiphers": [],
                          "groupMgmtCipher": null, "pmf": null },
            "mode": "infrastructure", "maxBitrateKbps": null, "lastSeenAgeMs": 5, "isConnected": false,
            "noiseDbm": null, "snrDb": null, "channelUtilizationPct": null, "stationCount": null,
            "beaconIntervalTu": null, "phyType": null, "wifiGeneration": null
        }))
        .unwrap();
        obs.adapter_id = AdapterId::linux("wlan0");
        let p = db
            .insert_survey_point(&point(floor, vec![Sample::from(&obs)], vec![]))
            .unwrap();
        let d = p.samples[0].detail.clone().unwrap();
        assert_eq!(
            (d.akms, d.pmf, d.group_mgmt_cipher, d.hidden),
            (vec![Akm::Psk], None, None, false)
        );
    }

    #[test]
    fn anomalies_marks_and_findings_end_to_end() {
        let db = Database::open_in_memory().unwrap();
        let (project, floor) = setup(&db);
        let ours = "AA:00:00:00:00:01";
        let rogue = "DE:AD:BE:EF:00:01";
        db.create_placed_ap(
            floor,
            &PlacedApInput {
                name: "AP 1".into(),
                x: 1.0,
                y: 1.0,
                model: None,
                notes: None,
                bssids: vec![ours.into()],
            },
        )
        .unwrap();
        let anomaly = PointAnomaly {
            bssid: ours.into(),
            kind: AnomalyKind::MultiFrequency,
            frequency_mhz: 5500,
            channel: Some(100),
            band: Band::Band5GHz,
            ssid_raw: b"Corp".to_vec(),
            signal: Signal::from_dbm(-70.0),
        };
        let p = db
            .insert_survey_point(&point(
                floor,
                vec![
                    sample(ours, "Corp", SecurityKind::Wpa2Personal, None),
                    sample(rogue, "Corp", SecurityKind::Open, None),
                ],
                vec![anomaly.clone()],
            ))
            .unwrap();
        let data = db.project_survey(project).unwrap();
        assert_eq!(data.anomalies, [(p.id, anomaly)]);

        let f = db.findings(FindingScope::Floor { id: floor }).unwrap();
        let kinds: Vec<_> = f
            .findings
            .iter()
            .map(|x| (x.kind, x.bssid.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (FindingKind::UnknownTransmitter, rogue),
                (FindingKind::SecurityMismatch, rogue),
                (FindingKind::MultiFrequency, ours),
            ]
        );

        // "Neighbour" silences it; clearing the mark brings it back.
        let mark = db
            .set_bssid_mark(
                project,
                "de-ad-be-ef-00-01",
                MarkStatus::Neighbour,
                Some(" café ".into()),
            )
            .unwrap();
        assert_eq!(
            (mark.bssid.as_str(), mark.note.as_deref()),
            (rogue, Some("café"))
        );
        let f = db.findings(FindingScope::Project { id: project }).unwrap();
        assert!(f.findings.iter().all(|x| x.bssid != rogue));
        assert_eq!(f.marks.len(), 1);
        assert!(db.clear_bssid_mark(project, rogue).unwrap());
        assert!(!db.clear_bssid_mark(project, rogue).unwrap());
        assert_eq!(
            db.findings(FindingScope::Project { id: project })
                .unwrap()
                .findings
                .len(),
            3
        );

        // Link options for the rogue; then deleting the point cascades.
        let o = db.bssid_link_options(project, rogue).unwrap();
        assert_eq!(o.strongest_per_floor[0].point_id, p.id);
        assert_eq!(o.aps.len(), 1);
        assert!(db.delete_survey_point(p.id).unwrap());
        assert!(db.project_survey(project).unwrap().anomalies.is_empty());
        assert!(db.findings(FindingScope::Floor { id: floor + 99 }).is_err());
    }
}
