//! Notes on floors, survey points and placed APs; note pins on the plan;
//! and everything a floor's report section needs from them.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::aps::touch;
use super::photos::PhotoTarget;
use super::{parse_ts, Database};
use crate::error::{Result, WifiError};

/// Longest note (floor, point, AP or pin text), in characters.
pub const MAX_NOTE_CHARS: usize = 4000;
pub const MAX_CATEGORY_CHARS: usize = 40;

/// Trim; blank → `None`; longer than `max` characters → an error.
pub(crate) fn clean_text(s: Option<&str>, max: usize, what: &str) -> Result<Option<String>> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let chars = s.chars().count();
    if chars > max {
        return Err(WifiError::InvalidInput(format!(
            "the {what} is {chars} characters long; the limit is {max}"
        )));
    }
    Ok(Some(s.to_string()))
}

pub(crate) fn clean_note(s: Option<&str>) -> Result<Option<String>> {
    clean_text(s, MAX_NOTE_CHARS, "note")
}

/// What a free-text note belongs to. Serialised as `{ "kind": "point", "id": 7 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum NoteTarget {
    Floor(i64),
    Point(i64),
    Ap(i64),
}

impl NoteTarget {
    /// (table, id, what for messages)
    fn row(self) -> (&'static str, i64, &'static str) {
        match self {
            Self::Floor(id) => ("floors", id, "floor"),
            Self::Point(id) => ("survey_points", id, "survey point"),
            Self::Ap(id) => ("placed_aps", id, "access point"),
        }
    }
}

/// A note placed anywhere on a plan ("metal cabinet", "ceiling 4 m").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotePin {
    pub id: i64,
    pub floor_id: i64,
    /// Plan pixels, like survey points.
    pub x: f64,
    pub y: f64,
    pub text: String,
    pub category: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotePinInput {
    pub x: f64,
    pub y: f64,
    pub text: String,
    #[serde(default)]
    pub category: Option<String>,
}

/// A note on one point or AP.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNote {
    pub id: i64,
    /// The AP's name; `None` for points (the report numbers them).
    pub name: Option<String>,
    pub notes: String,
}

/// A photo as the report uses it: the downscaled copy without metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportPhoto {
    pub id: i64,
    pub target: PhotoTarget,
    /// JPEG in the photo store; never the original.
    pub report_file: String,
    pub caption: Option<String>,
    pub taken_at: Option<String>,
}

/// Everything written or photographed on one floor, for its report section.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorAnnotations {
    pub floor_id: i64,
    pub notes: Option<String>,
    /// Points with a note, in measurement order.
    pub point_notes: Vec<ItemNote>,
    /// APs on this floor with a note.
    pub ap_notes: Vec<ItemNote>,
    pub pins: Vec<NotePin>,
    /// Photos marked "in report", oldest first.
    pub photos: Vec<ReportPhoto>,
}

const PIN_COLUMNS: &str = "id, floor_id, x, y, text, category, created_at, updated_at";

fn pin_from_row(r: &Row<'_>) -> rusqlite::Result<NotePin> {
    Ok(NotePin {
        id: r.get(0)?,
        floor_id: r.get(1)?,
        x: r.get(2)?,
        y: r.get(3)?,
        text: r.get(4)?,
        category: r.get(5)?,
        created_at: parse_ts(r, 6)?,
        updated_at: parse_ts(r, 7)?,
    })
}

