//! Requirement profiles (project-level), their target networks, per-floor
//! overrides, and floor evaluation.

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{params, Connection, ErrorCode, OptionalExtension, Row};
use serde::Serialize;

use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::survey::requirements::*;
use crate::wifi::models::Band;

const COLUMNS: &str = "p.id, p.project_id, p.name, p.preset, p.primary_min_dbm,
    p.secondary_min_dbm, p.cochannel_max, p.cochannel_level_dbm, p.required_bands,
    p.min_snr_db, p.max_util_pct, p.is_default, p.created_at, p.updated_at";

fn band_text(b: Band) -> &'static str {
    match b {
        Band::Band2_4GHz => "2.4ghz",
        Band::Band5GHz => "5ghz",
        Band::Band6GHz => "6ghz",
        Band::Band60GHz => "60ghz",
        Band::Unknown => "unknown",
    }
}

fn bands_to_text(bands: &[Band]) -> String {
    bands
        .iter()
        .map(|b| band_text(*b))
        .collect::<Vec<_>>()
        .join(",")
}

fn bands_from_text(s: &str) -> Vec<Band> {
    s.split(',')
        .filter_map(|t| REQUIRABLE_BANDS.into_iter().find(|b| band_text(*b) == t))
        .collect()
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<RequirementProfile> {
    let preset: String = r.get(3)?;
    let bands: String = r.get(8)?;
    Ok(RequirementProfile {
        id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        preset: Preset::parse(&preset).unwrap_or(Preset::Custom),
        values: RequirementValues {
            primary_min_dbm: r.get(4)?,
            secondary_min_dbm: r.get(5)?,
            cochannel_max: r.get(6)?,
            cochannel_level_dbm: r.get(7)?,
            required_bands: bands_from_text(&bands),
            min_snr_db: r.get(9)?,
            max_util_pct: r.get(10)?,
        },
        is_default: r.get(11)?,
        targets: Vec::new(),
        created_at: parse_ts(r, 12)?,
        updated_at: parse_ts(r, 13)?,
    })
}

/// Profiles matching `filter` (an SQL condition on `p`), targets attached.
fn query(conn: &Connection, filter: &str, id: i64) -> Result<Vec<RequirementProfile>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM requirement_profiles p WHERE {filter}
         ORDER BY p.name COLLATE NOCASE, p.id"
    ))?;
    let mut profiles: Vec<RequirementProfile> = stmt
        .query_map([id], from_row)?
        .collect::<rusqlite::Result<_>>()?;

    let mut stmt = conn.prepare(&format!(
        "SELECT t.profile_id, t.ssid_raw, t.placed_ap_id, a.name,
                (SELECT group_concat(bssid, ',') FROM
                    (SELECT bssid FROM placed_ap_bssids WHERE ap_id = t.placed_ap_id ORDER BY bssid))
         FROM requirement_profile_targets t
         JOIN requirement_profiles p ON p.id = t.profile_id
         LEFT JOIN placed_aps a ON a.id = t.placed_ap_id
         WHERE {filter} ORDER BY t.id"
    ))?;
    let mut by_profile: HashMap<i64, Vec<TargetInfo>> = HashMap::new();
    let rows = stmt.query_map([id], |r| {
        let ssid_raw: Option<Vec<u8>> = r.get(1)?;
        let ap_id: Option<i64> = r.get(2)?;
        let ap_name: Option<String> = r.get(3)?;
        let bssids: Option<String> = r.get(4)?;
        let info = match (ssid_raw, ap_id) {
            (Some(raw), _) => TargetInfo {
                label: String::from_utf8_lossy(&raw).into_owned(),
                target: RequirementTarget::Ssid { ssid_raw: raw },
                bssids: Vec::new(),
            },
            (None, ap_id) => TargetInfo {
                target: RequirementTarget::Ap {
                    ap_id: ap_id.unwrap_or_default(),
                },
                label: ap_name.unwrap_or_default(),
                bssids: bssids
                    .map(|b| b.split(',').map(String::from).collect())
                    .unwrap_or_default(),
            },
        };
        Ok((r.get::<_, i64>(0)?, info))
    })?;
    for row in rows {
        let (profile, info) = row?;
        by_profile.entry(profile).or_default().push(info);
    }
    for p in &mut profiles {
        p.targets = by_profile.remove(&p.id).unwrap_or_default();
    }
    Ok(profiles)
}

