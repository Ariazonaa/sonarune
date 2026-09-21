//! Server-side item metadata editing (admin-only, whitelisted round-trip).
//!
//! The write always re-fetches the full item inside the retry closure, applies
//! only the whitelisted keys, and POSTs the whole object back — so unknown
//! fields and any concurrent change to a different field are never dropped
//! (a partial POST would 400 and can corrupt the item until a library rescan).
//! The server enforces admin-only (`RequiresElevation`); the UI additionally
//! gates every entry point on `can_edit`.

use crate::api::types::{
    apply_bulk_metadata_edits, apply_metadata_edits, BulkMetadataEdits, BulkMetadataResult,
    EditableMetadata, MetadataEdits,
};
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

/// The same round-trip for many items, with one set of edits.
///
/// One item at a time on purpose: every write is a fetch and a POST of the
/// whole item, and firing fifty of those at a home server at once is a good
/// way to make it refuse the lot. A single failure is counted and the run goes
/// on — the alternative is stopping halfway and leaving the user to guess
/// which half.
#[tauri::command]
pub async fn update_items_metadata(
    state: State<'_, AppState>,
    item_ids: Vec<String>,
    edits: BulkMetadataEdits,
) -> AppResult<BulkMetadataResult> {
    // Logged on the way in as well as out: a save that "did nothing" is
    // otherwise indistinguishable from a click that never reached the backend,
    // and only one of those two is a bug in here.
    tracing::info!("bulk metadata write: {} items", item_ids.len());
    let mut result = BulkMetadataResult::default();
    for item_id in item_ids {
        let outcome = with_retry(&state, |c| {
            let item_id = item_id.clone();
            let edits = edits.clone();
            async move {
                let mut item = c.get_item_raw(&item_id).await?;
                apply_bulk_metadata_edits(&mut item, &edits)?;
                c.update_item(&item_id, item).await
            }
        })
        .await;
        match outcome {
            Ok(()) => result.changed += 1,
            Err(e) => {
                tracing::warn!("bulk metadata write failed for {item_id}: {e}");
                result.failed += 1;
                result.error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    tracing::info!(
        "bulk metadata write done: {} changed, {} failed",
        result.changed,
        result.failed
    );
    Ok(result)
}

/// Fetch the full item, apply only the whitelisted edits, and POST it back —
/// all in one retry scope so the write is built on the freshest server state.
#[tauri::command]
pub async fn update_item_metadata(
    state: State<'_, AppState>,
    item_id: String,
    edits: MetadataEdits,
) -> AppResult<()> {
    let outcome = with_retry(&state, |c| {
        let item_id = item_id.clone();
        let edits = edits.clone();
        async move {
            let mut item = c.get_item_raw(&item_id).await?;
            apply_metadata_edits(&mut item, &edits)?;
            c.update_item(&item_id, item).await
        }
    })
    .await;
    // Same reason as the bulk write: without this, a refused write leaves no
    // trace anywhere but in the dialog the user is looking at.
    match &outcome {
        Ok(()) => tracing::info!("metadata write done for {item_id}"),
        Err(e) => tracing::warn!("metadata write failed for {item_id}: {e}"),
    }
    outcome
}
