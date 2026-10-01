//! Access points placed on floor plans.

use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::survey::models::{FloorPlan, PlacedAp, PlacedApInput};

const COLUMNS: &str =
    "a.id, a.floor_id, a.name, a.x, a.y, a.model, a.notes, a.created_at, a.updated_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<PlacedAp> {
    Ok(PlacedAp {
        id: r.get(0)?,
        floor_id: r.get(1)?,
        name: r.get(2)?,
        x: r.get(3)?,
        y: r.get(4)?,
        model: r.get(5)?,
        notes: r.get(6)?,
        bssids: Vec::new(),
        created_at: parse_ts(r, 7)?,
        updated_at: parse_ts(r, 8)?,
    })
}

/// `aa-bb-cc-dd-ee-ff` / `aabb.ccdd.eeff` / mixed case → `AA:BB:CC:DD:EE:FF`.
pub fn normalize_bssid(s: &str) -> Option<String> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let separators_ok = s
        .chars()
        .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '-' | '.'));
    if hex.len() != 12 || !separators_ok {
        return None;
    }
    let up = hex.to_ascii_uppercase();
    Some(
        (0..6)
            .map(|i| &up[i * 2..i * 2 + 2])
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn optional_text(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// building_id, plan_file, plan_mime, plan_width, plan_height
type FloorRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<f64>,
    Option<f64>,
);

/// Floor's plan plus its building; errors if the floor is gone or has no plan.
fn floor_context(conn: &Connection, floor_id: i64) -> Result<(i64, FloorPlan)> {
    let row: Option<FloorRow> = conn
        .query_row(
            "SELECT building_id, plan_file, plan_mime, plan_width, plan_height FROM floors WHERE id = ?1",
            [floor_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    match row {
        None => Err(WifiError::InvalidInput(format!(
            "floor {floor_id} no longer exists"
        ))),
        Some((building, Some(file), Some(mime), Some(width), Some(height))) => Ok((
            building,
            FloorPlan {
                file,
                mime,
                width,
                height,
            },
        )),
        Some(_) => Err(WifiError::InvalidInput(
            "import a floor plan before placing access points".into(),
        )),
    }
}

/// Validate input; returns (name, normalised sorted BSSIDs).
fn validate(input: &PlacedApInput, plan: &FloorPlan) -> Result<(String, Vec<String>)> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(WifiError::InvalidInput(
            "the access point needs a name".into(),
        ));
    }
    if !(input.x.is_finite() && input.y.is_finite())
        || input.x < 0.0
        || input.y < 0.0
        || input.x > plan.width
        || input.y > plan.height
    {
        return Err(WifiError::InvalidInput(
            "the access point must be placed on the floor plan".into(),
        ));
    }
    let mut bssids = BTreeSet::new();
    for b in &input.bssids {
        let n = normalize_bssid(b).ok_or_else(|| {
            WifiError::InvalidInput(format!("'{b}' is not a BSSID (MAC address)"))
        })?;
        bssids.insert(n);
    }
    Ok((name.to_string(), bssids.into_iter().collect()))
}

/// A BSSID can belong to only one AP per building.
fn check_unclaimed(
    conn: &Connection,
    building_id: i64,
    bssids: &[String],
    except_ap: Option<i64>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT a.name FROM placed_ap_bssids b
         JOIN placed_aps a ON a.id = b.ap_id
         JOIN floors f ON f.id = a.floor_id
         WHERE f.building_id = ?1 AND b.bssid = ?2 AND a.id IS NOT ?3",
    )?;
    for bssid in bssids {
        if let Some(owner) = stmt
            .query_row(params![building_id, bssid, except_ap], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            return Err(WifiError::InvalidInput(format!(
                "{bssid} already belongs to '{owner}' in this building"
            )));
        }
    }
    Ok(())
}

