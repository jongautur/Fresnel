//! Environment clean-up that must happen before GTK/WebKit start (and
//! before any other thread exists, since it changes the process
//! environment). Linux only; elsewhere [`apply`] does nothing.
//!
//! * Snap leakage: apps started from a snap (typically VS Code's integrated
//!   terminal) inherit variables such as `GTK_PATH` or `GIO_MODULE_DIR`
//!   pointing into `/snap`, then load the snap's libraries and crash
//!   (`symbol lookup error: /snap/core20/...`). Same logic as
//!   `scripts/clean-snap-env.sh`, but only variables that point into a snap
//!   or are snap-specific are touched.
//! * NVIDIA: WebKitGTK's DMA-BUF renderer shows a blank window with the
//!   proprietary driver; disable it unless the user chose otherwise.

use std::sync::OnceLock;

/// What [`apply`] changed, for the log and the diagnostics report.
static CHANGES: OnceLock<Vec<String>> = OnceLock::new();

/// Fix the environment for this process. Call first thing in `main()`.
pub fn apply() {
    let changes = platform::apply();
    let _ = CHANGES.set(changes);
}

/// Human-readable list of the changes [`apply`] made.
pub fn changes() -> &'static [String] {
    CHANGES.get().map(Vec::as_slice).unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
mod platform {
    pub fn apply() -> Vec<String> {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;

    use super::linux::*;

    pub fn apply() -> Vec<String> {
        let mut changes = Vec::new();

        let exe_in_snap = std::env::current_exe()
            .map(|p| p.starts_with("/snap"))
            .unwrap_or(false);
        // A snap-packaged Fresnel needs its own snap environment.
        if !exe_in_snap {
            let vars: Vec<(String, String)> = std::env::vars_os()
                .filter_map(|(k, v)| {
                    Some((k.into_string().ok()?, v.to_string_lossy().into_owned()))
                })
                .collect();
            for action in snap_cleanup(&vars) {
                match &action {
                    EnvAction::Unset(name) => std::env::remove_var(name),
                    EnvAction::Set(name, value) => std::env::set_var(name, value),
                }
                changes.push(action.describe());
            }
        }

        let user_choice = std::env::var_os(DMABUF_VAR).is_some();
        if needs_dmabuf_workaround(Path::new("/sys/module/nvidia").exists(), user_choice) {
            std::env::set_var(DMABUF_VAR, "1");
            changes.push(format!("set {DMABUF_VAR}=1 (NVIDIA driver loaded)"));
        }
        changes
    }
}

/// The pure part, kept separate so it can be tested without touching the
/// test process's environment.
#[cfg(target_os = "linux")]
mod linux {
    pub const DMABUF_VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

    /// Variables the VS Code snap (and snaps in general) point into `/snap`.
    const SNAP_PATH_VARS: &[&str] = &[
        "GTK_PATH",
        "GTK_EXE_PREFIX",
        "GTK_IM_MODULE_FILE",
        "GIO_MODULE_DIR",
        "GDK_PIXBUF_MODULE_FILE",
        "GDK_PIXBUF_MODULEDIR",
        "LOCPATH",
        "GSETTINGS_SCHEMA_DIR",
        "XDG_DATA_HOME",
    ];

    /// VS Code's snap saves the value a variable had before it changed it
    /// as `<NAME>_VSCODE_SNAP_ORIG`.
    const ORIG_SUFFIX: &str = "_VSCODE_SNAP_ORIG";

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum EnvAction {
        Unset(String),
        Set(String, String),
    }

    impl EnvAction {
        pub fn describe(&self) -> String {
            match self {
                Self::Unset(name) => format!("unset {name} (snap leftover)"),
                Self::Set(name, value) => format!("restored {name}={value} (from before the snap)"),
            }
        }
    }

    /// Covers `/snap/...` and `~/snap/...` (snap per-user data).
    fn points_into_snap(value: &str) -> bool {
        value.contains("/snap/")
    }

