mod commands;
mod env_fix;
mod logging;
mod state;

use tauri::Manager;

use crate::state::AppState;

macro_rules! app_commands {
    () => {
        tauri::generate_handler![
            commands::app::app_info,
            commands::diagnostics::diagnostics_report,
            commands::diagnostics::log_frontend_error,
            commands::adapters::list_adapters,
            commands::adapters::get_adapter,
            commands::wifi::scan,
            commands::wifi::last_scan,
            commands::wifi::get_current_connection,
            commands::projects::list_projects,
            commands::projects::create_project,
            commands::projects::delete_project,
            commands::survey::list_buildings,
            commands::survey::create_building,
            commands::survey::delete_building,
            commands::survey::list_floors,
            commands::survey::create_floor,
            commands::survey::delete_floor,
            commands::survey::import_floor_plan,
            commands::survey::floor_plan_image,
            commands::survey::set_floor_scale,
            commands::survey::list_survey_points,
            commands::survey::measure_here,
            commands::survey::delete_survey_point,
            commands::survey::list_building_aps,
            commands::survey::create_placed_ap,
            commands::survey::update_placed_ap,
            commands::survey::delete_placed_ap,
        ]
    };
}

/// Fix up the process environment (see `env_fix`). Call first thing in
/// `main()`, before any other thread exists.
pub fn prepare_environment() {
    env_fix::apply();
}

pub fn run() {
    logging::init();
    logging::install_panic_hook();

    let result = tauri::Builder::default()
        // Must be the first plugin. A second instance would defeat the
        // per-adapter scan lock and spacing and race on plan files, so it
        // hands over to the running one and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tracing::info!("another launch; focusing the existing window");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            logging::attach_file(app.path().app_log_dir().map_err(|e| e.to_string()));
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting Fresnel");
            for change in env_fix::changes() {
                tracing::info!("environment: {change}");
            }
            // Local, not Roaming, on Windows: SQLite with WAL and large plan
            // images don't belong in a profile that is synced at logon and
            // logoff. Linux and macOS resolve both to the same directory.
            let data_dir = app.path().app_local_data_dir()?;
            app.manage(AppState::new(data_dir));
            Ok(())
        })
        .invoke_handler(app_commands!())
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "application error");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    //! The binary IPC paths (raw request body + headers, raw response) can't
    //! be exercised from the browser preview, so test them on the mock runtime.

    use fresnel_core::database::projects::NewProject;
    use fresnel_core::survey::models::{NewBuilding, NewFloor};
    use tauri::http::{HeaderMap, HeaderValue};
    use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
    use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
    use tauri::webview::InvokeRequest;

    use super::*;

    fn request(cmd: &str, body: InvokeBody, headers: HeaderMap) -> InvokeRequest {
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body,
            headers,
            invoke_key: INVOKE_KEY.to_string(),
        }
    }

    #[test]
    fn floor_plan_upload_and_download_over_ipc() {
        let dir = std::env::temp_dir().join(format!("fresnel-ipc-{}", std::process::id()));
        let state = AppState::new(dir.clone());
        let db = state.db().unwrap();
        let project = db
            .create_project(&NewProject {
                name: "P".into(),
                customer_name: None,
            })
            .unwrap();
        let building = db
            .create_building(&NewBuilding {
                project_id: project.id,
                name: "B".into(),
            })
            .unwrap();
        let floor = db
            .create_floor(&NewFloor {
                building_id: building.id,
                name: "F".into(),
                level: 0,
            })
            .unwrap();

        let app = mock_builder()
            .manage(state)
            .invoke_handler(app_commands!())
            .build(mock_context(noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();

        let png = b"\x89PNG\r\n\x1a\n-not-really-a-png-".to_vec();
        let mut headers = HeaderMap::new();
        headers.insert("x-floor-id", HeaderValue::from(floor.id));
        headers.insert("x-plan-width", HeaderValue::from_static("1600"));
        headers.insert("x-plan-height", HeaderValue::from_static("900.5"));

        let floor_json = get_ipc_response(
            &webview,
            request(
                "import_floor_plan",
                InvokeBody::Raw(png.clone()),
                headers.clone(),
            ),
        )
        .unwrap()
        .deserialize::<serde_json::Value>()
        .unwrap();
        assert_eq!(floor_json["plan"]["mime"], "image/png");
        assert_eq!(floor_json["plan"]["height"], 900.5);

        let image = get_ipc_response(
            &webview,
            request(
                "floor_plan_image",
                InvokeBody::Json(serde_json::json!({ "floorId": floor.id })),
                HeaderMap::new(),
            ),
        )
        .unwrap();
        match image {
            InvokeResponseBody::Raw(bytes) => assert_eq!(bytes, png),
            other => panic!("expected raw bytes, got {other:?}"),
        }

        // Missing metadata is a typed error, not a panic.
        headers.remove("x-plan-width");
        let err = get_ipc_response(
            &webview,
            request("import_floor_plan", InvokeBody::Raw(png), headers),
        )
        .unwrap_err();
        assert_eq!(err["kind"], "invalid_input");

        let _ = std::fs::remove_dir_all(dir);
    }
}
