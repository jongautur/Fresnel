use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{parse_ts, Database};
use crate::error::{Result, WifiError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub customer_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProject {
    pub name: String,
    pub customer_name: Option<String>,
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        customer_name: r.get(2)?,
        created_at: parse_ts(r, 3)?,
        updated_at: parse_ts(r, 4)?,
    })
}

const COLUMNS: &str = "id, name, customer_name, created_at, updated_at";

impl Database {
    pub fn create_project(&self, new: &NewProject) -> Result<Project> {
        let name = new.name.trim();
        if name.is_empty() {
            return Err(WifiError::InvalidInput(
                "project name must not be empty".into(),
            ));
        }
        let customer = new
            .customer_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO projects (name, customer_name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![name, customer, now],
        )?;
        let id = conn.last_insert_rowid();
        Ok(conn.query_row(
            &format!("SELECT {COLUMNS} FROM projects WHERE id = ?1"),
            [id],
            from_row,
        )?)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM projects ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_project(&self, id: i64) -> Result<Option<Project>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM projects WHERE id = ?1"),
                [id],
                from_row,
            )
            .optional()?)
    }

    pub fn delete_project(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM projects WHERE id = ?1", [id])?
            > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.schema_version().unwrap() >= 1);
        let p = db
            .create_project(&NewProject {
                name: "  HQ survey ".into(),
                customer_name: Some("".into()),
            })
            .unwrap();
        assert_eq!(p.name, "HQ survey");
        assert_eq!(p.customer_name, None);
        assert_eq!(db.list_projects().unwrap().len(), 1);
        assert_eq!(db.get_project(p.id).unwrap().unwrap(), p);
        assert!(db.delete_project(p.id).unwrap());
        assert!(db.get_project(p.id).unwrap().is_none());
    }

    #[test]
    fn rejects_empty_name() {
        let db = Database::open_in_memory().unwrap();
        let err = db
            .create_project(&NewProject {
                name: "  ".into(),
                customer_name: None,
            })
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }
}