fn write_bssids(conn: &Connection, ap_id: i64, bssids: &[String]) -> Result<()> {
    conn.execute("DELETE FROM placed_ap_bssids WHERE ap_id = ?1", [ap_id])?;
    let mut stmt = conn.prepare("INSERT INTO placed_ap_bssids (ap_id, bssid) VALUES (?1, ?2)")?;
    for b in bssids {
        stmt.execute(params![ap_id, b])?;
    }
    Ok(())
}

fn query(conn: &Connection, filter: &str, id: i64) -> Result<Vec<PlacedAp>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM placed_aps a JOIN floors f ON f.id = a.floor_id WHERE {filter} ORDER BY a.id"
    ))?;
    let mut aps: Vec<PlacedAp> = stmt
        .query_map([id], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT b.ap_id, b.bssid FROM placed_ap_bssids b
         JOIN placed_aps a ON a.id = b.ap_id JOIN floors f ON f.id = a.floor_id
         WHERE {filter} ORDER BY b.bssid"
    ))?;
    let mut by_ap: HashMap<i64, Vec<String>> = HashMap::new();
    for row in stmt.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (ap, bssid) = row?;
        by_ap.entry(ap).or_default().push(bssid);
    }
    for ap in &mut aps {
        ap.bssids = by_ap.remove(&ap.id).unwrap_or_default();
    }
    Ok(aps)
}

fn touch(conn: &Connection, floor_id: i64, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE floors SET updated_at = ?2 WHERE id = ?1",
        params![floor_id, now],
    )?;
    conn.execute(
        "UPDATE buildings SET updated_at = ?2 WHERE id = (SELECT building_id FROM floors WHERE id = ?1)",
        params![floor_id, now],
    )?;
    conn.execute(
        "UPDATE projects SET updated_at = ?2 WHERE id =
            (SELECT b.project_id FROM buildings b JOIN floors f ON f.building_id = b.id WHERE f.id = ?1)",
        params![floor_id, now],
    )?;
    Ok(())
}

impl Database {
    /// All APs placed anywhere in the building, oldest first (a stable order:
    /// the UI derives each AP's colour from it).
    pub fn list_building_aps(&self, building_id: i64) -> Result<Vec<PlacedAp>> {
        query(&self.conn(), "f.building_id = ?1", building_id)
    }

