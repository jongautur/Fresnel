use std::path::PathBuf;
use std::sync::Arc;

use fresnel_core::database::Database;
use fresnel_core::wifi::scanner::Scanner;
use fresnel_core::WifiError;

pub struct AppState {
    pub scanner: Scanner,
    /// The app stays usable for live scanning even if the database can't be
    /// opened (e.g. read-only home); project commands then return this error.
    pub db: Result<Arc<Database>, WifiError>,
    pub db_path: PathBuf,
}

impl AppState {
    pub fn new(db_path: PathBuf) -> Self {
        let db = Database::open(&db_path).map(Arc::new);
        if let Err(e) = &db {
            tracing::error!(path = %db_path.display(), error = %e, "database unavailable");
        }
        Self {
            scanner: Scanner::new(Arc::new(fresnel_core::default_registry())),
            db,
            db_path,
        }
    }

    pub fn db(&self) -> Result<Arc<Database>, WifiError> {
        self.db.clone()
    }
}
