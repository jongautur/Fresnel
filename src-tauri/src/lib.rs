mod commands;
mod state;

use tauri::Manager;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

fn init_logging() {
    // Override with e.g. RUST_LOG=fresnel_core=debug
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,fresnel_core=info,fresnel_lib=info,zbus=warn")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

pub fn run() {
    init_logging();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting Fresnel"
    );

    let result = tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(AppState::new(data_dir.join("fresnel.db")));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_info,
            commands::adapters::list_adapters,
            commands::adapters::get_adapter,
            commands::wifi::scan,
            commands::wifi::last_scan,
            commands::wifi::get_current_connection,
            commands::projects::list_projects,
            commands::projects::create_project,
            commands::projects::delete_project,
        ])
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "application error");
        std::process::exit(1);
    }
}