fn require_profile(conn: &Connection, id: i64) -> Result<RequirementProfile> {
    query(conn, "p.id = ?1", id)?.pop().ok_or_else(|| {
        WifiError::InvalidInput(format!("requirement profile {id} no longer exists"))
    })
}

fn touch_project(conn: &Connection, project_id: i64, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE projects SET updated_at = ?2 WHERE id = ?1",
        params![project_id, now],
    )?;
    Ok(())
}

/// A duplicate name is the user's mistake, not a database failure.
fn name_conflict(e: rusqlite::Error, name: &str) -> WifiError {
    match e.sqlite_error_code() {
        Some(ErrorCode::ConstraintViolation) if e.to_string().contains("UNIQUE") => {
            WifiError::InvalidInput(format!("a profile named '{name}' already exists"))
        }
        _ => e.into(),
    }
}

/// (project, building, override profile) of a floor.
fn floor_context(conn: &Connection, floor_id: i64) -> Result<(i64, i64, Option<i64>)> {
    conn.query_row(
        "SELECT b.project_id, f.building_id, f.requirement_profile_id
         FROM floors f JOIN buildings b ON b.id = f.building_id WHERE f.id = ?1",
        [floor_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| WifiError::InvalidInput(format!("floor {floor_id} no longer exists")))
}

/// An SSID heard somewhere in the project, for the target picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SsidOption {
    pub ssid_raw: Vec<u8>,
    /// Lossy UTF-8, for display.
    pub label: String,
    /// Survey points (any floor of the project) that heard it.
    pub points: i64,
}

/// A placed AP of the project, for the target picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApOption {
    pub ap_id: i64,
    pub name: String,
    pub building_name: String,
    pub floor_name: String,
    pub bssid_count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetOptions {
    pub ssids: Vec<SsidOption>,
    pub aps: Vec<ApOption>,
}

impl Database {
    pub fn list_requirement_profiles(&self, project_id: i64) -> Result<Vec<RequirementProfile>> {
        query(&self.conn(), "p.project_id = ?1", project_id)
    }

    /// Create a profile. The project's first profile becomes its default.
    pub fn create_requirement_profile(
        &self,
        project_id: i64,
        input: &RequirementProfileInput,
    ) -> Result<RequirementProfile> {
        let (name, preset, values) = input.validated()?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (exists, others): (bool, i64) = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1),
                    (SELECT COUNT(*) FROM requirement_profiles WHERE project_id = ?1)",
            [project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !exists {
            return Err(WifiError::InvalidInput(format!(
                "project {project_id} no longer exists"
            )));
        }
        let is_default = input.is_default || others == 0;
        if is_default {
            tx.execute(
                "UPDATE requirement_profiles SET is_default = 0 WHERE project_id = ?1",
                [project_id],
            )?;
        }
        tx.execute(
            "INSERT INTO requirement_profiles (project_id, name, preset, primary_min_dbm,
                 secondary_min_dbm, cochannel_max, cochannel_level_dbm, required_bands,
                 min_snr_db, max_util_pct, is_default, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
            params![
                project_id,
                name,
                preset.as_str(),
                values.primary_min_dbm,
                values.secondary_min_dbm,
                values.cochannel_max,
                values.cochannel_level_dbm,
                bands_to_text(&values.required_bands),
                values.min_snr_db,
                values.max_util_pct,
                is_default,
                now,
            ],
        )
        .map_err(|e| name_conflict(e, &name))?;
        let id = tx.last_insert_rowid();
        touch_project(&tx, project_id, &now)?;
        let profile = require_profile(&tx, id)?;
        tx.commit()?;
        Ok(profile)
    }

