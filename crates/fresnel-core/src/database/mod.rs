//! Local SQLite storage.
//!
//! The database is only reachable through Rust; the webview never sees SQL.
//! rusqlite is synchronous, so callers on async code paths should go through
//! `tokio::task::spawn_blocking` (the Tauri command layer does).

pub mod aps;
pub mod findings;
pub mod marks;
mod migrations;
pub mod notes;
pub mod photos;
pub mod point_tests;
pub mod projects;
pub mod requirements;
pub mod survey;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, ErrorCode, Row};
use tracing::{info, warn};

use crate::error::{Result, WifiError};

/// Backups made before schema upgrades that are kept next to the database.
const KEEP_BACKUPS: usize = 3;

pub struct Database {
    conn: Mutex<Connection>,
}

/// Why [`Database::open_checked`] failed. Callers recover differently: a
/// damaged file can be set aside, a busy one opened again later.
#[derive(Debug)]
pub enum OpenError {
    /// `PRAGMA quick_check` found damage, or SQLite says the file is
    /// corrupt or not a database at all.
    Corrupt(String),
    /// Another connection held a lock past the busy timeout; opening again
    /// later may succeed.
    Busy(WifiError),
    /// Anything else (no permission, disk full, newer schema, ...).
    Failed(WifiError),
}

impl OpenError {
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::Corrupt(_))
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(detail) => write!(f, "the database file is damaged: {detail}"),
            Self::Busy(e) | Self::Failed(e) => e.fmt(f),
        }
    }
}

impl From<OpenError> for WifiError {
    fn from(e: OpenError) -> Self {
        match e {
            OpenError::Corrupt(_) => WifiError::Database(e.to_string()),
            OpenError::Busy(e) | OpenError::Failed(e) => e,
        }
    }
}

impl From<WifiError> for OpenError {
    fn from(e: WifiError) -> Self {
        Self::Failed(e)
    }
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        match e.sqlite_error_code() {
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                Self::Corrupt(e.to_string())
            }
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy(e.into()),
            _ => Self::Failed(e.into()),
        }
    }
}

impl From<rusqlite_migration::Error> for OpenError {
    fn from(e: rusqlite_migration::Error) -> Self {
        let code = match &e {
            rusqlite_migration::Error::RusqliteError { err, .. } => err.sqlite_error_code(),
            _ => None,
        };
        match code {
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                Self::Corrupt(e.to_string())
            }
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Self::Busy(e.into()),
            _ => Self::Failed(e.into()),
        }
    }
}

impl Database {
    /// Open (creating if necessary) and migrate the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_checked(path).map_err(Into::into)
    }

    /// Like [`Database::open`], but tells a damaged or busy file apart from
    /// other failures. Before a schema upgrade the database is backed up
    /// next to itself (`<name>.bak-v{old}-{timestamp}`); if that fails,
    /// nothing is migrated.
    pub fn open_checked(path: &Path) -> Result<Self, OpenError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                WifiError::Database(format!("cannot create {}: {e}", dir.display()))
            })?;
        }
        let conn = Connection::open(path)?;
        let db = Self::init(conn, Some(path))?;
        info!(path = %path.display(), version = db.schema_version()?, "database ready");
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::init(Connection::open_in_memory()?, None)?)
    }

    fn init(mut conn: Connection, path: Option<&Path>) -> Result<Self, OpenError> {
        // The busy timeout must come first: switching to WAL takes a lock,
        // and without a timeout a briefly busy database fails to open.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        quick_check(&conn)?;
        migrate(&mut conn, |conn, from| match path {
            Some(path) => backup_before_migration(conn, path, from).map(|_| ()),
            None => Ok(()),
        })?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite inconsistent
        // (statements are transactional), so recover from poisoning.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The newest schema this build knows (and migrates to).
    pub fn latest_schema_version() -> u32 {
        migrations::latest_version()
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

/// `PRAGMA quick_check`: verifies the b-tree structure of every table and
/// index (not that indexes match their tables, which a full
/// `integrity_check` does at much higher cost).
fn quick_check(conn: &Connection) -> Result<(), OpenError> {
    let mut stmt = conn.prepare("PRAGMA quick_check")?;
    let problems = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if problems.len() == 1 && problems[0] == "ok" {
        return Ok(());
    }
    let mut detail = problems
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if problems.len() > 5 {
        detail.push_str(&format!(" (and {} more)", problems.len() - 5));
    }
    Err(OpenError::Corrupt(detail))
}

/// Bring the schema up to date. `backup` runs first whenever an existing
/// database (version > 0) is about to change; its failure aborts.
fn migrate(
    conn: &mut Connection,
    backup: impl FnOnce(&Connection, u32) -> Result<()>,
) -> Result<(), OpenError> {
    let latest = migrations::latest_version();
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = u32::try_from(current).map_err(|_| {
        WifiError::Database(format!(
            "the database has an invalid schema version ({current}); it was not written by Fresnel"
        ))
    })?;
    if current > latest {
        return Err(WifiError::Database(format!(
            "this database was made by a newer version of Fresnel (schema v{current}, \
             this version supports up to v{latest}); update Fresnel to open it"
        ))
        .into());
    }
    if current == latest {
        return Ok(());
    }
    if current > 0 {
        backup(conn, current).map_err(|e| {
            WifiError::Database(format!(
                "not upgrading the database from schema v{current} to v{latest} because the \
                 backup beforehand failed ({e}); the database was left unchanged"
            ))
        })?;
    }
    migrations::migrations().to_latest(conn)?;
    info!(from = current, to = latest, "database schema migrated");
    Ok(())
}

/// Checkpoint the WAL, then `VACUUM INTO` a copy next to the database and
/// prune older copies. Returns the backup's path.
fn backup_before_migration(conn: &Connection, db_path: &Path, from: u32) -> Result<PathBuf> {
    // Fold the WAL into the main file so what is left on disk is
    // self-contained. VACUUM INTO copies all committed data either way, so
    // a checkpoint blocked by a reader is not fatal.
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        warn!("WAL checkpoint before backup was blocked by another connection");
    }
    let target = backup_path(db_path, from, Utc::now());
    let target_str = target.to_str().ok_or_else(|| {
        WifiError::Database(format!(
            "backup path {} is not valid UTF-8",
            target.display()
        ))
    })?;
    if let Err(e) = conn.execute("VACUUM INTO ?1", [target_str]) {
        // Don't leave a partial copy that looks like a good backup.
        let _ = std::fs::remove_file(&target);
        return Err(WifiError::Database(format!(
            "cannot write backup {}: {e}",
            target.display()
        )));
    }
    info!(path = %target.display(), schema = from, "database backed up before migration");
    prune_backups(db_path, KEEP_BACKUPS);
    Ok(target)
}

/// `<db file name>.bak-v{version}-{UTC timestamp}`. The timestamp is fixed
/// width, so names sort chronologically; no colons, for Windows.
fn backup_path(db_path: &Path, version: u32, at: DateTime<Utc>) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".bak-v{version}-{}",
        at.format("%Y%m%dT%H%M%S%.3fZ")
    ));
    db_path.with_file_name(name)
}

