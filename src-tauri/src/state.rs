use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use fresnel_core::database::{Database, OpenError};
use fresnel_core::nettools::Cancel;
use fresnel_core::settings::SettingsStore;
use fresnel_core::survey::floorplan::PlanStore;
use fresnel_core::survey::photos::PhotoStore;
use fresnel_core::wifi::scanner::Scanner;
use fresnel_core::WifiError;
use tokio::sync::watch;

pub struct AppState {
    pub scanner: Scanner,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub plans: Arc<PlanStore>,
    pub photos: Arc<PhotoStore>,
    /// Report branding (settings.json + logo in the data directory).
    pub settings: Arc<SettingsStore>,
    /// The last file `save_export` wrote: the only path "Open" may open.
    pub last_export: Mutex<Option<PathBuf>>,
    /// Running active tests by the UI's test ID. Cancelling is cooperative:
    /// the tests check their token at every probe and data-loop boundary,
    /// and every network phase has its own deadline anyway.
    test_cancellations: Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Running Tools page runs by the UI's run ID, and whether each is
    /// bound to a Wi-Fi adapter (those don't overlap with point tests).
    tool_runs: Mutex<HashMap<String, ToolSlot>>,
    /// The app stays usable for live scanning even if the database can't be
    /// opened (e.g. read-only home); project commands then return the error.
    db: Mutex<DbSlot>,
}

/// Most Tools runs at once (ping during iperf3 is useful; a dozen sweeps
/// at once only disturb each other).
pub const MAX_TOOL_RUNS: usize = 4;

struct ToolSlot {
    cancel: watch::Sender<bool>,
    wifi_bound: bool,
}

struct DbSlot {
    state: DbState,
    /// Something the user should be told about the database (e.g. that a
    /// damaged file was set aside). Kept until the app exits.
    notice: Option<String>,
}

