//! The user's classification of BSSIDs (ours, not placed / neighbour /
//! ignored), once per project.

use chrono::Utc;
use rusqlite::{params, Connection};

use super::survey::{enum_parse, enum_text};
use super::{parse_ts, Database};
use crate::error::{Result, WifiError};
use crate::survey::findings::{BssidMark, MarkStatus};

/// Notes longer than this are refused rather than cut.
const MAX_NOTE_CHARS: usize = 2000;

pub(super) fn list(conn: &Connection, project_id: i64) -> Result<Vec<BssidMark>> {
    let mut stmt = conn.prepare(
        "SELECT bssid, status, note, updated_at FROM bssid_marks
         WHERE project_id = ?1 ORDER BY bssid",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        let status: String = r.get(1)?;
        Ok(BssidMark {
            bssid: r.get(0)?,
            // The CHECK constraint allows nothing else; fall back to the
            // mildest status if a newer Fresnel added one.
            status: enum_parse(&status, MarkStatus::Ignored),
            note: r.get(2)?,
            updated_at: parse_ts(r, 3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn bssid(s: &str) -> Result<String> {
    super::aps::normalize_bssid(s)
        .ok_or_else(|| WifiError::InvalidInput(format!("'{s}' is not a BSSID (MAC address)")))
}

impl Database {
    pub fn list_bssid_marks(&self, project_id: i64) -> Result<Vec<BssidMark>> {
        list(&self.conn(), project_id)
    }

    /// Set (or replace) the mark on a BSSID.
    pub fn set_bssid_mark(
        &self,
        project_id: i64,
        bssid_text: &str,
        status: MarkStatus,
        note: Option<String>,
    ) -> Result<BssidMark> {
        let bssid = bssid(bssid_text)?;
        let note = note
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(String::from);
        if note
            .as_ref()
            .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
        {
            return Err(WifiError::InvalidInput(format!(
                "the note is longer than {MAX_NOTE_CHARS} characters"
            )));
        }
        let now = Utc::now();
        let conn = self.conn();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(WifiError::InvalidInput(format!(
                "project {project_id} no longer exists"
            )));
        }
        conn.execute(
            "INSERT INTO bssid_marks (project_id, bssid, status, note, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (project_id, bssid) DO UPDATE
                 SET status = excluded.status, note = excluded.note, updated_at = excluded.updated_at",
            params![project_id, bssid, enum_text(&status), note, now.to_rfc3339()],
        )?;
        Ok(BssidMark {
            bssid,
            status,
            note,
            updated_at: now,
        })
    }

    pub fn clear_bssid_mark(&self, project_id: i64, bssid_text: &str) -> Result<bool> {
        let bssid = bssid(bssid_text)?;
        Ok(self.conn().execute(
            "DELETE FROM bssid_marks WHERE project_id = ?1 AND bssid = ?2",
            params![project_id, bssid],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::projects::NewProject;

    #[test]
    fn set_replace_list_clear_and_cascade() {
        let db = Database::open_in_memory().unwrap();
        let p = db
            .create_project(&NewProject {
                name: "HQ".into(),
                customer_name: None,
            })
            .unwrap();
        db.set_bssid_mark(p.id, "aa:bb:cc:dd:ee:ff", MarkStatus::Neighbour, None)
            .unwrap();
        db.set_bssid_mark(
            p.id,
            "AA-BB-CC-DD-EE-FF",
            MarkStatus::OursUnplaced,
            Some("store room".into()),
        )
        .unwrap();
        let marks = db.list_bssid_marks(p.id).unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].bssid, "AA:BB:CC:DD:EE:FF");
        assert_eq!(marks[0].status, MarkStatus::OursUnplaced);
        assert_eq!(marks[0].note.as_deref(), Some("store room"));

        assert_eq!(
            db.set_bssid_mark(p.id, "nope", MarkStatus::Ignored, None)
                .unwrap_err()
                .kind(),
            "invalid_input"
        );
        assert!(db
            .set_bssid_mark(
                p.id,
                "AA:BB:CC:DD:EE:01",
                MarkStatus::Ignored,
                Some("x".repeat(2001))
            )
            .is_err());
        assert!(db
            .set_bssid_mark(p.id + 1, "AA:BB:CC:DD:EE:01", MarkStatus::Ignored, None)
            .is_err());

        assert!(db.delete_project(p.id).unwrap());
        assert!(db.list_bssid_marks(p.id).unwrap().is_empty());
    }
}