    pub fn create_placed_ap(&self, floor_id: i64, input: &PlacedApInput) -> Result<PlacedAp> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (building, plan) = floor_context(&tx, floor_id)?;
        let (name, bssids) = validate(input, &plan)?;
        check_unclaimed(&tx, building, &bssids, None)?;
        tx.execute(
            "INSERT INTO placed_aps (floor_id, name, x, y, model, notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                floor_id,
                name,
                input.x,
                input.y,
                optional_text(&input.model),
                optional_text(&input.notes),
                now
            ],
        )?;
        let id = tx.last_insert_rowid();
        write_bssids(&tx, id, &bssids)?;
        touch(&tx, floor_id, &now)?;
        let ap = query(&tx, "a.id = ?1", id)?
            .pop()
            .ok_or_else(|| WifiError::Database("inserted access point vanished".into()))?;
        tx.commit()?;
        Ok(ap)
    }

    pub fn update_placed_ap(&self, id: i64, input: &PlacedApInput) -> Result<PlacedAp> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor_id: i64 = tx
            .query_row("SELECT floor_id FROM placed_aps WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| {
                WifiError::InvalidInput(format!("access point {id} no longer exists"))
            })?;
        let (building, plan) = floor_context(&tx, floor_id)?;
        let (name, bssids) = validate(input, &plan)?;
        check_unclaimed(&tx, building, &bssids, Some(id))?;
        tx.execute(
            "UPDATE placed_aps SET name = ?2, x = ?3, y = ?4, model = ?5, notes = ?6, updated_at = ?7 WHERE id = ?1",
            params![id, name, input.x, input.y, optional_text(&input.model), optional_text(&input.notes), now],
        )?;
        write_bssids(&tx, id, &bssids)?;
        touch(&tx, floor_id, &now)?;
        let ap = query(&tx, "a.id = ?1", id)?
            .pop()
            .ok_or_else(|| WifiError::Database("updated access point vanished".into()))?;
        tx.commit()?;
        Ok(ap)
    }

    pub fn delete_placed_ap(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM placed_aps WHERE id = ?1", [id])?
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::projects::NewProject;
    use crate::survey::models::{NewBuilding, NewFloor};

    fn setup() -> (Database, i64, i64) {
        let db = Database::open_in_memory().unwrap();
        let p = db
            .create_project(&NewProject {
                name: "P".into(),
                customer_name: None,
            })
            .unwrap();
        let b = db
            .create_building(&NewBuilding {
                project_id: p.id,
                name: "B".into(),
            })
            .unwrap();
        let mk = |name: &str| {
            let f = db
                .create_floor(&NewFloor {
                    building_id: b.id,
                    name: name.into(),
                    level: 0,
                })
                .unwrap();
            db.set_floor_plan(
                f.id,
                &FloorPlan {
                    file: format!("plan-{name}.png"),
                    mime: "image/png".into(),
                    width: 100.0,
                    height: 100.0,
                },
            )
            .unwrap();
            f.id
        };
        let (f1, f2) = (mk("1"), mk("2"));
        (db, f1, f2)
    }

    fn input(name: &str, bssids: &[&str]) -> PlacedApInput {
        PlacedApInput {
            name: name.into(),
            x: 10.0,
            y: 20.0,
            model: Some("  ".into()),
            notes: None,
            bssids: bssids.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn normalizes_bssids() {
        assert_eq!(
            normalize_bssid("78-20-51-37-70-62").as_deref(),
            Some("78:20:51:37:70:62")
        );
        assert_eq!(
            normalize_bssid("7820.5137.70ab").as_deref(),
            Some("78:20:51:37:70:AB")
        );
        assert_eq!(normalize_bssid("78:20:51:37:70"), None);
        assert_eq!(normalize_bssid("zz:20:51:37:70:62"), None);
    }

    #[test]
    fn crud_and_building_wide_uniqueness() {
        let (db, f1, f2) = setup();
        let ap = db
            .create_placed_ap(
                f1,
                &input(" Hallway ", &["86:20:51:37:70:62", "78-20-51-37-70-62"]),
            )
            .unwrap();
        assert_eq!(ap.name, "Hallway");
        assert_eq!(ap.model, None);
        assert_eq!(ap.bssids, ["78:20:51:37:70:62", "86:20:51:37:70:62"]);

        // Same BSSID on another floor of the same building is refused.
        let err = db
            .create_placed_ap(f2, &input("Other", &["78:20:51:37:70:62"]))
            .unwrap_err();
        assert!(err.to_string().contains("Hallway"), "{err}");

        // Editing may keep its own BSSIDs.
        let moved = db
            .update_placed_ap(
                ap.id,
                &PlacedApInput {
                    x: 50.0,
                    ..input("Hallway", &["78:20:51:37:70:62"])
                },
            )
            .unwrap();
        assert_eq!((moved.x, moved.bssids.len()), (50.0, 1));

        let b = db
            .create_placed_ap(f2, &input("Upstairs", &["86:20:51:37:70:62"]))
            .unwrap();
        let building = db.get_floor(f1).unwrap().unwrap().building_id;
        let all = db.list_building_aps(building).unwrap();
        assert_eq!(all.iter().map(|a| a.id).collect::<Vec<_>>(), [ap.id, b.id]);

        assert!(db
            .create_placed_ap(
                f1,
                &PlacedApInput {
                    x: 101.0,
                    ..input("Off", &[])
                }
            )
            .is_err());
        assert!(db.delete_placed_ap(ap.id).unwrap());
        db.delete_floor(f2).unwrap();
        assert!(db.list_building_aps(building).unwrap().is_empty());
    }
}
