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
            commands::point_tests::get_test_settings,
            commands::point_tests::save_test_settings,
            commands::point_tests::run_point_tests,
            commands::point_tests::cancel_active_test,
            commands::point_tests::list_floor_point_tests,
            commands::survey::list_building_aps,
            commands::survey::create_placed_ap,
            commands::survey::update_placed_ap,
            commands::survey::delete_placed_ap,
            commands::requirements::requirement_presets,
            commands::requirements::list_requirement_profiles,
            commands::requirements::create_requirement_profile,
            commands::requirements::update_requirement_profile,
            commands::requirements::delete_requirement_profile,
            commands::requirements::set_requirement_targets,
            commands::requirements::requirement_target_options,
            commands::requirements::set_floor_requirement_profile,
            commands::requirements::evaluate_floor_requirements,
            commands::notes::get_notes,
            commands::notes::set_notes,
            commands::notes::list_note_pins,
            commands::notes::create_note_pin,
            commands::notes::update_note_pin,
            commands::notes::delete_note_pin,
            commands::notes::import_photo,
            commands::notes::list_floor_photos,
            commands::notes::photo_thumbnail,
            commands::notes::photo_report_image,
            commands::notes::update_photo,
            commands::notes::delete_photo,
            commands::report::floor_annotations,
            commands::report::save_export,
            commands::report::open_export,
            commands::settings::get_branding,
            commands::settings::set_branding,
            commands::settings::branding_logo,
            commands::settings::set_branding_logo,
            commands::settings::clear_branding_logo,
            commands::findings::survey_findings,
            commands::findings::set_bssid_mark,
            commands::findings::clear_bssid_mark,
            commands::findings::bssid_link_options,
            commands::tools::tool_ping,
            commands::tools::tool_traceroute,
            commands::tools::tool_dns,
            commands::tools::tool_port_check,
            commands::tools::tool_iperf3,
            commands::tools::cancel_tool,
            commands::tools::dns_system_servers,
            commands::tools::list_tool_runs,
            commands::tools::get_tool_run,
            commands::tools::delete_tool_run,
            commands::tools::clear_tool_runs,
            commands::tools::attach_tool_run,
            commands::tools::list_floor_tool_runs,
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
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
            // The app's own origin, which is what the ACL allows commands from.
            url: if cfg!(windows) {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
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

    /// Requirement profiles: flattened values and tagged targets survive the
    /// JSON round trip the UI uses.
    #[test]
    fn requirement_profiles_over_ipc() {
        let dir = std::env::temp_dir().join(format!("fresnel-ipc-req-{}", std::process::id()));
        let state = AppState::new(dir.clone());
        let db = state.db().unwrap();
        let project = db
            .create_project(&NewProject {
                name: "P".into(),
                customer_name: None,
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
        let call = |cmd: &str, args: serde_json::Value| {
            get_ipc_response(
                &webview,
                request(cmd, InvokeBody::Json(args), HeaderMap::new()),
            )
            .map(|r| r.deserialize::<serde_json::Value>().unwrap())
        };

        let presets = call("requirement_presets", serde_json::json!({})).unwrap();
        let office = presets
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["preset"] == "office_data")
            .unwrap();
        let mut input = office["values"].clone();
        input["name"] = "Office".into();
        input["preset"] = "office_data".into();
        let profile = call(
            "create_requirement_profile",
            serde_json::json!({ "projectId": project.id, "profile": input }),
        )
        .unwrap();
        assert_eq!(profile["primaryMinDbm"], -67);
        assert_eq!(profile["requiredBands"], serde_json::json!(["5ghz"]));
        assert_eq!(profile["isDefault"], true);

        let profile = call(
            "set_requirement_targets",
            serde_json::json!({
                "profileId": profile["id"],
                "targets": [{ "kind": "ssid", "ssidRaw": [67, 111, 114, 112] }],
            }),
        )
        .unwrap();
        assert_eq!(profile["targets"][0]["label"], "Corp");

        let err = call(
            "set_requirement_targets",
            serde_json::json!({ "profileId": profile["id"], "targets": [{ "kind": "ap", "apId": 99 }] }),
        )
        .unwrap_err();
        assert_eq!(err["kind"], "invalid_input");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn photo_upload_thumbnail_and_delete_over_ipc() {
        let dir = std::env::temp_dir().join(format!("fresnel-ipc-photo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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
        let invoke = |cmd: &str, body: InvokeBody, headers: HeaderMap| {
            get_ipc_response(&webview, request(cmd, body, headers))
        };

        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(640, 480, image::Rgb([200, 100, 50]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-photo-target", HeaderValue::from_static("floor"));
        headers.insert("x-photo-target-id", HeaderValue::from(floor.id));
        let photo = invoke(
            "import_photo",
            InvokeBody::Raw(jpeg.clone()),
            headers.clone(),
        )
        .unwrap()
        .deserialize::<serde_json::Value>()
        .unwrap();
        assert_eq!(
            photo["target"],
            serde_json::json!({ "kind": "floor", "id": floor.id })
        );
        assert_eq!(photo["width"], 640);
        assert_eq!(photo["inReport"], true);
        let id = photo["id"].as_i64().unwrap();

        let photos_dir = dir.join("photos");
        assert_eq!(std::fs::read_dir(&photos_dir).unwrap().count(), 3);
        let original = std::fs::read(photos_dir.join(photo["file"].as_str().unwrap())).unwrap();
        assert_eq!(original, jpeg);

        for cmd in ["photo_thumbnail", "photo_report_image"] {
            let body = invoke(
                cmd,
                InvokeBody::Json(serde_json::json!({ "id": id })),
                HeaderMap::new(),
            )
            .unwrap();
            match body {
                InvokeResponseBody::Raw(bytes) => assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF])),
                other => panic!("expected raw bytes, got {other:?}"),
            }
        }

        let updated = invoke(
            "update_photo",
            InvokeBody::Json(
                serde_json::json!({ "id": id, "caption": "Server room", "inReport": false }),
            ),
            HeaderMap::new(),
        )
        .unwrap()
        .deserialize::<serde_json::Value>()
        .unwrap();
        assert_eq!(updated["caption"], "Server room");

        // HEIC is refused with advice, not a decoder error.
        let err = invoke(
            "import_photo",
            InvokeBody::Raw(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec()),
            headers,
        )
        .unwrap_err();
        assert_eq!(err["kind"], "invalid_input");
        assert!(err["message"].as_str().unwrap().contains("JPEG"));

        // Deleting the row removes all three files.
        invoke(
            "delete_photo",
            InvokeBody::Json(serde_json::json!({ "id": id })),
            HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(std::fs::read_dir(&photos_dir).unwrap().count(), 0);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Branding: names as JSON, the logo as a raw body both ways.
    #[test]
    fn branding_over_ipc() {
        let dir = std::env::temp_dir().join(format!("fresnel-ipc-brand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = mock_builder()
            .manage(AppState::new(dir.clone()))
            .invoke_handler(app_commands!())
            .build(mock_context(noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |cmd: &str, body: InvokeBody| {
            get_ipc_response(&webview, request(cmd, body, HeaderMap::new()))
        };

        let info = invoke(
            "set_branding",
            InvokeBody::Json(serde_json::json!({
                "branding": { "technicianName": " Ada ", "companyName": null }
            })),
        )
        .unwrap()
        .deserialize::<serde_json::Value>()
        .unwrap();
        assert_eq!(info["technicianName"], "Ada");
        assert_eq!(info["logo"], serde_json::Value::Null);

        let mut png = Vec::new();
        image::RgbImage::new(8, 4)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let info = invoke("set_branding_logo", InvokeBody::Raw(png.clone()))
            .unwrap()
            .deserialize::<serde_json::Value>()
            .unwrap();
        assert_eq!(info["logo"]["mime"], "image/png");
        match invoke("branding_logo", InvokeBody::Json(serde_json::json!({}))).unwrap() {
            InvokeResponseBody::Raw(bytes) => assert_eq!(bytes, png),
            other => panic!("expected raw bytes, got {other:?}"),
        }

        let err = invoke(
            "set_branding_logo",
            InvokeBody::Raw(b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec()),
        )
        .unwrap_err();
        assert_eq!(err["kind"], "invalid_input");

        let _ = std::fs::remove_dir_all(dir);
    }
}
