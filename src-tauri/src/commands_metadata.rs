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
use crate::error::{AppError, AppResult};
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
/// which half. Unless the server cannot be reached at all: then every further
/// item would only wait out its own timeout, for minutes on end, so the run
/// stops there and counts the rest as skipped.
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
    let total = item_ids.len();
    for (index, item_id) in item_ids.into_iter().enumerate() {
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
                if server_unreachable(&e) {
                    // The reason the run stopped is the one worth showing.
                    result.error = Some(e.to_string());
                    result.skipped = total - index - 1;
                    break;
                }
                result.error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    tracing::info!(
        "bulk metadata write done: {} changed, {} failed, {} skipped",
        result.changed,
        result.failed,
        result.skipped
    );
    Ok(result)
}

/// A failure that is about getting to the server at all, not about the item:
/// the request never got an answer, or there is no session to send it with.
fn server_unreachable(err: &AppError) -> bool {
    matches!(err, AppError::Network(_) | AppError::NotConnected)
}

#[cfg(test)]
mod tests {
    use super::server_unreachable;
    use crate::error::AppError;

    #[tokio::test]
    async fn only_a_server_out_of_reach_stops_a_bulk_write() {
        // Nothing listens there: the request never gets an answer.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let client = crate::api::JellyfinClient::new(&url, "test-device", Vec::new())
            .unwrap()
            .with_session("token".into(), "user".into());
        let network = client.get_item_raw("item").await.unwrap_err();
        assert!(matches!(network, AppError::Network(_)), "{network}");
        assert!(server_unreachable(&network));
        assert!(server_unreachable(&AppError::NotConnected));

        // The server answered, about this one item: the next one may work.
        for refused in [
            AppError::Server {
                status: 403,
                message: "not an administrator".into(),
            },
            AppError::Server {
                status: 400,
                message: "bad item".into(),
            },
            AppError::Other("unreadable server response".into()),
        ] {
            assert!(!server_unreachable(&refused), "{refused}");
        }
    }
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