/// Delete all but the `keep` newest backups of `db_path`. Files that don't
/// match the backup name pattern are never touched.
fn prune_backups(db_path: &Path, keep: usize) {
    let (Some(dir), Some(db_name)) = (db_path.parent(), db_path.file_name()) else {
        return;
    };
    let prefix = format!("{}.bak-v", db_name.to_string_lossy());
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "cannot list database backups");
            return;
        }
    };
    let mut backups: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let (version, stamp) = name.strip_prefix(&prefix)?.split_once('-')?;
            let valid = !version.is_empty()
                && version.bytes().all(|b| b.is_ascii_digit())
                && stamp.ends_with('Z');
            valid.then(|| (stamp.to_string(), entry.path()))
        })
        .collect();
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in backups.into_iter().skip(keep) {
        match std::fs::remove_file(&path) {
            Ok(()) => info!(path = %path.display(), "removed old database backup"),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot remove old database backup")
            }
        }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "fresnel-db-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn user_version(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn upgrade_backs_up_the_old_schema_first() {
        let dir = TempDir::new("upgrade");
        let path = dir.0.join("fresnel.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            migrations::migrations().to_version(&mut conn, 1).unwrap();
            conn.execute(
                "INSERT INTO projects (name, created_at, updated_at) VALUES ('Old', 'x', 'x')",
                [],
            )
            .unwrap();
        }

        let db = Database::open_checked(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::latest_version());
        drop(db);

        let backups: Vec<String> = dir
            .names()
            .into_iter()
            .filter(|n| n.starts_with("fresnel.db.bak-v1-"))
            .collect();
        assert_eq!(backups.len(), 1, "{:?}", dir.names());
        let backup = dir.0.join(&backups[0]);
        assert_eq!(user_version(&backup), 1);
        let name: String = Connection::open(&backup)
            .unwrap()
            .query_row("SELECT name FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Old");

        // Already up to date: no further backup.
        drop(Database::open_checked(&path).unwrap());
        assert_eq!(
            dir.names().iter().filter(|n| n.contains(".bak-v")).count(),
            1
        );
    }

    #[test]
    fn new_database_is_not_backed_up() {
        let dir = TempDir::new("fresh");
        drop(Database::open_checked(&dir.0.join("fresnel.db")).unwrap());
        assert!(!dir.names().iter().any(|n| n.contains(".bak-v")));
    }

    #[test]
    fn failed_backup_aborts_the_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::migrations().to_version(&mut conn, 1).unwrap();

        let err = migrate(&mut conn, |_, from| {
            assert_eq!(from, 1);
            Err(WifiError::Database("disk full".into()))
        })
        .unwrap_err();
        let msg = WifiError::from(err).to_string();
        assert!(msg.contains("backup") && msg.contains("disk full"), "{msg}");

        let version: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn only_the_newest_backups_are_kept() {
        let dir = TempDir::new("prune");
        let db = dir.0.join("fresnel.db");
        let files = [
            "fresnel.db",
            "fresnel.db.bak-v1-20260101T000000.000Z",
            "fresnel.db.bak-v2-20260301T000000.000Z",
            "fresnel.db.bak-v2-20260201T000000.000Z",
            "fresnel.db.bak-v10-20260401T000000.000Z",
            "fresnel.db.bak-vX-20250101T000000.000Z",
            "other.db.bak-v1-20250101T000000.000Z",
        ];
        for f in files {
            std::fs::write(dir.0.join(f), b"").unwrap();
        }

        prune_backups(&db, 3);

        assert_eq!(
            dir.names(),
            [
                "fresnel.db",
                "fresnel.db.bak-v10-20260401T000000.000Z",
                "fresnel.db.bak-v2-20260201T000000.000Z",
                "fresnel.db.bak-v2-20260301T000000.000Z",
                "fresnel.db.bak-vX-20250101T000000.000Z",
                "other.db.bak-v1-20250101T000000.000Z",
            ]
        );
    }

    #[test]
    fn backup_names_sort_chronologically() {
        let at = DateTime::parse_from_rfc3339("2026-10-02T09:05:03.042Z")
            .unwrap()
            .with_timezone(&Utc);
        let path = backup_path(Path::new("/data/fresnel.db"), 3, at);
        assert_eq!(
            path,
            Path::new("/data/fresnel.db.bak-v3-20261002T090503.042Z")
        );
    }

    #[test]
    fn newer_schema_is_refused_with_a_clear_message() {
        let dir = TempDir::new("newer");
        let path = dir.0.join("fresnel.db");
        let latest = migrations::latest_version();
        drop(Database::open_checked(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", latest + 2)
            .unwrap();

        let err = Database::open_checked(&path).err().unwrap();
        assert!(!err.is_corrupt() && !err.is_transient());
        let msg = WifiError::from(err).to_string();
        assert!(
            msg.contains(&format!(
                "made by a newer version of Fresnel (schema v{}, this version supports up to v{latest})",
                latest + 2
            )),
            "{msg}"
        );
        assert_eq!(user_version(&path), i64::from(latest + 2));
        assert!(!dir.names().iter().any(|n| n.contains(".bak-v")));
    }

    #[test]
    fn open_errors_are_classified() {
        let sqlite = |code| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
        assert!(OpenError::from(sqlite(rusqlite::ffi::SQLITE_BUSY)).is_transient());
        assert!(OpenError::from(sqlite(rusqlite::ffi::SQLITE_LOCKED)).is_transient());
        assert!(OpenError::from(sqlite(rusqlite::ffi::SQLITE_CORRUPT)).is_corrupt());
        assert!(OpenError::from(sqlite(rusqlite::ffi::SQLITE_NOTADB)).is_corrupt());
        let other = OpenError::from(sqlite(rusqlite::ffi::SQLITE_CANTOPEN));
        assert!(!other.is_transient() && !other.is_corrupt());
    }

    #[test]
    fn a_file_that_is_not_a_database_is_corrupt() {
        let dir = TempDir::new("garbage");
        let path = dir.0.join("fresnel.db");
        std::fs::write(&path, "definitely not SQLite ".repeat(500)).unwrap();
        let err = Database::open_checked(&path).err().unwrap();
        assert!(err.is_corrupt(), "{err}");
    }

    #[test]
    fn damaged_pages_are_reported_corrupt() {
        let dir = TempDir::new("damaged");
        let path = dir.0.join("fresnel.db");
        {
            let db = Database::open_checked(&path).unwrap();
            let conn = db.conn();
            conn.execute_batch(
                "CREATE TABLE filler (id INTEGER PRIMARY KEY, body TEXT NOT NULL);
                 CREATE INDEX idx_filler_body ON filler(body);",
            )
            .unwrap();
            for i in 0..2000 {
                conn.execute(
                    "INSERT INTO filler (body) VALUES (?1)",
                    [format!("row {i:05} {}", "x".repeat(100))],
                )
                .unwrap();
            }
        }
        // Closing the last connection checkpointed the WAL into the file.
        let mut bytes = std::fs::read(&path).unwrap();
        let page = 4096;
        assert!(bytes.len() > 20 * page);
        let start = bytes.len() - 6 * page;
        bytes[start..start + 3 * page].fill(0xA5);
        std::fs::write(&path, bytes).unwrap();

        let err = Database::open_checked(&path).err().unwrap();
        assert!(err.is_corrupt(), "{err}");
    }
}