    /// Replace a profile's values. Clearing `is_default` leaves the project
    /// without a default (floors then need an override).
    pub fn update_requirement_profile(
        &self,
        id: i64,
        input: &RequirementProfileInput,
    ) -> Result<RequirementProfile> {
        let (name, preset, values) = input.validated()?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let project_id = require_profile(&tx, id)?.project_id;
        if input.is_default {
            tx.execute(
                "UPDATE requirement_profiles SET is_default = 0 WHERE project_id = ?1 AND id <> ?2",
                params![project_id, id],
            )?;
        }
        tx.execute(
            "UPDATE requirement_profiles SET name = ?2, preset = ?3, primary_min_dbm = ?4,
                 secondary_min_dbm = ?5, cochannel_max = ?6, cochannel_level_dbm = ?7,
                 required_bands = ?8, min_snr_db = ?9, max_util_pct = ?10, is_default = ?11,
                 updated_at = ?12
             WHERE id = ?1",
            params![
                id,
                name,
                preset.as_str(),
                values.primary_min_dbm,
                values.secondary_min_dbm,
                values.cochannel_max,
                values.cochannel_level_dbm,
                bands_to_text(&values.required_bands),
                values.min_snr_db,
                values.max_util_pct,
                input.is_default,
                now,
            ],
        )
        .map_err(|e| name_conflict(e, &name))?;
        touch_project(&tx, project_id, &now)?;
        let profile = require_profile(&tx, id)?;
        tx.commit()?;
        Ok(profile)
    }

