//! Buildings, floors, survey points and samples.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::survey::models::*;
use crate::wifi::models::{AdapterId, Band, SecurityKind, Signal};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn non_empty<'a>(s: &'a str, what: &str) -> Result<&'a str> {
    let s = s.trim();
    if s.is_empty() {
        Err(WifiError::InvalidInput(format!("{what} must not be empty")))
    } else {
        Ok(s)
    }
}

/// Serde name of a unit enum (`Band::Band5GHz` → "5ghz"), for TEXT columns.
fn enum_text<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "unknown".into(),
    }
}

fn enum_parse<T: DeserializeOwned>(s: &str, fallback: T) -> T {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).unwrap_or(fallback)
}

fn exists(conn: &Connection, table: &str, id: i64) -> Result<bool> {
    Ok(conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
        [id],
        |r| r.get(0),
    )?)
}

/// Bump `updated_at` on a floor and everything above it.
fn touch_floor(conn: &Connection, floor_id: i64, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE floors SET updated_at = ?2 WHERE id = ?1",
        params![floor_id, now],
    )?;
    let building: Option<i64> = conn
        .query_row(
            "SELECT building_id FROM floors WHERE id = ?1",
            [floor_id],
            |r| r.get(0),
        )
        .optional()?;
    match building {
        Some(b) => touch_building(conn, b, now),
        None => Ok(()),
    }
}

fn touch_building(conn: &Connection, building_id: i64, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE buildings SET updated_at = ?2 WHERE id = ?1",
        params![building_id, now],
    )?;
    conn.execute(
        "UPDATE projects SET updated_at = ?2
         WHERE id = (SELECT project_id FROM buildings WHERE id = ?1)",
        params![building_id, now],
    )?;
    Ok(())
}

fn finite(v: f64, what: &str) -> Result<f64> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(WifiError::InvalidInput(format!("{what} is not a number")))
    }
}

