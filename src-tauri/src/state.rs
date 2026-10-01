use std::path::PathBuf;
use std::sync::Arc;

use fresnel_core::database::Database;
use fresnel_core::survey::floorplan::PlanStore;
use fresnel_core::wifi::scanner::Scanner;
use fresnel_core::WifiError;

pub struct AppState {
    pub scanner: Scanner,
    /// The app stays usable for live scanning even if the database can't be
    /// opened (e.g. read-only home); project commands then return this error.
    pub db: Result<Arc<Database>, WifiError>,
    pub db_path: PathBuf,
    pub plans: Arc<PlanStore>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let db_path = data_dir.join("fresnel.db");
        let db = Database::open(&db_path).map(Arc::new);
        let plans = Arc::new(PlanStore::new(data_dir.join("floorplans")));
        match &db {
            Ok(db) => match db.referenced_plan_files() {
                Ok(referenced) => {
                    plans.collect_garbage(&referenced);
                }
                Err(e) => tracing::warn!(error = %e, "skipping floor plan clean-up"),
            },
            Err(e) => {
                tracing::error!(path = %db_path.display(), error = %e, "database unavailable")
            }
        }
        Self {
            scanner: Scanner::new(Arc::new(fresnel_core::default_registry())),
            db,
            db_path,
            plans,
        }
    }

    pub fn db(&self) -> Result<Arc<Database>, WifiError> {
        self.db.clone()
    }
}