    pub fn snap_cleanup(vars: &[(String, String)]) -> Vec<EnvAction> {
        let get = |name: &str| {
            vars.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        let mut actions = Vec::new();
        let mut handled: Vec<&str> = Vec::new();

        // Restore the saved originals (the script does this for
        // XDG_DATA_DIRS and XDG_CONFIG_DIRS; the snap saves others the same way).
        for (key, orig) in vars {
            let Some(name) = key.strip_suffix(ORIG_SUFFIX) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let current = get(name);
            if !orig.is_empty() {
                if current != Some(orig.as_str()) {
                    actions.push(EnvAction::Set(name.to_string(), orig.clone()));
                }
                handled.push(name);
            } else if current.is_some_and(points_into_snap) {
                actions.push(EnvAction::Unset(name.to_string()));
                handled.push(name);
            }
            actions.push(EnvAction::Unset(key.clone()));
        }

        for name in SNAP_PATH_VARS {
            if !handled.contains(name) && get(name).is_some_and(points_into_snap) {
                actions.push(EnvAction::Unset(name.to_string()));
            }
        }

        // SNAP, SNAP_NAME, SNAP_REVISION, SNAP_LIBRARY_PATH, ...
        for (key, _) in vars {
            if key == "SNAP" || key.starts_with("SNAP_") {
                actions.push(EnvAction::Unset(key.clone()));
            }
        }
        actions
    }

    pub fn needs_dmabuf_workaround(nvidia_loaded: bool, user_set_it: bool) -> bool {
        nvidia_loaded && !user_set_it
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        }

        fn unset(name: &str) -> EnvAction {
            EnvAction::Unset(name.into())
        }

        #[test]
        fn clean_environment_is_left_alone() {
            let env = vars(&[
                ("HOME", "/home/u"),
                ("PATH", "/usr/bin:/bin"),
                ("XDG_DATA_HOME", "/home/u/.local/share"),
                ("GTK_PATH", "/usr/lib/gtk"),
                ("SNAPSHOT_DIR", "/srv/snapshots"),
            ]);
            assert!(snap_cleanup(&env).is_empty());
        }

        #[test]
        fn vscode_snap_leftovers_are_removed() {
            let env = vars(&[
                (
                    "GTK_PATH",
                    "/snap/code/200/usr/lib/x86_64-linux-gnu/gtk-3.0",
                ),
                (
                    "GIO_MODULE_DIR",
                    "/home/u/snap/code/common/.cache/gio-modules",
                ),
                ("XDG_DATA_HOME", "/home/u/snap/code/200/.local/share"),
                ("LOCPATH", "/usr/lib/locale"),
                (
                    "XDG_DATA_DIRS",
                    "/home/u/snap/code/200/.local/share:/usr/share",
                ),
                (
                    "XDG_DATA_DIRS_VSCODE_SNAP_ORIG",
                    "/usr/local/share:/usr/share",
                ),
                ("GDK_BACKEND_VSCODE_SNAP_ORIG", ""),
                ("SNAP", "/snap/code/200"),
                ("SNAP_REVISION", "200"),
            ]);
            let actions = snap_cleanup(&env);
            assert_eq!(
                actions,
                vec![
                    EnvAction::Set("XDG_DATA_DIRS".into(), "/usr/local/share:/usr/share".into()),
                    unset("XDG_DATA_DIRS_VSCODE_SNAP_ORIG"),
                    unset("GDK_BACKEND_VSCODE_SNAP_ORIG"),
                    unset("GTK_PATH"),
                    unset("GIO_MODULE_DIR"),
                    unset("XDG_DATA_HOME"),
                    unset("SNAP"),
                    unset("SNAP_REVISION"),
                ]
            );
        }

        #[test]
        fn empty_original_unsets_only_snap_values() {
            let env = vars(&[
                ("XDG_CONFIG_DIRS", "/snap/code/200/etc/xdg"),
                ("XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG", ""),
                ("GDK_BACKEND", "wayland"),
                ("GDK_BACKEND_VSCODE_SNAP_ORIG", ""),
            ]);
            assert_eq!(
                snap_cleanup(&env),
                vec![
                    unset("XDG_CONFIG_DIRS"),
                    unset("XDG_CONFIG_DIRS_VSCODE_SNAP_ORIG"),
                    unset("GDK_BACKEND_VSCODE_SNAP_ORIG"),
                ]
            );
        }

        #[test]
        fn dmabuf_only_on_nvidia_and_never_over_the_user() {
            assert!(needs_dmabuf_workaround(true, false));
            assert!(!needs_dmabuf_workaround(true, true));
            assert!(!needs_dmabuf_workaround(false, false));
        }
    }
}