enum DbState {
    Ready(Arc<Database>),
    /// Opening failed in a way that may pass (database busy); [`AppState::db`]
    /// tries again on the next call.
    Retry(WifiError),
    Failed(WifiError),
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let db_path = data_dir.join("fresnel.db");
        let plans_dir = data_dir.join("floorplans");
        let photos_dir = data_dir.join("photos");
        let (state, notice) = open_or_recover(&db_path, &[&plans_dir, &photos_dir]);
        let plans = Arc::new(PlanStore::new(plans_dir));
        let photos = Arc::new(PhotoStore::new(photos_dir));
        let settings = Arc::new(SettingsStore::new(data_dir.clone()));
        if let DbState::Ready(db) = &state {
            collect_garbage(db, &plans, &photos);
        }
        Self {
            scanner: Scanner::new(Arc::new(fresnel_core::default_registry())),
            data_dir,
            db_path,
            plans,
            photos,
            settings,
            last_export: Mutex::new(None),
            test_cancellations: Mutex::new(HashMap::new()),
            tool_runs: Mutex::new(HashMap::new()),
            db: Mutex::new(DbSlot { state, notice }),
        }
    }

    /// The database, opening it again first if the last attempt hit a busy
    /// database. Commands are user-driven, so this retries at most once per
    /// command; a retry can block for up to SQLite's busy timeout.
    pub fn db(&self) -> Result<Arc<Database>, WifiError> {
        let mut slot = self.db.lock().unwrap_or_else(|p| p.into_inner());
        match &slot.state {
            DbState::Ready(db) => return Ok(db.clone()),
            DbState::Failed(e) => return Err(e.clone()),
            DbState::Retry(_) => {}
        }
        tracing::info!(path = %self.db_path.display(), "retrying to open the database");
        let (state, notice) =
            open_or_recover(&self.db_path, &[self.plans.dir(), self.photos.dir()]);
        if let DbState::Ready(db) = &state {
            collect_garbage(db, &self.plans, &self.photos);
        }
        if notice.is_some() {
            slot.notice = notice;
        }
        slot.state = state;
        match &slot.state {
            DbState::Ready(db) => Ok(db.clone()),
            DbState::Retry(e) | DbState::Failed(e) => Err(e.clone()),
        }
    }

    /// See [`DbSlot::notice`].
    pub fn db_notice(&self) -> Option<String> {
        let slot = self.db.lock().unwrap_or_else(|p| p.into_inner());
        slot.notice.clone()
    }

    /// Register a running test so `cancel_test` can stop it. One run at a
    /// time: tests on one Wi-Fi link would disturb each other.
    pub fn begin_test(&self, id: &str) -> Result<Cancel, WifiError> {
        let mut tests = self
            .test_cancellations
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !tests.is_empty() {
            return Err(WifiError::InvalidInput(
                "other tests are still running; wait for them or cancel them".into(),
            ));
        }
        if self.tools().values().any(|t| t.wifi_bound) {
            return Err(WifiError::InvalidInput(
                "a Tools run bound to Wi-Fi is still running; stop it first, so it doesn't \
                 disturb the point's tests"
                    .into(),
            ));
        }
        let (sender, cancel) = Cancel::new();
        tests.insert(id.to_owned(), sender);
        Ok(cancel)
    }

    pub fn finish_test(&self, id: &str) {
        self.test_cancellations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }

    fn tools(&self) -> std::sync::MutexGuard<'_, HashMap<String, ToolSlot>> {
        self.tool_runs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Register a Tools run so `cancel_tool` can stop it.
    pub fn begin_tool(&self, id: &str, wifi_bound: bool) -> Result<Cancel, WifiError> {
        // Lock order: tests, then tools (as in `begin_test`).
        let tests = self
            .test_cancellations
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut tools = self.tools();
        if tools.contains_key(id) {
            return Err(WifiError::InvalidInput(format!(
                "run {id} is already running"
            )));
        }
        if tools.len() >= MAX_TOOL_RUNS {
            return Err(WifiError::InvalidInput(format!(
                "{MAX_TOOL_RUNS} tools are already running; stop one first"
            )));
        }
        if wifi_bound && !tests.is_empty() {
            return Err(WifiError::InvalidInput(
                "a survey point's tests are running on Wi-Fi; wait for them first".into(),
            ));
        }
        let (sender, cancel) = Cancel::new();
        tools.insert(
            id.to_owned(),
            ToolSlot {
                cancel: sender,
                wifi_bound,
            },
        );
        Ok(cancel)
    }

    pub fn finish_tool(&self, id: &str) {
        self.tools().remove(id);
    }

    /// False if no such run is going (it may just have finished).
    pub fn cancel_tool(&self, id: &str) -> bool {
        self.tools()
            .get(id)
            .is_some_and(|t| t.cancel.send(true).is_ok())
    }

    /// False if no such test is running (it may just have finished).
    pub fn cancel_test(&self, id: &str) -> bool {
        self.test_cancellations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .is_some_and(|tx| tx.send(true).is_ok())
    }
}

fn collect_garbage(db: &Database, plans: &PlanStore, photos: &PhotoStore) {
    if let Err(e) = plans.collect_garbage(|| db.referenced_plan_files()) {
        tracing::warn!(error = %e, "skipping floor plan clean-up");
    }
    if let Err(e) = photos.collect_garbage(|| db.referenced_photo_files()) {
        tracing::warn!(error = %e, "skipping photo clean-up");
    }
}

/// `file_dirs`: the directories of files the database references (plans,
/// photos); they are set aside with a damaged database.
fn open_or_recover(db_path: &Path, file_dirs: &[&Path]) -> (DbState, Option<String>) {
    match Database::open_checked(db_path) {
        Ok(db) => (DbState::Ready(Arc::new(db)), None),
        Err(OpenError::Corrupt(detail)) => recover_corrupt(db_path, file_dirs, &detail),
        Err(e) if e.is_transient() => {
            tracing::warn!(path = %db_path.display(), error = %e, "database busy; will retry");
            (DbState::Retry(e.into()), None)
        }
        Err(e) => {
            tracing::error!(path = %db_path.display(), error = %e, "database unavailable");
            (DbState::Failed(e.into()), None)
        }
    }
}

