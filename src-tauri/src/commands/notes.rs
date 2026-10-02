//! Notes, note pins and photos.

use fresnel_core::database::notes::{NotePin, NotePinInput, NoteTarget};
use fresnel_core::database::photos::{Photo, PhotoTarget};
use fresnel_core::WifiError;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::State;

use super::with_db;
use crate::state::AppState;

/// Remove photo files no row references any more (after deletes, including
/// cascades from points, APs, pins, floors). Best effort: a failure only
/// leaves orphaned files for the next run.
pub(crate) async fn collect_photo_garbage(state: &AppState) {
    let photos = state.photos.clone();
    if let Err(e) = with_db(state, move |db| {
        photos.collect_garbage(|| db.referenced_photo_files())
    })
    .await
    {
        tracing::warn!(error = %e, "skipping photo clean-up");
    }
}

// --- Notes ----------------------------------------------------------------

#[tauri::command]
pub async fn get_notes(
    state: State<'_, AppState>,
    target: NoteTarget,
) -> Result<Option<String>, WifiError> {
    with_db(&state, move |db| db.get_notes(target)).await
}

/// Returns the stored text (trimmed; `null` when blank).
#[tauri::command]
pub async fn set_notes(
    state: State<'_, AppState>,
    target: NoteTarget,
    text: Option<String>,
) -> Result<Option<String>, WifiError> {
    with_db(&state, move |db| db.set_notes(target, text.as_deref())).await
}

// --- Note pins ------------------------------------------------------------

#[tauri::command]
pub async fn list_note_pins(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<Vec<NotePin>, WifiError> {
    with_db(&state, move |db| db.list_note_pins(floor_id)).await
}

#[tauri::command]
pub async fn create_note_pin(
    state: State<'_, AppState>,
    floor_id: i64,
    pin: NotePinInput,
) -> Result<NotePin, WifiError> {
    with_db(&state, move |db| db.create_note_pin(floor_id, &pin)).await
}

#[tauri::command]
pub async fn update_note_pin(
    state: State<'_, AppState>,
    id: i64,
    pin: NotePinInput,
) -> Result<NotePin, WifiError> {
    with_db(&state, move |db| db.update_note_pin(id, &pin)).await
}

#[tauri::command]
pub async fn delete_note_pin(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    let deleted = with_db(&state, move |db| db.delete_note_pin(id)).await?;
    collect_photo_garbage(&state).await;
    Ok(deleted)
}

// --- Photos ---------------------------------------------------------------

/// Import a photo. The file travels as the raw IPC body; what it is
/// attached to comes in headers: `x-photo-target` (`floor`, `point`, `ap`
/// or `pin`) and `x-photo-target-id`. Decoding and downscaling run on a
/// blocking thread.
#[tauri::command]
pub async fn import_photo(
    state: State<'_, AppState>,
    request: Request<'_>,
) -> Result<Photo, WifiError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(WifiError::InvalidInput(
            "expected the photo as a binary body".into(),
        ));
    };
    let header = |name: &str| -> Result<&str, WifiError> {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| WifiError::InvalidInput(format!("missing {name} header")))
    };
    let id: i64 = header("x-photo-target-id")?
        .parse()
        .map_err(|_| WifiError::InvalidInput("x-photo-target-id is not an id".into()))?;
    let target = match header("x-photo-target")? {
        "floor" => PhotoTarget::Floor(id),
        "point" => PhotoTarget::Point(id),
        "ap" => PhotoTarget::Ap(id),
        "pin" => PhotoTarget::Pin(id),
        other => {
            return Err(WifiError::InvalidInput(format!(
                "unknown photo target '{other}'"
            )))
        }
    };
    let bytes = bytes.clone();

    let photos = state.photos.clone();
    with_db(&state, move |db| {
        photos.import(&bytes, |stored| db.insert_photo(target, stored))
    })
    .await
}

/// Every photo on a floor (the UI picks those of a point, AP or pin).
#[tauri::command]
pub async fn list_floor_photos(
    state: State<'_, AppState>,
    floor_id: i64,
) -> Result<Vec<Photo>, WifiError> {
    with_db(&state, move |db| db.list_floor_photos(floor_id)).await
}

/// A photo's thumbnail (JPEG) as raw bytes.
#[tauri::command]
pub async fn photo_thumbnail(state: State<'_, AppState>, id: i64) -> Result<Response, WifiError> {
    let photos = state.photos.clone();
    let bytes = with_db(&state, move |db| photos.read(&db.get_photo(id)?.thumb_file)).await?;
    Ok(Response::new(bytes))
}

/// A photo's report copy (JPEG, no metadata) as raw bytes. The app never
/// shows the original, which keeps its metadata.
#[tauri::command]
pub async fn photo_report_image(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Response, WifiError> {
    let photos = state.photos.clone();
    let bytes = with_db(&state, move |db| {
        photos.read(&db.get_photo(id)?.report_file)
    })
    .await?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn update_photo(
    state: State<'_, AppState>,
    id: i64,
    caption: Option<String>,
    in_report: bool,
) -> Result<Photo, WifiError> {
    with_db(&state, move |db| {
        db.update_photo(id, caption.as_deref(), in_report)
    })
    .await
}

#[tauri::command]
pub async fn delete_photo(state: State<'_, AppState>, id: i64) -> Result<bool, WifiError> {
    let deleted = with_db(&state, move |db| db.delete_photo(id)).await?;
    collect_photo_garbage(&state).await;
    Ok(deleted)
}