    /// Floors using it as their override fall back to the project default.
    pub fn delete_requirement_profile(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM requirement_profiles WHERE id = ?1", [id])?
            > 0)
    }

    /// Replace a profile's targets. Placed APs must belong to the profile's
    /// project; SSIDs must be 1–32 bytes (a hidden network is targeted
    /// through its placed AP instead). Duplicates are dropped.
    pub fn set_requirement_targets(
        &self,
        profile_id: i64,
        targets: &[RequirementTarget],
    ) -> Result<RequirementProfile> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let project_id = require_profile(&tx, profile_id)?.project_id;
        let mut unique: Vec<&RequirementTarget> = Vec::new();
        for t in targets {
            match t {
                RequirementTarget::Ssid { ssid_raw } if !(1..=32).contains(&ssid_raw.len()) => {
                    return Err(WifiError::InvalidInput(
                        "an SSID target must be 1 to 32 bytes; target a hidden network through \
                         its placed access point"
                            .into(),
                    ));
                }
                RequirementTarget::Ap { ap_id } => {
                    let in_project: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM placed_aps a JOIN floors f ON f.id = a.floor_id
                             JOIN buildings b ON b.id = f.building_id
                             WHERE a.id = ?1 AND b.project_id = ?2)",
                        params![ap_id, project_id],
                        |r| r.get(0),
                    )?;
                    if !in_project {
                        return Err(WifiError::InvalidInput(format!(
                            "access point {ap_id} isn't part of this project"
                        )));
                    }
                }
                RequirementTarget::Ssid { .. } => {}
            }
            if !unique.contains(&t) {
                unique.push(t);
            }
        }
        tx.execute(
            "DELETE FROM requirement_profile_targets WHERE profile_id = ?1",
            [profile_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO requirement_profile_targets (profile_id, ssid_raw, placed_ap_id)
                 VALUES (?1, ?2, ?3)",
            )?;
            for t in unique {
                match t {
                    RequirementTarget::Ssid { ssid_raw } => {
                        stmt.execute(params![profile_id, ssid_raw, None::<i64>])?
                    }
                    RequirementTarget::Ap { ap_id } => {
                        stmt.execute(params![profile_id, None::<Vec<u8>>, ap_id])?
                    }
                };
            }
        }
        tx.execute(
            "UPDATE requirement_profiles SET updated_at = ?2 WHERE id = ?1",
            params![profile_id, now],
        )?;
        touch_project(&tx, project_id, &now)?;
        let profile = require_profile(&tx, profile_id)?;
        tx.commit()?;
        Ok(profile)
    }

    /// Use a whole profile on this floor (`None`: the project default).
    pub fn set_floor_requirement_profile(
        &self,
        floor_id: i64,
        profile_id: Option<i64>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (project_id, _, _) = floor_context(&tx, floor_id)?;
        if let Some(id) = profile_id {
            if require_profile(&tx, id)?.project_id != project_id {
                return Err(WifiError::InvalidInput(
                    "that requirement profile belongs to another project".into(),
                ));
            }
        }
        tx.execute(
            "UPDATE floors SET requirement_profile_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![floor_id, profile_id, now],
        )?;
        touch_project(&tx, project_id, &now)?;
        tx.commit()?;
        Ok(())
    }

    /// Which profile applies to the floor and how each point fares.
    pub fn floor_requirements(&self, floor_id: i64) -> Result<FloorRequirements> {
        let (project_id, building_id, override_id, profile) = {
            let conn = self.conn();
            let (project_id, building_id, override_id) = floor_context(&conn, floor_id)?;
            let profile = match override_id {
                Some(id) => query(&conn, "p.id = ?1", id)?
                    .pop()
                    .map(|p| (p, ProfileSource::FloorOverride)),
                None => query(&conn, "p.project_id = ?1 AND p.is_default = 1", project_id)?
                    .pop()
                    .map(|p| (p, ProfileSource::ProjectDefault)),
            };
            (project_id, building_id, override_id, profile)
        };
        let mut result = FloorRequirements {
            floor_id,
            project_id,
            override_profile_id: override_id,
            profile: None,
            source: None,
            points: Vec::new(),
            summary: None,
        };
        if let Some((profile, source)) = profile {
            let points = self.list_survey_points(floor_id)?;
            let aps = self.list_building_aps(building_id)?;
            let (evals, summary) = evaluate_floor(&profile, &aps, &points);
            result.profile = Some(profile);
            result.source = Some(source);
            result.points = evals;
            result.summary = Some(summary);
        }
        Ok(result)
    }

    /// The profile values and point summary a report embeds for this floor;
    /// `None` when no profile applies.
    pub fn requirements_snapshot(&self, floor_id: i64) -> Result<Option<RequirementsSnapshot>> {
        Ok(self.floor_requirements(floor_id)?.snapshot())
    }

    /// SSIDs heard on any floor of the project, most widely heard first, and
    /// the project's placed APs.
    pub fn requirement_target_options(&self, project_id: i64) -> Result<TargetOptions> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.ssid_raw, COUNT(DISTINCT s.point_id) AS n FROM survey_samples s
             JOIN survey_points p ON p.id = s.point_id
             JOIN floors f ON f.id = p.floor_id
             JOIN buildings b ON b.id = f.building_id
             WHERE b.project_id = ?1 AND length(s.ssid_raw) BETWEEN 1 AND 32
             GROUP BY s.ssid_raw ORDER BY n DESC, s.ssid_raw",
        )?;
        let ssids = stmt
            .query_map([project_id], |r| {
                let raw: Vec<u8> = r.get(0)?;
                Ok(SsidOption {
                    label: String::from_utf8_lossy(&raw).into_owned(),
                    ssid_raw: raw,
                    points: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, b.name, f.name,
                    (SELECT COUNT(*) FROM placed_ap_bssids x WHERE x.ap_id = a.id)
             FROM placed_aps a JOIN floors f ON f.id = a.floor_id
             JOIN buildings b ON b.id = f.building_id
             WHERE b.project_id = ?1
             ORDER BY b.name COLLATE NOCASE, f.level, a.name COLLATE NOCASE, a.id",
        )?;
        let aps = stmt
            .query_map([project_id], |r| {
                Ok(ApOption {
                    ap_id: r.get(0)?,
                    name: r.get(1)?,
                    building_name: r.get(2)?,
                    floor_name: r.get(3)?,
                    bssid_count: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(TargetOptions { ssids, aps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::projects::NewProject;
    use crate::survey::models::*;
    use crate::wifi::models::{AdapterId, Capability, SecurityKind, Signal};

    struct Setup {
        db: Database,
        project: i64,
        floor: i64,
    }

    fn setup() -> Setup {
        let db = Database::open_in_memory().unwrap();
        let project = db
            .create_project(&NewProject {
                name: "P".into(),
                customer_name: None,
            })
            .unwrap()
            .id;
        let building = db
            .create_building(&NewBuilding {
                project_id: project,
                name: "B".into(),
            })
            .unwrap()
            .id;
        let floor = db
            .create_floor(&NewFloor {
                building_id: building,
                name: "F".into(),
                level: 0,
            })
            .unwrap()
            .id;
        db.set_floor_plan(
            floor,
            &FloorPlan {
                file: "plan-1.png".into(),
                mime: "image/png".into(),
                width: 100.0,
                height: 100.0,
            },
        )
        .unwrap();
        Setup { db, project, floor }
    }

    fn input(name: &str, preset: Preset) -> RequirementProfileInput {
        RequirementProfileInput {
            name: name.into(),
            preset,
            values: preset.values().unwrap(),
            is_default: false,
        }
    }

    fn sample(bssid: &str, ssid: &[u8], dbm: f32) -> Sample {
        Sample {
            detail: None,
            bssid: bssid.into(),
            ssid: Some(String::from_utf8_lossy(ssid).into_owned()).filter(|s| !s.is_empty()),
            ssid_raw: ssid.to_vec(),
            frequency_mhz: 5180,
            channel: Some(36),
            band: Band::Band5GHz,
            channel_width_mhz: Some(20),
            channel_center_mhz: Some(5180),
            signal: Signal::from_dbm(dbm),
            security: SecurityKind::Wpa2Personal,
            phy_type: None,
            wifi_generation: None,
            noise_dbm: None,
            snr_db: None,
            channel_utilization_pct: None,
            station_count: None,
            last_seen_age_ms: Some(10),
            is_connected: false,
        }
    }

    fn measure(s: &Setup, samples: Vec<Sample>) -> SurveyPoint {
        s.db.insert_survey_point(&NewSurveyPoint {
            anomalies: vec![],
            floor_id: s.floor,
            x: 10.0,
            y: 10.0,
            measured_at: Utc::now(),
            scan_duration_ms: 3000,
            adapter: MeasuringAdapter {
                id: AdapterId::linux("wlan0"),
                provider: "networkmanager".into(),
                model: None,
                driver: None,
                hw_id: None,
            },
            adapter_bands: Some(AdapterBands {
                band_2ghz: Capability::Supported,
                band_5ghz: Capability::Supported,
                band_6ghz: Capability::Unsupported,
            }),
            samples,
        })
        .unwrap()
    }

    #[test]
    fn profiles_crud_and_a_single_default() {
        let s = setup();
        let a =
            s.db.create_requirement_profile(s.project, &input(" Office ", Preset::OfficeData))
                .unwrap();
        assert_eq!(a.name, "Office");
        assert!(a.is_default, "the first profile becomes the default");
        assert_eq!(a.values, Preset::OfficeData.values().unwrap());

        let mut voice = input("Voice", Preset::VoiceVideo);
        voice.is_default = true;
        let b = s.db.create_requirement_profile(s.project, &voice).unwrap();
        let list = s.db.list_requirement_profiles(s.project).unwrap();
        assert_eq!(
            list.iter()
                .map(|p| (p.id, p.is_default))
                .collect::<Vec<_>>(),
            [(a.id, false), (b.id, true)]
        );

        let err =
            s.db.create_requirement_profile(s.project, &input("office", Preset::OfficeData))
                .map(|_| ());
        assert!(err.is_ok(), "names are case-sensitive: {err:?}");
        let err =
            s.db.create_requirement_profile(s.project, &input("Voice", Preset::OfficeData))
                .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
        assert!(err.to_string().contains("already exists"), "{err}");

        // Editing a preset's values turns it into Custom.
        let mut edited = input("Voice", Preset::VoiceVideo);
        edited.values.primary_min_dbm = -60;
        edited.values.min_snr_db = Some(25);
        let b2 = s.db.update_requirement_profile(b.id, &edited).unwrap();
        assert_eq!(b2.preset, Preset::Custom);
        assert_eq!(b2.values.min_snr_db, Some(25));
        assert!(!b2.is_default);

        assert!(s.db.delete_requirement_profile(a.id).unwrap());
        assert!(!s.db.delete_requirement_profile(a.id).unwrap());
        assert!(s
            .db
            .create_requirement_profile(999, &input("X", Preset::OfficeData))
            .is_err());
    }

    #[test]
    fn targets_and_floor_override_drive_evaluation() {
        let s = setup();
        let office =
            s.db.create_requirement_profile(s.project, &input("Office", Preset::OfficeData))
                .unwrap();
        let mut warehouse = input("Warehouse", Preset::WarehouseBasic);
        warehouse.is_default = false;
        let warehouse =
            s.db.create_requirement_profile(s.project, &warehouse)
                .unwrap();

        // No targets yet: every point not evaluated.
        measure(&s, vec![sample("00:11:11:11:11:01", b"Corp", -70.0)]);
        let fr = s.db.floor_requirements(s.floor).unwrap();
        assert_eq!(fr.source, Some(ProfileSource::ProjectDefault));
        assert_eq!(fr.profile.as_ref().unwrap().id, office.id);
        assert_eq!(fr.points[0].outcome, Outcome::NotEvaluated);

        // Target the SSID: -70 fails Office's -67.
        let corp = RequirementTarget::Ssid {
            ssid_raw: b"Corp".to_vec(),
        };
        let p =
            s.db.set_requirement_targets(office.id, &[corp.clone(), corp.clone()])
                .unwrap();
        assert_eq!(p.targets.len(), 1);
        assert_eq!(p.targets[0].label, "Corp");
        let fr = s.db.floor_requirements(s.floor).unwrap();
        assert_eq!(fr.points[0].outcome, Outcome::Fail);
        let summary = fr.summary.unwrap();
        assert_eq!((summary.passed, summary.failed), (0, 1));
        // The adapter can't receive 6 GHz, and 5 GHz was required: heard.
        assert!(fr.points[0]
            .rules
            .iter()
            .any(|r| r.rule == Rule::RequiredBand && r.band == Some(Band::Band5GHz)));

        // Floor override: Warehouse (-72) with the same SSID passes.
        s.db.set_requirement_targets(warehouse.id, &[corp]).unwrap();
        s.db.set_floor_requirement_profile(s.floor, Some(warehouse.id))
            .unwrap();
        let fr = s.db.floor_requirements(s.floor).unwrap();
        assert_eq!(fr.source, Some(ProfileSource::FloorOverride));
        assert_eq!(fr.override_profile_id, Some(warehouse.id));
        assert_eq!(fr.points[0].outcome, Outcome::Pass);
        let snap = s.db.requirements_snapshot(s.floor).unwrap().unwrap();
        assert_eq!(snap.profile_name, "Warehouse");
        assert_eq!(snap.summary.passed, 1);

        // Deleting the override's profile falls back to the default.
        s.db.delete_requirement_profile(warehouse.id).unwrap();
        let fr = s.db.floor_requirements(s.floor).unwrap();
        assert_eq!(
            (fr.override_profile_id, fr.source),
            (None, Some(ProfileSource::ProjectDefault))
        );

        // No default and no override: nothing applies.
        let mut off = input("Office", Preset::OfficeData);
        off.is_default = false;
        s.db.update_requirement_profile(office.id, &off).unwrap();
        let fr = s.db.floor_requirements(s.floor).unwrap();
        assert!(fr.profile.is_none() && fr.points.is_empty());
        assert!(s.db.requirements_snapshot(s.floor).unwrap().is_none());
    }

    #[test]
    fn ap_targets_hidden_ssids_and_project_boundaries() {
        let s = setup();
        let profile =
            s.db.create_requirement_profile(s.project, &input("Basic", Preset::WarehouseBasic))
                .unwrap();
        let ap =
            s.db.create_placed_ap(
                s.floor,
                &PlacedApInput {
                    name: "Lobby".into(),
                    x: 1.0,
                    y: 1.0,
                    model: None,
                    notes: None,
                    bssids: vec!["00:11:11:11:11:01".into()],
                },
            )
            .unwrap();
        measure(&s, vec![sample("00:11:11:11:11:01", b"", -60.0)]);

        let p =
            s.db.set_requirement_targets(profile.id, &[RequirementTarget::Ap { ap_id: ap.id }])
                .unwrap();
        assert_eq!(p.targets[0].label, "Lobby");
        assert_eq!(p.targets[0].bssids, ["00:11:11:11:11:01"]);
        let fr = s.db.floor_requirements(s.floor).unwrap();
        assert_eq!(fr.points[0].outcome, Outcome::Pass);

        // Hidden (empty) SSIDs can't be SSID targets.
        let err = s
            .db
            .set_requirement_targets(profile.id, &[RequirementTarget::Ssid { ssid_raw: vec![] }])
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // An unknown AP, or another project's profile or AP, is refused.
        assert!(s
            .db
            .set_requirement_targets(profile.id, &[RequirementTarget::Ap { ap_id: 999 }])
            .is_err());
        let p2 =
            s.db.create_project(&NewProject {
                name: "Q".into(),
                customer_name: None,
            })
            .unwrap();
        let q_profile =
            s.db.create_requirement_profile(p2.id, &input("Q", Preset::OfficeData))
                .unwrap();
        assert!(s
            .db
            .set_floor_requirement_profile(s.floor, Some(q_profile.id))
            .is_err());
        assert!(s
            .db
            .set_requirement_targets(q_profile.id, &[RequirementTarget::Ap { ap_id: ap.id }])
            .is_err());

        // Deleting the AP removes it as a target.
        s.db.delete_placed_ap(ap.id).unwrap();
        assert!(s.db.list_requirement_profiles(s.project).unwrap()[0]
            .targets
            .is_empty());
    }

    #[test]
    fn target_options_list_project_ssids_and_aps() {
        let s = setup();
        measure(
            &s,
            vec![
                sample("00:11:11:11:11:01", b"Corp", -60.0),
                sample("00:22:22:22:22:01", b"", -60.0),
            ],
        );
        measure(&s, vec![sample("00:11:11:11:11:01", b"Corp", -60.0)]);
        s.db.create_placed_ap(
            s.floor,
            &PlacedApInput {
                name: "Lobby".into(),
                x: 1.0,
                y: 1.0,
                model: None,
                notes: None,
                bssids: vec!["00:11:11:11:11:01".into()],
            },
        )
        .unwrap();
        let opts = s.db.requirement_target_options(s.project).unwrap();
        assert_eq!(opts.ssids.len(), 1, "hidden SSIDs aren't offered");
        assert_eq!(
            (opts.ssids[0].label.as_str(), opts.ssids[0].points),
            ("Corp", 2)
        );
        assert_eq!(opts.aps.len(), 1);
        assert_eq!(
            (opts.aps[0].name.as_str(), opts.aps[0].bssid_count),
            ("Lobby", 1)
        );
    }

    #[test]
    fn points_from_before_v4_have_unknown_adapter_bands() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::database::migrations::migrations()
            .to_version(&mut conn, 3)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO projects VALUES (1, 'P', NULL, 'x', 'x');
             INSERT INTO buildings VALUES (1, 1, 'B', 'x', 'x');
             INSERT INTO floors (id, building_id, name, plan_file, plan_mime, plan_width,
                 plan_height, created_at, updated_at)
                 VALUES (1, 1, 'F', 'p.png', 'image/png', 10, 10, 'x', 'x');
             INSERT INTO survey_points (id, floor_id, x, y, measured_at, scan_duration_ms,
                 provider, adapter_id)
                 VALUES (1, 1, 1, 1, '2026-01-01T00:00:00Z', 3000, 'networkmanager', 'linux:wlan0');",
        )
        .unwrap();
        crate::database::migrations::migrations()
            .to_latest(&mut conn)
            .unwrap();
        let floor: Option<i64> = conn
            .query_row(
                "SELECT requirement_profile_id FROM floors WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(floor, None);
        let db = Database {
            conn: std::sync::Mutex::new(conn),
        };
        let points = db.list_survey_points(1).unwrap();
        assert_eq!(points[0].adapter_bands, None);
        // Malformed JSON never gets in.
        assert!(db
            .conn()
            .execute("UPDATE survey_points SET adapter_bands = 'nope'", [])
            .is_err());
    }

    #[test]
    fn bands_text_round_trips() {
        let bands = [Band::Band2_4GHz, Band::Band6GHz];
        assert_eq!(bands_to_text(&bands), "2.4ghz,6ghz");
        assert_eq!(bands_from_text("2.4ghz,6ghz"), bands);
        assert!(bands_from_text("").is_empty());
    }
}
