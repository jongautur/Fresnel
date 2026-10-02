// Release builds have no console window on Windows; logs go to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Before Tauri, GTK or any other thread starts.
    fresnel_lib::prepare_environment();
    fresnel_lib::run()
}
