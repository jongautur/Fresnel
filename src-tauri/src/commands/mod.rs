//! Tauri IPC commands. Thin wrappers: validation and orchestration live in
//! `fresnel-core`. Errors serialise as `{ kind, message }`.

pub mod adapters;
pub mod app;
pub mod projects;
pub mod wifi;
