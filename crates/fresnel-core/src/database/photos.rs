//! Photo rows: which floor, point, AP or note pin a photo belongs to, and
//! its three files in the photo store (see `survey::photos`).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::aps::touch;
use super::notes::clean_text;
use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::survey::photos::StoredPhoto;

pub const MAX_CAPTION_CHARS: usize = 500;

/// What a photo is attached to: the floor as a whole, or one point, AP or
/// note pin on it. Serialised as `{ "kind": "pin", "id": 4 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum PhotoTarget {
    Floor(i64),
    Point(i64),
    Ap(i64),
    Pin(i64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Photo {
    pub id: i64,
    pub floor_id: i64,
    pub target: PhotoTarget,
    /// The original as imported, metadata (GPS) included. Evidence only;
    /// never put into a report.
    pub file: String,
    /// Downscaled JPEG without metadata, for reports.
    pub report_file: String,
    pub thumb_file: String,
    /// The original's size, upright.
    pub width: u32,
    pub height: u32,
    /// See [`StoredPhoto::taken_at`].
    pub taken_at: Option<String>,
    pub had_gps: bool,
    pub caption: Option<String>,
    pub in_report: bool,
    pub created_at: DateTime<Utc>,
}

pub(super) const PHOTO_COLUMNS: &str = "id, floor_id, point_id, ap_id, pin_id, file, report_file,
    thumb_file, width, height, taken_at, had_gps, caption, in_report, created_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Photo> {
    let floor_id: i64 = r.get(1)?;
    let target = match (
        r.get::<_, Option<i64>>(2)?,
        r.get::<_, Option<i64>>(3)?,
        r.get::<_, Option<i64>>(4)?,
    ) {
        (Some(id), _, _) => PhotoTarget::Point(id),
        (_, Some(id), _) => PhotoTarget::Ap(id),
        (_, _, Some(id)) => PhotoTarget::Pin(id),
        _ => PhotoTarget::Floor(floor_id),
    };
    Ok(Photo {
        id: r.get(0)?,
        floor_id,
        target,
        file: r.get(5)?,
        report_file: r.get(6)?,
        thumb_file: r.get(7)?,
        width: r.get(8)?,
        height: r.get(9)?,
        taken_at: r.get(10)?,
        had_gps: r.get(11)?,
        caption: r.get(12)?,
        in_report: r.get(13)?,
        created_at: parse_ts(r, 14)?,
    })
}

/// Photos matching `filter` (an SQL condition on `photos`), oldest first.
pub(super) fn query(conn: &Connection, filter: &str, id: i64) -> Result<Vec<Photo>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PHOTO_COLUMNS} FROM photos WHERE {filter} ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The floor the target is on; an error if it's gone.
fn floor_of(conn: &Connection, target: PhotoTarget) -> Result<i64> {
    let (sql, id, what) = match target {
        PhotoTarget::Floor(id) => ("SELECT id FROM floors WHERE id = ?1", id, "floor"),
        PhotoTarget::Point(id) => (
            "SELECT floor_id FROM survey_points WHERE id = ?1",
            id,
            "survey point",
        ),
        PhotoTarget::Ap(id) => (
            "SELECT floor_id FROM placed_aps WHERE id = ?1",
            id,
            "access point",
        ),
        PhotoTarget::Pin(id) => ("SELECT floor_id FROM note_pins WHERE id = ?1", id, "note"),
    };
    conn.query_row(sql, [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| WifiError::InvalidInput(format!("{what} {id} no longer exists")))
}

fn get(conn: &Connection, id: i64) -> Result<Photo> {
    query(conn, "id = ?1", id)?
        .pop()
        .ok_or_else(|| WifiError::InvalidInput(format!("photo {id} no longer exists")))
}

impl Database {
    /// Record an imported photo's files. Called by `PhotoStore::import`
    /// while the files are protected from garbage collection.
    pub fn insert_photo(&self, target: PhotoTarget, stored: &StoredPhoto) -> Result<Photo> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor_id = floor_of(&tx, target)?;
        let (point, ap, pin) = match target {
            PhotoTarget::Floor(_) => (None, None, None),
            PhotoTarget::Point(id) => (Some(id), None, None),
            PhotoTarget::Ap(id) => (None, Some(id), None),
            PhotoTarget::Pin(id) => (None, None, Some(id)),
        };
        tx.execute(
            "INSERT INTO photos (floor_id, point_id, ap_id, pin_id, file, report_file, thumb_file,
                 width, height, taken_at, had_gps, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                floor_id,
                point,
                ap,
                pin,
                stored.file,
                stored.report_file,
                stored.thumb_file,
                stored.width,
                stored.height,
                stored.taken_at,
                stored.had_gps,
                now
            ],
        )?;
        let id = tx.last_insert_rowid();
        touch(&tx, floor_id, &now)?;
        let photo = get(&tx, id)?;
        tx.commit()?;
        Ok(photo)
    }

    /// Every photo on the floor (attached to it or to anything on it).
    pub fn list_floor_photos(&self, floor_id: i64) -> Result<Vec<Photo>> {
        query(&self.conn(), "floor_id = ?1", floor_id)
    }

    pub fn get_photo(&self, id: i64) -> Result<Photo> {
        get(&self.conn(), id)
    }

    pub fn update_photo(&self, id: i64, caption: Option<&str>, in_report: bool) -> Result<Photo> {
        let caption = clean_text(caption, MAX_CAPTION_CHARS, "caption")?;
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let photo = get(&tx, id)?;
        tx.execute(
            "UPDATE photos SET caption = ?2, in_report = ?3 WHERE id = ?1",
            params![id, caption, in_report],
        )?;
        touch(&tx, photo.floor_id, &now)?;
        let photo = get(&tx, id)?;
        tx.commit()?;
        Ok(photo)
    }

    /// Deletes the row; the files go at the next garbage collection.
    pub fn delete_photo(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let floor_id: Option<i64> = tx
            .query_row("SELECT floor_id FROM photos WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(floor_id) = floor_id else {
            return Ok(false);
        };
        tx.execute("DELETE FROM photos WHERE id = ?1", [id])?;
        touch(&tx, floor_id, &now)?;
        tx.commit()?;
        Ok(true)
    }

    /// All three files of every photo row (everything else in the photo
    /// directory is garbage).
    pub fn referenced_photo_files(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT file FROM photos UNION SELECT report_file FROM photos
             UNION SELECT thumb_file FROM photos",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::database::notes::{NotePinInput, NoteTarget};
    use crate::database::projects::NewProject;
    use crate::survey::models::*;
    use crate::wifi::models::AdapterId;

    /// A database with one floor (1000 × 500 px plan) and one point on it.
    /// Returns (db, floor id, point id).
    pub(crate) fn floor_with_plan() -> (Database, i64, i64) {
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
        let f = db
            .create_floor(&NewFloor {
                building_id: b.id,
                name: "F".into(),
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
        let point = db
            .insert_survey_point(&NewSurveyPoint {
                anomalies: vec![],
                floor_id: f.id,
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
                adapter_bands: None,
                samples: vec![],
            })
            .unwrap();
        (db, f.id, point.id)
    }

    pub(crate) fn stored_photo(tag: &str) -> StoredPhoto {
        StoredPhoto {
            file: format!("photo-{tag}.jpg"),
            report_file: format!("photo-{tag}-report.jpg"),
            thumb_file: format!("photo-{tag}-thumb.jpg"),
            width: 4000,
            height: 3000,
            taken_at: Some("2026-09-30T14:05:09".into()),
            had_gps: true,
        }
    }

    #[test]
    fn attach_list_update_delete() {
        let (db, floor, point) = floor_with_plan();
        let photo = db
            .insert_photo(PhotoTarget::Point(point), &stored_photo("a"))
            .unwrap();
        assert_eq!(photo.floor_id, floor);
        assert_eq!(photo.target, PhotoTarget::Point(point));
        assert!(photo.in_report && photo.had_gps && photo.caption.is_none());
        assert_eq!((photo.width, photo.height), (4000, 3000));

        let floor_photo = db
            .insert_photo(PhotoTarget::Floor(floor), &stored_photo("b"))
            .unwrap();
        assert_eq!(floor_photo.target, PhotoTarget::Floor(floor));
        assert_eq!(db.list_floor_photos(floor).unwrap().len(), 2);

        let edited = db
            .update_photo(photo.id, Some("  Server room  "), false)
            .unwrap();
        assert_eq!(edited.caption.as_deref(), Some("Server room"));
        assert!(!edited.in_report);
        assert!(db
            .update_photo(photo.id, Some(&"c".repeat(MAX_CAPTION_CHARS + 1)), true)
            .is_err());

        assert!(db
            .insert_photo(PhotoTarget::Ap(424242), &stored_photo("c"))
            .is_err());
        assert!(db.delete_photo(floor_photo.id).unwrap());
        assert!(!db.delete_photo(floor_photo.id).unwrap());
        assert!(db.get_photo(floor_photo.id).is_err());
        let files = db.referenced_photo_files().unwrap();
        assert_eq!(
            files,
            ["photo-a.jpg", "photo-a-report.jpg", "photo-a-thumb.jpg"]
                .map(String::from)
                .into()
        );
    }

    #[test]
    fn photos_go_with_what_they_are_attached_to() {
        let (db, floor, point) = floor_with_plan();
        let pin = db
            .create_note_pin(
                floor,
                &NotePinInput {
                    x: 1.0,
                    y: 1.0,
                    text: "Cabinet".into(),
                    category: None,
                },
            )
            .unwrap();
        db.insert_photo(PhotoTarget::Point(point), &stored_photo("p"))
            .unwrap();
        db.insert_photo(PhotoTarget::Pin(pin.id), &stored_photo("n"))
            .unwrap();
        db.insert_photo(PhotoTarget::Floor(floor), &stored_photo("f"))
            .unwrap();
        db.set_notes(NoteTarget::Point(point), Some("x")).unwrap();

        assert!(db.delete_survey_point(point).unwrap());
        assert!(db.delete_note_pin(pin.id).unwrap());
        let left = db.list_floor_photos(floor).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].target, PhotoTarget::Floor(floor));
        assert_eq!(db.referenced_photo_files().unwrap().len(), 3);

        assert!(db.delete_floor(floor).unwrap());
        assert!(db.referenced_photo_files().unwrap().is_empty());
    }

    #[test]
    fn schema_refuses_a_photo_on_two_things() {
        let (db, floor, point) = floor_with_plan();
        let pin = db
            .create_note_pin(
                floor,
                &NotePinInput {
                    x: 1.0,
                    y: 1.0,
                    text: "x".into(),
                    category: None,
                },
            )
            .unwrap();
        let err = db.conn().execute(
            "INSERT INTO photos (floor_id, point_id, pin_id, file, report_file, thumb_file,
                 width, height, had_gps, created_at)
             VALUES (?1, ?2, ?3, 'a', 'b', 'c', 1, 1, 0, 'x')",
            params![floor, point, pin.id],
        );
        assert!(err.is_err());
    }
}
