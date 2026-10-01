//! Local SQLite storage.
//!
//! The database is only reachable through Rust; the webview never sees SQL.
//! rusqlite is synchronous, so callers on async code paths should go through
//! `tokio::task::spawn_blocking` (the Tauri command layer does).

pub mod aps;
mod migrations;
pub mod projects;
pub mod survey;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row};
use tracing::info;

use crate::error::{Result, WifiError};

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Open (creating if necessary) and migrate the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                WifiError::Database(format!("cannot create {}: {e}", dir.display()))
            })?;
        }
        let conn = Connection::open(path)?;
        let db = Self::init(conn)?;
        info!(path = %path.display(), version = db.schema_version()?, "database ready");
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        migrations::migrations().to_latest(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite inconsistent
        // (statements are transactional), so recover from poisoning.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

/// Read an RFC 3339 TEXT column as a UTC timestamp.
pub(crate) fn parse_ts(r: &Row<'_>, idx: usize) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = r.get(idx)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
        })
}