fn check_on_plan(plan: &FloorPlan, x: f64, y: f64) -> Result<()> {
    finite(x, "x")?;
    finite(y, "y")?;
    if x < 0.0 || y < 0.0 || x > plan.width || y > plan.height {
        return Err(WifiError::InvalidInput(format!(
            "position ({x:.0}, {y:.0}) is outside the floor plan ({:.0} × {:.0})",
            plan.width, plan.height
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------

const BUILDING_COLUMNS: &str = "id, project_id, name, created_at, updated_at";

fn building_from_row(r: &Row<'_>) -> rusqlite::Result<Building> {
    Ok(Building {
        id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        created_at: parse_ts(r, 3)?,
        updated_at: parse_ts(r, 4)?,
    })
}

const FLOOR_COLUMNS: &str = "f.id, f.building_id, f.name, f.level,
    f.plan_file, f.plan_mime, f.plan_width, f.plan_height,
    f.scale_x1, f.scale_y1, f.scale_x2, f.scale_y2, f.scale_length_m,
    f.created_at, f.updated_at,
    (SELECT COUNT(*) FROM survey_points p WHERE p.floor_id = f.id)";

fn floor_from_row(r: &Row<'_>) -> rusqlite::Result<Floor> {
    let plan = match (
        r.get::<_, Option<String>>(4)?,
        r.get::<_, Option<String>>(5)?,
        r.get::<_, Option<f64>>(6)?,
        r.get::<_, Option<f64>>(7)?,
    ) {
        (Some(file), Some(mime), Some(width), Some(height)) => Some(FloorPlan {
            file,
            mime,
            width,
            height,
        }),
        _ => None,
    };
    let scale = match (
        r.get::<_, Option<f64>>(8)?,
        r.get::<_, Option<f64>>(9)?,
        r.get::<_, Option<f64>>(10)?,
        r.get::<_, Option<f64>>(11)?,
        r.get::<_, Option<f64>>(12)?,
    ) {
        (Some(x1), Some(y1), Some(x2), Some(y2), Some(length_m)) => Some(FloorScale {
            x1,
            y1,
            x2,
            y2,
            length_m,
        }),
        _ => None,
    };
    Ok(Floor {
        id: r.get(0)?,
        building_id: r.get(1)?,
        name: r.get(2)?,
        level: r.get(3)?,
        plan,
        scale,
        created_at: parse_ts(r, 13)?,
        updated_at: parse_ts(r, 14)?,
        point_count: r.get(15)?,
    })
}

const POINT_COLUMNS: &str = "id, floor_id, x, y, measured_at, scan_duration_ms,
    provider, adapter_id, adapter_model, adapter_driver, adapter_hw_id";

fn point_from_row(r: &Row<'_>) -> rusqlite::Result<SurveyPoint> {
    Ok(SurveyPoint {
        id: r.get(0)?,
        floor_id: r.get(1)?,
        x: r.get(2)?,
        y: r.get(3)?,
        measured_at: parse_ts(r, 4)?,
        scan_duration_ms: r.get(5)?,
        adapter: MeasuringAdapter {
            provider: r.get(6)?,
            id: AdapterId(r.get(7)?),
            model: r.get(8)?,
            driver: r.get(9)?,
            hw_id: r.get(10)?,
        },
        samples: Vec::new(),
    })
}

const SAMPLE_COLUMNS: &str = "s.point_id, s.bssid, s.ssid, s.ssid_raw, s.frequency_mhz,
    s.channel, s.band, s.channel_width_mhz, s.channel_center_mhz, s.signal_dbm,
    s.signal_percent, s.security, s.phy_type, s.wifi_generation, s.noise_dbm,
    s.snr_db, s.channel_utilization_pct, s.station_count, s.last_seen_age_ms,
    s.is_connected";

/// Strongest first: dBm readings by dBm, then %-only readings by %.
const SAMPLE_ORDER: &str =
    "s.point_id, s.signal_dbm IS NULL, s.signal_dbm DESC, s.signal_percent DESC, s.bssid";

fn sample_from_row(r: &Row<'_>) -> rusqlite::Result<(i64, Sample)> {
    let band: String = r.get(6)?;
    let security: String = r.get(11)?;
    Ok((
        r.get(0)?,
        Sample {
            bssid: r.get(1)?,
            ssid: r.get(2)?,
            ssid_raw: r.get(3)?,
            frequency_mhz: r.get(4)?,
            channel: r.get(5)?,
            band: enum_parse(&band, Band::Unknown),
            channel_width_mhz: r.get(7)?,
            channel_center_mhz: r.get(8)?,
            signal: Signal {
                dbm: r.get(9)?,
                quality_percent: r.get(10)?,
            },
            security: enum_parse(&security, SecurityKind::Unknown),
            phy_type: r.get(12)?,
            wifi_generation: r.get(13)?,
            noise_dbm: r.get(14)?,
            snr_db: r.get(15)?,
            channel_utilization_pct: r.get(16)?,
            station_count: r.get(17)?,
            last_seen_age_ms: r.get::<_, Option<i64>>(18)?.map(|v| v.max(0) as u64),
            is_connected: r.get(19)?,
        },
    ))
}

fn get_floor(conn: &Connection, id: i64) -> Result<Option<Floor>> {
    Ok(conn
        .query_row(
            &format!("SELECT {FLOOR_COLUMNS} FROM floors f WHERE f.id = ?1"),
            [id],
            floor_from_row,
        )
        .optional()?)
}

fn require_floor(conn: &Connection, id: i64) -> Result<Floor> {
    get_floor(conn, id)?
        .ok_or_else(|| WifiError::InvalidInput(format!("floor {id} no longer exists")))
}

/// Points matching `filter` (an SQL condition on `p`), samples attached.
fn query_points(conn: &Connection, filter: &str, id: i64) -> Result<Vec<SurveyPoint>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POINT_COLUMNS} FROM survey_points p WHERE {filter} ORDER BY measured_at, id"
    ))?;
    let mut points: Vec<SurveyPoint> = stmt
        .query_map([id], point_from_row)?
        .collect::<rusqlite::Result<_>>()?;

    let mut stmt = conn.prepare(&format!(
        "SELECT {SAMPLE_COLUMNS} FROM survey_samples s
         JOIN survey_points p ON p.id = s.point_id
         WHERE {filter} ORDER BY {SAMPLE_ORDER}"
    ))?;
    let mut by_point: HashMap<i64, Vec<Sample>> = HashMap::new();
    for row in stmt.query_map([id], sample_from_row)? {
        let (point_id, sample) = row?;
        by_point.entry(point_id).or_default().push(sample);
    }
    for p in &mut points {
        p.samples = by_point.remove(&p.id).unwrap_or_default();
    }
    Ok(points)
}

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

impl Database {
    // --- Buildings --------------------------------------------------------

    pub fn create_building(&self, new: &NewBuilding) -> Result<Building> {
        let name = non_empty(&new.name, "building name")?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !exists(&tx, "projects", new.project_id)? {
            return Err(WifiError::InvalidInput(format!(
                "project {} no longer exists",
                new.project_id
            )));
        }
        tx.execute(
            "INSERT INTO buildings (project_id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![new.project_id, name, now],
        )?;
        let id = tx.last_insert_rowid();
        touch_building(&tx, id, &now)?;
        let b = tx.query_row(
            &format!("SELECT {BUILDING_COLUMNS} FROM buildings WHERE id = ?1"),
            [id],
            building_from_row,
        )?;
        tx.commit()?;
        Ok(b)
    }

    pub fn list_buildings(&self, project_id: i64) -> Result<Vec<Building>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {BUILDING_COLUMNS} FROM buildings WHERE project_id = ?1 ORDER BY name COLLATE NOCASE, id"
        ))?;
        let rows = stmt.query_map([project_id], building_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_building(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM buildings WHERE id = ?1", [id])?
            > 0)
    }

    // --- Floors -----------------------------------------------------------

    pub fn create_floor(&self, new: &NewFloor) -> Result<Floor> {
        let name = non_empty(&new.name, "floor name")?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !exists(&tx, "buildings", new.building_id)? {
            return Err(WifiError::InvalidInput(format!(
                "building {} no longer exists",
                new.building_id
            )));
        }
        tx.execute(
            "INSERT INTO floors (building_id, name, level, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![new.building_id, name, new.level, now],
        )?;
        let id = tx.last_insert_rowid();
        touch_floor(&tx, id, &now)?;
        let f = require_floor(&tx, id)?;
        tx.commit()?;
        Ok(f)
    }

    pub fn list_floors(&self, building_id: i64) -> Result<Vec<Floor>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FLOOR_COLUMNS} FROM floors f WHERE f.building_id = ?1 ORDER BY f.level, f.name COLLATE NOCASE, f.id"
        ))?;
        let rows = stmt.query_map([building_id], floor_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_floor(&self, id: i64) -> Result<Option<Floor>> {
        get_floor(&self.conn(), id)
    }

    pub fn delete_floor(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM floors WHERE id = ?1", [id])?
            > 0)
    }

    /// Attach a (new) plan image. Returns the updated floor and the previous
    /// plan's file name, which the caller should delete from disk.
    ///
    /// Refused while the floor has survey points: their coordinates belong
    /// to the old image. Any scale line is cleared for the same reason.
    pub fn set_floor_plan(
        &self,
        floor_id: i64,
        plan: &FloorPlan,
    ) -> Result<(Floor, Option<String>)> {
        if !(plan.width >= 1.0
            && plan.height >= 1.0
            && plan.width.is_finite()
            && plan.height.is_finite())
        {
            return Err(WifiError::InvalidInput(
                "floor plan has no usable size".into(),
            ));
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor = require_floor(&tx, floor_id)?;
        let ap_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM placed_aps WHERE floor_id = ?1",
            [floor_id],
            |r| r.get(0),
        )?;
        if floor.point_count > 0 || ap_count > 0 {
            return Err(WifiError::InvalidInput(format!(
                "'{}' already has {} survey point(s) and {} access point(s) placed on the \
                 current plan. Delete them first, or create a new floor for the new plan.",
                floor.name, floor.point_count, ap_count
            )));
        }
        tx.execute(
            "UPDATE floors SET plan_file = ?2, plan_mime = ?3, plan_width = ?4, plan_height = ?5,
                 scale_x1 = NULL, scale_y1 = NULL, scale_x2 = NULL, scale_y2 = NULL, scale_length_m = NULL
             WHERE id = ?1",
            params![floor_id, plan.file, plan.mime, plan.width, plan.height],
        )?;
        touch_floor(&tx, floor_id, &now)?;
        let updated = require_floor(&tx, floor_id)?;
        tx.commit()?;
        Ok((updated, floor.plan.map(|p| p.file)))
    }

    /// Set (or clear, with `None`) the floor's scale reference line.
    pub fn set_floor_scale(&self, floor_id: i64, scale: Option<&FloorScale>) -> Result<Floor> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor = require_floor(&tx, floor_id)?;
        if let Some(s) = scale {
            let plan = floor.plan.as_ref().ok_or_else(|| {
                WifiError::InvalidInput("import a floor plan before setting its scale".into())
            })?;
            check_on_plan(plan, s.x1, s.y1)?;
            check_on_plan(plan, s.x2, s.y2)?;
            if !(finite(s.length_m, "length")? > 0.0 && s.length_m <= 100_000.0) {
                return Err(WifiError::InvalidInput(
                    "the reference length must be between 0 and 100 000 m".into(),
                ));
            }
            if s.length_px() < 5.0 {
                return Err(WifiError::InvalidInput(
                    "the reference line is too short; pick two points further apart".into(),
                ));
            }
        }
        tx.execute(
            "UPDATE floors SET scale_x1 = ?2, scale_y1 = ?3, scale_x2 = ?4, scale_y2 = ?5, scale_length_m = ?6
             WHERE id = ?1",
            params![
                floor_id,
                scale.map(|s| s.x1),
                scale.map(|s| s.y1),
                scale.map(|s| s.x2),
                scale.map(|s| s.y2),
                scale.map(|s| s.length_m),
            ],
        )?;
        touch_floor(&tx, floor_id, &now)?;
        let updated = require_floor(&tx, floor_id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Plan files still referenced by some floor (everything else in the
    /// plan directory is garbage).
    pub fn referenced_plan_files(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT plan_file FROM floors WHERE plan_file IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- Survey points ----------------------------------------------------

    /// Store a point and its samples. At most one sample per BSSID: which
    /// reading to keep is the caller's decision (Measure Here de-duplicates),
    /// so a duplicate fails the whole insert rather than replacing a row.
    pub fn insert_survey_point(&self, new: &NewSurveyPoint) -> Result<SurveyPoint> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor = require_floor(&tx, new.floor_id)?;
        let plan = floor.plan.as_ref().ok_or_else(|| {
            WifiError::InvalidInput("import a floor plan before measuring".into())
        })?;
        check_on_plan(plan, new.x, new.y)?;

        tx.execute(
            "INSERT INTO survey_points (floor_id, x, y, measured_at, scan_duration_ms, provider,
                 adapter_id, adapter_model, adapter_driver, adapter_hw_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                new.floor_id,
                new.x,
                new.y,
                new.measured_at.to_rfc3339(),
                new.scan_duration_ms,
                new.adapter.provider,
                new.adapter.id.as_str(),
                new.adapter.model,
                new.adapter.driver,
                new.adapter.hw_id,
            ],
        )?;
        let point_id = tx.last_insert_rowid();
        {
            let mut stmt = tx.prepare(&format!(
                "INSERT INTO survey_samples ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                SAMPLE_COLUMNS.replace("s.", "")
            ))?;
            for s in &new.samples {
                stmt.execute(params![
                    point_id,
                    s.bssid,
                    s.ssid,
                    s.ssid_raw,
                    s.frequency_mhz,
                    s.channel,
                    enum_text(&s.band),
                    s.channel_width_mhz,
                    s.channel_center_mhz,
                    s.signal.dbm,
                    s.signal.quality_percent,
                    enum_text(&s.security),
                    s.phy_type,
                    s.wifi_generation,
                    s.noise_dbm,
                    s.snr_db,
                    s.channel_utilization_pct,
                    s.station_count,
                    s.last_seen_age_ms.map(|v| v.min(i64::MAX as u64) as i64),
                    s.is_connected,
                ])?;
            }
        }
        touch_floor(&tx, new.floor_id, &new.measured_at.to_rfc3339())?;
        let point = query_points(&tx, "p.id = ?1", point_id)?
            .pop()
            .ok_or_else(|| WifiError::Database("inserted survey point vanished".into()))?;
        tx.commit()?;
        Ok(point)
    }

    /// All points on a floor, oldest first, samples strongest first.
    pub fn list_survey_points(&self, floor_id: i64) -> Result<Vec<SurveyPoint>> {
        query_points(&self.conn(), "p.floor_id = ?1", floor_id)
    }

    pub fn delete_survey_point(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor: Option<i64> = tx
            .query_row(
                "SELECT floor_id FROM survey_points WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(floor) = floor else {
            return Ok(false);
        };
        tx.execute("DELETE FROM survey_points WHERE id = ?1", [id])?;
        touch_floor(&tx, floor, &now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Distinct adapters that have measured on this floor.
    pub fn floor_adapters(&self, floor_id: i64) -> Result<Vec<MeasuringAdapter>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT provider, adapter_id, adapter_model, adapter_driver, adapter_hw_id
             FROM survey_points WHERE floor_id = ?1
             GROUP BY adapter_id, adapter_hw_id ORDER BY MIN(measured_at)",
        )?;
        let rows = stmt.query_map([floor_id], |r| {
            Ok(MeasuringAdapter {
                provider: r.get(0)?,
                id: AdapterId(r.get(1)?),
                model: r.get(2)?,
                driver: r.get(3)?,
                hw_id: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::projects::NewProject;

    fn floor_with_plan(db: &Database) -> Floor {
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
        .unwrap()
        .0
    }

    fn sample(bssid: &str, dbm: Option<f32>, pct: Option<u8>) -> Sample {
        Sample {
            bssid: bssid.into(),
            ssid: Some("Net".into()),
            ssid_raw: b"Net".to_vec(),
            frequency_mhz: 5180,
            channel: Some(36),
            band: Band::Band5GHz,
            channel_width_mhz: Some(80),
            channel_center_mhz: Some(5210),
            signal: Signal {
                dbm,
                quality_percent: pct,
            },
            security: SecurityKind::Wpa2Wpa3Personal,
            phy_type: Some("802.11ax".into()),
            wifi_generation: Some(6),
            noise_dbm: None,
            snr_db: None,
            channel_utilization_pct: Some(12.5),
            station_count: Some(3),
            last_seen_age_ms: Some(800),
            is_connected: false,
        }
    }

    fn adapter() -> MeasuringAdapter {
        MeasuringAdapter {
            id: AdapterId::linux("wlan0"),
            provider: "networkmanager".into(),
            model: Some("Wireless-AC 9560".into()),
            driver: Some("iwlwifi".into()),
            hw_id: Some("pci:8086:a370".into()),
        }
    }

    #[test]
    fn hierarchy_and_points_round_trip() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.schema_version().unwrap() >= 2);
        let floor = floor_with_plan(&db);
        assert_eq!(floor.plan.as_ref().unwrap().width, 1000.0);

        let point = db
            .insert_survey_point(&NewSurveyPoint {
                floor_id: floor.id,
                x: 100.0,
                y: 200.0,
                measured_at: Utc::now(),
                scan_duration_ms: 3100,
                adapter: adapter(),
                samples: vec![
                    sample("AA:00:00:00:00:02", None, Some(40)),
                    sample("AA:00:00:00:00:01", Some(-61.0), Some(70)),
                    sample("AA:00:00:00:00:03", Some(-48.0), Some(90)),
                ],
            })
            .unwrap();
        // Strongest first, %-only last.
        let order: Vec<_> = point.samples.iter().map(|s| s.bssid.as_str()).collect();
        assert_eq!(
            order,
            [
                "AA:00:00:00:00:03",
                "AA:00:00:00:00:01",
                "AA:00:00:00:00:02"
            ]
        );
        assert_eq!(point.samples[0].band, Band::Band5GHz);
        assert_eq!(point.samples[0].security, SecurityKind::Wpa2Wpa3Personal);
        assert_eq!(point.adapter, adapter());

        let listed = db.list_survey_points(floor.id).unwrap();
        assert_eq!(listed, vec![point.clone()]);
        assert_eq!(db.get_floor(floor.id).unwrap().unwrap().point_count, 1);
        assert_eq!(db.floor_adapters(floor.id).unwrap(), vec![adapter()]);

        // Plan can't be swapped under existing points.
        let err = db
            .set_floor_plan(
                floor.id,
                &FloorPlan {
                    file: "plan-2.png".into(),
                    mime: "image/png".into(),
                    width: 10.0,
                    height: 10.0,
                },
            )
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        assert!(db.delete_survey_point(point.id).unwrap());
        assert!(db.list_survey_points(floor.id).unwrap().is_empty());
        assert!(!db.delete_survey_point(point.id).unwrap());
    }

    #[test]
    fn duplicate_bssid_fails_instead_of_overwriting() {
        let db = Database::open_in_memory().unwrap();
        let floor = floor_with_plan(&db);
        let mut other_channel = sample("AA:00:00:00:00:01", Some(-70.0), None);
        other_channel.frequency_mhz = 5500;
        let result = db.insert_survey_point(&NewSurveyPoint {
            floor_id: floor.id,
            x: 10.0,
            y: 10.0,
            measured_at: Utc::now(),
            scan_duration_ms: 3000,
            adapter: adapter(),
            samples: vec![
                sample("AA:00:00:00:00:01", Some(-50.0), None),
                other_channel,
            ],
        });
        assert!(result.is_err());
        // Nothing half-stored.
        assert!(db.list_survey_points(floor.id).unwrap().is_empty());
        assert_eq!(db.get_floor(floor.id).unwrap().unwrap().point_count, 0);
    }

    #[test]
    fn rejects_points_off_the_plan() {
        let db = Database::open_in_memory().unwrap();
        let floor = floor_with_plan(&db);
        let err = db
            .insert_survey_point(&NewSurveyPoint {
                floor_id: floor.id,
                x: 1001.0,
                y: 10.0,
                measured_at: Utc::now(),
                scan_duration_ms: 0,
                adapter: adapter(),
                samples: vec![],
            })
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    #[test]
    fn scale_validation() {
        let db = Database::open_in_memory().unwrap();
        let floor = floor_with_plan(&db);
        let s = FloorScale {
            x1: 0.0,
            y1: 0.0,
            x2: 300.0,
            y2: 400.0,
            length_m: 10.0,
        };
        let f = db.set_floor_scale(floor.id, Some(&s)).unwrap();
        assert_eq!(f.scale.unwrap().px_per_metre(), 50.0);
        let bad = FloorScale { length_m: 0.0, ..s };
        assert!(db.set_floor_scale(floor.id, Some(&bad)).is_err());
        let short = FloorScale {
            x2: 1.0,
            y2: 1.0,
            ..s
        };
        assert!(db.set_floor_scale(floor.id, Some(&short)).is_err());
        assert!(db.set_floor_scale(floor.id, None).unwrap().scale.is_none());
    }

    #[test]
    fn cascade_delete_frees_plan_reference() {
        let db = Database::open_in_memory().unwrap();
        let floor = floor_with_plan(&db);
        assert!(db.referenced_plan_files().unwrap().contains("plan-1.png"));
        let building = floor.building_id;
        let project = db.list_projects().unwrap()[0].id;
        assert_eq!(db.list_buildings(project).unwrap()[0].id, building);
        assert!(db.delete_project(project).unwrap());
        assert!(db.referenced_plan_files().unwrap().is_empty());
        assert!(db.list_floors(building).unwrap().is_empty());
    }
}