/// The floor a row belongs to; an error if the row is gone.
fn floor_of(conn: &Connection, target: NoteTarget) -> Result<i64> {
    let (table, id, what) = target.row();
    let sql = match target {
        NoteTarget::Floor(_) => "SELECT id FROM floors WHERE id = ?1".to_string(),
        _ => format!("SELECT floor_id FROM {table} WHERE id = ?1"),
    };
    conn.query_row(&sql, [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| WifiError::InvalidInput(format!("{what} {id} no longer exists")))
}

/// Check a pin position against the floor's plan.
fn check_pin(conn: &Connection, floor_id: i64, input: &NotePinInput) -> Result<NotePinInput> {
    let size: Option<(Option<f64>, Option<f64>)> = conn
        .query_row(
            "SELECT plan_width, plan_height FROM floors WHERE id = ?1",
            [floor_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (width, height) = match size {
        None => {
            return Err(WifiError::InvalidInput(format!(
                "floor {floor_id} no longer exists"
            )))
        }
        Some((Some(w), Some(h))) => (w, h),
        Some(_) => {
            return Err(WifiError::InvalidInput(
                "import a floor plan before placing notes".into(),
            ))
        }
    };
    let (x, y) = (input.x, input.y);
    if !(x.is_finite() && y.is_finite()) || x < 0.0 || y < 0.0 || x > width || y > height {
        return Err(WifiError::InvalidInput(
            "the note must be placed on the floor plan".into(),
        ));
    }
    let text = clean_note(Some(&input.text))?
        .ok_or_else(|| WifiError::InvalidInput("the note needs some text".into()))?;
    let category = clean_text(input.category.as_deref(), MAX_CATEGORY_CHARS, "category")?;
    Ok(NotePinInput {
        x,
        y,
        text,
        category,
    })
}

fn query_pins(conn: &Connection, filter: &str, id: i64) -> Result<Vec<NotePin>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PIN_COLUMNS} FROM note_pins WHERE {filter} ORDER BY id"
    ))?;
    let rows = stmt.query_map([id], pin_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl Database {
    // --- Notes on floors, points and APs ------------------------------------

    pub fn get_notes(&self, target: NoteTarget) -> Result<Option<String>> {
        let conn = self.conn();
        floor_of(&conn, target)?;
        let (table, id, _) = target.row();
        Ok(conn.query_row(
            &format!("SELECT notes FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )?)
    }

    /// Replace (or clear, with blank text) a note. Returns what was stored:
    /// trimmed, `None` when blank.
    pub fn set_notes(&self, target: NoteTarget, text: Option<&str>) -> Result<Option<String>> {
        let notes = clean_note(text)?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor_id = floor_of(&tx, target)?;
        let (table, id, _) = target.row();
        tx.execute(
            &format!("UPDATE {table} SET notes = ?2 WHERE id = ?1"),
            params![id, notes],
        )?;
        if let NoteTarget::Ap(id) = target {
            tx.execute(
                "UPDATE placed_aps SET updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        touch(&tx, floor_id, &now)?;
        tx.commit()?;
        Ok(notes)
    }

    // --- Note pins ------------------------------------------------------------

    pub fn list_note_pins(&self, floor_id: i64) -> Result<Vec<NotePin>> {
        query_pins(&self.conn(), "floor_id = ?1", floor_id)
    }

    pub fn create_note_pin(&self, floor_id: i64, input: &NotePinInput) -> Result<NotePin> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let pin = check_pin(&tx, floor_id, input)?;
        tx.execute(
            "INSERT INTO note_pins (floor_id, x, y, text, category, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![floor_id, pin.x, pin.y, pin.text, pin.category, now],
        )?;
        let id = tx.last_insert_rowid();
        touch(&tx, floor_id, &now)?;
        let pin = query_pins(&tx, "id = ?1", id)?
            .pop()
            .ok_or_else(|| WifiError::Database("inserted note pin vanished".into()))?;
        tx.commit()?;
        Ok(pin)
    }

    pub fn update_note_pin(&self, id: i64, input: &NotePinInput) -> Result<NotePin> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor_id: i64 = tx
            .query_row("SELECT floor_id FROM note_pins WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(|| WifiError::InvalidInput(format!("note {id} no longer exists")))?;
        let pin = check_pin(&tx, floor_id, input)?;
        tx.execute(
            "UPDATE note_pins SET x = ?2, y = ?3, text = ?4, category = ?5, updated_at = ?6
             WHERE id = ?1",
            params![id, pin.x, pin.y, pin.text, pin.category, now],
        )?;
        touch(&tx, floor_id, &now)?;
        let pin = query_pins(&tx, "id = ?1", id)?
            .pop()
            .ok_or_else(|| WifiError::Database("updated note pin vanished".into()))?;
        tx.commit()?;
        Ok(pin)
    }

    /// Deletes the pin's photos too (their files go at the next garbage
    /// collection).
    pub fn delete_note_pin(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor_id: Option<i64> = tx
            .query_row("SELECT floor_id FROM note_pins WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(floor_id) = floor_id else {
            return Ok(false);
        };
        tx.execute("DELETE FROM note_pins WHERE id = ?1", [id])?;
        touch(&tx, floor_id, &now)?;
        tx.commit()?;
        Ok(true)
    }

    // --- Report -----------------------------------------------------------------

    /// A floor's notes, note pins and the photos marked for the report (by
    /// their report copies), in one consistent read.
    pub fn floor_annotations(&self, floor_id: i64) -> Result<FloorAnnotations> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let notes = tx
            .query_row("SELECT notes FROM floors WHERE id = ?1", [floor_id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .ok_or_else(|| WifiError::InvalidInput(format!("floor {floor_id} no longer exists")))?;
        let item_notes = |sql: &str| -> Result<Vec<ItemNote>> {
            let mut stmt = tx.prepare(sql)?;
            let rows = stmt.query_map([floor_id], |r| {
                Ok(ItemNote {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    notes: r.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let point_notes = item_notes(
            "SELECT id, NULL, notes FROM survey_points
             WHERE floor_id = ?1 AND notes IS NOT NULL ORDER BY measured_at, id",
        )?;
        let ap_notes = item_notes(
            "SELECT id, name, notes FROM placed_aps
             WHERE floor_id = ?1 AND notes IS NOT NULL ORDER BY id",
        )?;
        let pins = query_pins(&tx, "floor_id = ?1", floor_id)?;
        let photos = super::photos::query(&tx, "floor_id = ?1 AND in_report = 1", floor_id)?
            .into_iter()
            .map(|p| ReportPhoto {
                id: p.id,
                target: p.target,
                report_file: p.report_file,
                caption: p.caption,
                taken_at: p.taken_at,
            })
            .collect();
        drop(tx);
        Ok(FloorAnnotations {
            floor_id,
            notes,
            point_notes,
            ap_notes,
            pins,
            photos,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::photos::tests::{floor_with_plan, stored_photo};
    use crate::survey::models::PlacedApInput;

    #[test]
    fn notes_round_trip_trimmed_and_capped() {
        let (db, floor, point) = floor_with_plan();
        let ap = db
            .create_placed_ap(
                floor,
                &PlacedApInput {
                    name: "Hall".into(),
                    x: 1.0,
                    y: 1.0,
                    model: None,
                    notes: None,
                    bssids: vec![],
                },
            )
            .unwrap();
        for target in [
            NoteTarget::Floor(floor),
            NoteTarget::Point(point),
            NoteTarget::Ap(ap.id),
        ] {
            assert_eq!(db.get_notes(target).unwrap(), None);
            let stored = db.set_notes(target, Some("  metal shelving  ")).unwrap();
            assert_eq!(stored.as_deref(), Some("metal shelving"));
            assert_eq!(db.get_notes(target).unwrap(), stored);
            assert_eq!(db.set_notes(target, Some("   ")).unwrap(), None);
            assert_eq!(db.get_notes(target).unwrap(), None);
        }
        // The cap counts characters, not bytes.
        let at_cap = "é".repeat(MAX_NOTE_CHARS);
        assert!(db
            .set_notes(NoteTarget::Floor(floor), Some(&at_cap))
            .is_ok());
        let err = db
            .set_notes(NoteTarget::Floor(floor), Some(&format!("{at_cap}x")))
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
        assert!(err.to_string().contains("4000"), "{err}");
        // AP edits go through the same cap.
        let mut input = PlacedApInput {
            name: "Hall".into(),
            x: 1.0,
            y: 1.0,
            model: None,
            notes: Some(format!("{at_cap}x")),
            bssids: vec![],
        };
        assert!(db.update_placed_ap(ap.id, &input).is_err());
        input.notes = Some("ceiling".into());
        assert_eq!(
            db.update_placed_ap(ap.id, &input).unwrap().notes.as_deref(),
            Some("ceiling")
        );

        let err = db
            .set_notes(NoteTarget::Point(9999), Some("x"))
            .unwrap_err();
        assert!(err.to_string().contains("no longer exists"), "{err}");
        // Serialised shape the UI sends.
        let t: NoteTarget = serde_json::from_str(r#"{"kind":"ap","id":3}"#).unwrap();
        assert_eq!(t, NoteTarget::Ap(3));
    }

    #[test]
    fn note_pins_crud_and_validation() {
        let (db, floor, _) = floor_with_plan();
        let input = |x: f64, text: &str| NotePinInput {
            x,
            y: 10.0,
            text: text.into(),
            category: Some(" obstruction ".into()),
        };
        let pin = db
            .create_note_pin(floor, &input(5.0, " Metal cabinet "))
            .unwrap();
        assert_eq!(pin.text, "Metal cabinet");
        assert_eq!(pin.category.as_deref(), Some("obstruction"));

        // Off the plan, blank text, NaN: refused.
        assert!(db.create_note_pin(floor, &input(1001.0, "x")).is_err());
        assert!(db.create_note_pin(floor, &input(-1.0, "x")).is_err());
        assert!(db.create_note_pin(floor, &input(f64::NAN, "x")).is_err());
        assert!(db.create_note_pin(floor, &input(5.0, "  ")).is_err());
        let long_category = NotePinInput {
            category: Some("c".repeat(MAX_CATEGORY_CHARS + 1)),
            ..input(5.0, "x")
        };
        assert!(db.create_note_pin(floor, &long_category).is_err());

        let moved = db
            .update_note_pin(
                pin.id,
                &NotePinInput {
                    category: None,
                    ..input(50.0, "Microwave")
                },
            )
            .unwrap();
        assert_eq!(
            (moved.x, moved.text.as_str(), moved.category.as_deref()),
            (50.0, "Microwave", None)
        );
        assert_eq!(db.list_note_pins(floor).unwrap(), vec![moved]);
        assert!(db.delete_note_pin(pin.id).unwrap());
        assert!(!db.delete_note_pin(pin.id).unwrap());
        assert!(db.update_note_pin(pin.id, &input(5.0, "x")).is_err());
        assert!(db.list_note_pins(floor).unwrap().is_empty());
    }

    #[test]
    fn floor_annotations_collect_report_material() {
        use crate::database::photos::PhotoTarget;

        let (db, floor, point) = floor_with_plan();
        db.set_notes(NoteTarget::Floor(floor), Some("Warehouse, 6 m racks"))
            .unwrap();
        db.set_notes(NoteTarget::Point(point), Some("behind the door"))
            .unwrap();
        let pin = db
            .create_note_pin(
                floor,
                &NotePinInput {
                    x: 1.0,
                    y: 1.0,
                    text: "Microwave".into(),
                    category: Some("interference".into()),
                },
            )
            .unwrap();
        let shown = db
            .insert_photo(PhotoTarget::Pin(pin.id), &stored_photo("a"))
            .unwrap();
        let hidden = db
            .insert_photo(PhotoTarget::Floor(floor), &stored_photo("b"))
            .unwrap();
        db.update_photo(hidden.id, Some("not for the customer"), false)
            .unwrap();

        let a = db.floor_annotations(floor).unwrap();
        assert_eq!(a.notes.as_deref(), Some("Warehouse, 6 m racks"));
        assert_eq!(a.point_notes.len(), 1);
        assert_eq!(a.point_notes[0].id, point);
        assert!(a.ap_notes.is_empty());
        assert_eq!(a.pins.len(), 1);
        assert_eq!(a.photos.len(), 1);
        assert_eq!(a.photos[0].id, shown.id);
        assert_eq!(a.photos[0].target, PhotoTarget::Pin(pin.id));
        assert_eq!(a.photos[0].report_file, "photo-a-report.jpg");
        assert!(db.floor_annotations(9999).is_err());
    }
}
