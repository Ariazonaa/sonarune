//! Server-side item metadata editing (admin-only, whitelisted round-trip).
//!
//! The write always re-fetches the full item inside the retry closure, applies
//! only the whitelisted keys, and POSTs the whole object back — so unknown
//! fields and any concurrent change to a different field are never dropped
//! (a partial POST would 400 and can corrupt the item until a library rescan).
//! The server enforces admin-only (`RequiresElevation`); the UI additionally
//! gates every entry point on `can_edit`.

use crate::api::types::{apply_metadata_edits, EditableMetadata, MetadataEdits};
use crate::commands::with_retry;
use crate::error::AppResult;
use crate::AppState;
use tauri::State;

/// Typed projection of the current server values, for the editor form + diff.
#[tauri::command]
pub async fn get_item_metadata(
    state: State<'_, AppState>,
    item_id: String,
) -> AppResult<EditableMetadata> {
    with_retry(&state, |c| {
        let item_id = item_id.clone();
        async move { Ok(EditableMetadata::from_raw(&c.get_item_raw(&item_id).await?)) }
    })
    .await
}

/// Fetch the full item, apply only the whitelisted edits, and POST it back —
/// all in one retry scope so the write is built on the freshest server state.
#[tauri::command]
pub async fn update_item_metadata(
    state: State<'_, AppState>,
    item_id: String,
    edits: MetadataEdits,
) -> AppResult<()> {
    with_retry(&state, |c| {
        let item_id = item_id.clone();
        let edits = edits.clone();
        async move {
            let mut item = c.get_item_raw(&item_id).await?;
            apply_metadata_edits(&mut item, &edits)?;
            c.update_item(&item_id, item).await
        }
    })
    .await
}