/// Move a damaged database (with its WAL and shared-memory files, and the
/// floor plans it references) aside as `<name>.corrupt-{timestamp}` and
/// start a fresh one. Nothing is deleted: the old file can still be
/// recovered with `sqlite3 ... .recover`.
fn recover_corrupt(db_path: &Path, file_dirs: &[&Path], detail: &str) -> (DbState, Option<String>) {
    tracing::error!(
        path = %db_path.display(),
        %detail,
        "DATABASE IS DAMAGED: setting it aside and starting a new one"
    );
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let aside = with_suffix(db_path, &format!(".corrupt-{stamp}"));
    let fail = |what: String| {
        let msg = format!(
            "the database {} is damaged ({detail}) and {what}",
            db_path.display()
        );
        tracing::error!("{msg}");
        (DbState::Failed(WifiError::Database(msg)), None)
    };

    // Plans and photos first: a new database references none, so garbage
    // collection would delete every file the damaged one still needs.
    // Never start a new database while they are still in place.
    let mut moved: Vec<(&Path, PathBuf)> = Vec::new();
    let put_back = |moved: &[(&Path, PathBuf)]| {
        for (dir, target) in moved {
            let _ = std::fs::rename(target, dir);
        }
    };
    for dir in file_dirs.iter().copied().filter(|d| d.exists()) {
        let target = with_suffix(dir, &format!(".corrupt-{stamp}"));
        if let Err(e) = std::fs::rename(dir, &target) {
            put_back(&moved);
            return fail(format!("{} could not be moved aside: {e}", dir.display()));
        }
        moved.push((dir, target));
    }
    if let Err(e) = std::fs::rename(db_path, &aside) {
        put_back(&moved);
        return fail(format!("could not be moved aside: {e}"));
    }
    // SQLite pairs `<db>-wal` with `<db>`, so the set-aside copy keeps its
    // WAL; a stale WAL left behind must not meet the new database.
    for suffix in ["-wal", "-shm"] {
        let from = with_suffix(db_path, suffix);
        if !from.exists() {
            continue;
        }
        if let Err(e) = std::fs::rename(&from, with_suffix(&aside, suffix)) {
            return fail(format!("its {suffix} file could not be moved aside: {e}"));
        }
    }

    let files_note: String = moved
        .iter()
        .map(|(_, p)| format!(", {}", p.display()))
        .collect();
    let notice = format!(
        "Fresnel's database was damaged ({detail}). It was set aside as {}{files_note} \
         (its floor plans and photos), and a new, empty database was started.",
        aside.display()
    );
    tracing::error!("{notice}");
    let state = match Database::open_checked(db_path) {
        Ok(db) => DbState::Ready(Arc::new(db)),
        Err(e) => {
            tracing::error!(path = %db_path.display(), error = %e, "cannot create a new database");
            DbState::Failed(e.into())
        }
    };
    (state, Some(notice))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fresnel-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn damaged_database_is_set_aside_with_its_plans() {
        let dir = temp_dir("corrupt");
        let garbage = "not a database ".repeat(400);
        std::fs::write(dir.join("fresnel.db"), &garbage).unwrap();
        std::fs::create_dir_all(dir.join("floorplans")).unwrap();
        std::fs::write(dir.join("floorplans/plan-1.png"), b"png").unwrap();
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        std::fs::write(dir.join("photos/photo-1.jpg"), b"jpg").unwrap();

        let state = AppState::new(dir.clone());
        let db = state.db().expect("a fresh database");
        assert_eq!(
            db.schema_version().unwrap(),
            Database::latest_schema_version()
        );
        let notice = state.db_notice().expect("a notice for the UI");
        assert!(notice.contains("damaged"), "{notice}");

        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        let aside = names
            .iter()
            .find(|n| n.starts_with("fresnel.db.corrupt-"))
            .expect("database set aside");
        assert_eq!(std::fs::read_to_string(dir.join(aside)).unwrap(), garbage);
        let plans = names
            .iter()
            .find(|n| n.starts_with("floorplans.corrupt-"))
            .expect("plans set aside");
        assert!(dir.join(plans).join("plan-1.png").exists());
        let photos = names
            .iter()
            .find(|n| n.starts_with("photos.corrupt-"))
            .expect("photos set aside");
        assert!(dir.join(photos).join("photo-1.jpg").exists());

        drop((db, state));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn healthy_database_has_no_notice() {
        let dir = temp_dir("healthy");
        let state = AppState::new(dir.clone());
        assert!(state.db().is_ok());
        assert!(state.db_notice().is_none());
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}
