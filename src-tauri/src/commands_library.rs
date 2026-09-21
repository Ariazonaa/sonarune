//! Library-slice commands: playlists, favorites, home page rows.

use crate::api::types::{
    AlbumDto, ArtistDto, CollectionDto, Page, PlaylistDto, PlaylistTrackDto, TrackDto,
};
use crate::api::MAX_IDS_PER_REQUEST;
use crate::commands::{to_queue_tracks, with_retry};
use crate::commands_discover::{shuffle_items, validate_page};
use crate::error::{AppError, AppResult};
use crate::player::PlayerCommand;
use crate::AppState;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDetail {
    pub collection: CollectionDto,
    pub albums: Vec<AlbumDto>,
}

#[tauri::command]
pub async fn get_collections(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<CollectionDto>> {
    validate_page(limit)?;
    with_retry(&state, |client| async move {
        client.collections(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn get_collection(
    state: State<'_, AppState>,
    collection_id: String,
) -> AppResult<CollectionDetail> {
    with_retry(&state, |client| {
        let collection_id = collection_id.clone();
        async move {
            let (mut collection, albums) = tokio::try_join!(
                client.collection(&collection_id),
                client.collection_albums(&collection_id)
            )?;
            // ChildCount can include non-music members. The detail count is
            // the exact read-only album membership Jellysic presents.
            collection.album_count = albums.len() as i32;
            Ok(CollectionDetail { collection, albums })
        }
    })
    .await
}

#[tauri::command]
pub async fn play_collection(
    state: State<'_, AppState>,
    collection_id: String,
    shuffle: bool,
) -> AppResult<()> {
    let queue = with_retry(&state, |client| {
        let collection_id = collection_id.clone();
        async move {
            let mut tracks = client.collection_tracks(&collection_id).await?;
            if shuffle {
                shuffle_items(&mut tracks);
            }
            to_queue_tracks(&client, tracks)
        }
    })
    .await?;
    if queue.is_empty() {
        return Err(AppError::Other("collection has no music tracks".into()));
    }
    state.player.send(PlayerCommand::PlayQueue {
        tracks: queue,
        start_index: 0,
    })
}

#[tauri::command]
pub async fn enqueue_collection(
    state: State<'_, AppState>,
    collection_id: String,
    next: bool,
    shuffle: bool,
) -> AppResult<()> {
    let queue = with_retry(&state, |client| {
        let collection_id = collection_id.clone();
        async move {
            let mut tracks = client.collection_tracks(&collection_id).await?;
            if shuffle {
                shuffle_items(&mut tracks);
            }
            to_queue_tracks(&client, tracks)
        }
    })
    .await?;
    if queue.is_empty() {
        return Err(AppError::Other("collection has no music tracks".into()));
    }
    state.player.send(if next {
        PlayerCommand::PlayNext(queue)
    } else {
        PlayerCommand::PlayLast(queue)
    })
}

#[tauri::command]
pub async fn get_playlists(state: State<'_, AppState>) -> AppResult<Vec<PlaylistDto>> {
    with_retry(&state, |c| async move { c.playlists().await }).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist: PlaylistDto,
    pub tracks: Vec<PlaylistTrackDto>,
}

#[tauri::command]
pub async fn get_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
) -> AppResult<PlaylistDetail> {
    with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        async move {
            let (playlist, tracks) =
                tokio::try_join!(c.playlist(&playlist_id), c.playlist_tracks(&playlist_id))?;
            Ok(PlaylistDetail { playlist, tracks })
        }
    })
    .await
}

/// Returns the id of the newly created playlist.
#[tauri::command]
pub async fn create_playlist(
    state: State<'_, AppState>,
    name: String,
    track_ids: Vec<String>,
) -> AppResult<String> {
    with_retry(&state, |c| {
        let name = name.clone();
        let track_ids = track_ids.clone();
        async move { c.create_playlist(&name, &track_ids).await }
    })
    .await
}

/// Duplicate the current contents of a playlist and return the new playlist id.
#[tauri::command]
pub async fn duplicate_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
    name: String,
) -> AppResult<String> {
    with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        let name = name.clone();
        async move { c.duplicate_playlist(&playlist_id, &name).await }
    })
    .await
}

#[tauri::command]
pub async fn delete_playlist(state: State<'_, AppState>, playlist_id: String) -> AppResult<()> {
    with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        async move { c.delete_playlist(&playlist_id).await }
    })
    .await
}

#[tauri::command]
pub async fn rename_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
    name: String,
) -> AppResult<()> {
    with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        let name = name.clone();
        async move { c.rename_playlist(&playlist_id, &name).await }
    })
    .await
}

/// Add in request-sized chunks, in order, each chunk its own retry scope:
/// adding is not idempotent, and retrying the whole list after a re-login
/// would re-send the chunks the server had already applied.
async fn add_to_playlist(
    state: &AppState,
    playlist_id: &str,
    item_ids: &[String],
) -> AppResult<()> {
    for chunk in item_ids.chunks(MAX_IDS_PER_REQUEST) {
        with_retry(state, |c| {
            let playlist_id = playlist_id.to_string();
            let chunk = chunk.to_vec();
            async move { c.playlist_add_chunk(&playlist_id, &chunk).await }
        })
        .await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn playlist_add(
    state: State<'_, AppState>,
    playlist_id: String,
    track_ids: Vec<String>,
) -> AppResult<()> {
    add_to_playlist(&state, &playlist_id, &track_ids).await
}

/// `entry_ids` are playlist entry ids (PlaylistItemId), not item ids.
#[tauri::command]
pub async fn playlist_remove(
    state: State<'_, AppState>,
    playlist_id: String,
    entry_ids: Vec<String>,
) -> AppResult<()> {
    with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        let entry_ids = entry_ids.clone();
        async move { c.playlist_remove(&playlist_id, &entry_ids).await }
    })
    .await
}

#[tauri::command]
pub async fn playlist_move(
    state: State<'_, AppState>,
    playlist_id: String,
    entry_id: String,
    new_index: usize,
) -> AppResult<()> {
    with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        let entry_id = entry_id.clone();
        async move { c.playlist_move(&playlist_id, &entry_id, new_index).await }
    })
    .await
}

/// Copy or move exact entries between two playlists. Resolution is performed
/// from a fresh source snapshot. Adding and removing are separate retry scopes
/// so a re-login during removal cannot add the selected tracks a second time.
#[tauri::command]
pub async fn transfer_playlist_entries(
    state: State<'_, AppState>,
    source_playlist_id: String,
    target_playlist_id: String,
    entry_ids: Vec<String>,
    move_entries: bool,
) -> AppResult<()> {
    if source_playlist_id.trim().is_empty() {
        return Err(AppError::Other(
            "source playlist id must not be empty".into(),
        ));
    }
    if target_playlist_id.trim().is_empty() {
        return Err(AppError::Other(
            "target playlist id must not be empty".into(),
        ));
    }
    if source_playlist_id == target_playlist_id {
        return Err(AppError::Other(
            "source and target playlists must differ".into(),
        ));
    }

    let item_ids = with_retry(&state, |c| {
        let source_playlist_id = source_playlist_id.clone();
        let entry_ids = entry_ids.clone();
        async move {
            c.playlist_item_ids_for_entries(&source_playlist_id, &entry_ids)
                .await
        }
    })
    .await?;

    add_to_playlist(&state, &target_playlist_id, &item_ids).await?;

    if move_entries {
        with_retry(&state, |c| {
            let source_playlist_id = source_playlist_id.clone();
            let entry_ids = entry_ids.clone();
            async move { c.playlist_remove(&source_playlist_id, &entry_ids).await }
        })
        .await?;
    }

    Ok(())
}

/// Replace the queue with the playlist's tracks, starting at `start_index`.
#[tauri::command]
pub async fn play_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
    start_index: usize,
) -> AppResult<()> {
    let mut queue = with_retry(&state, |c| {
        let playlist_id = playlist_id.clone();
        async move {
            let tracks = c
                .playlist_tracks(&playlist_id)
                .await?
                .into_iter()
                .map(|entry| entry.track)
                .collect();
            to_queue_tracks(&c, tracks)
        }
    })
    .await?;
    for track in &mut queue {
        track.source_playlist_id = Some(playlist_id.clone());
    }
    if queue.is_empty() {
        return Err(AppError::Other("playlist has no tracks".into()));
    }
    state.player.send(PlayerCommand::PlayQueue {
        tracks: queue,
        start_index,
    })
}

/// Permanently delete items (tracks or whole albums) on the server. The files
/// are removed from disk — this is irreversible. `with_retry` renews an expired
/// token; a permission failure (403) surfaces to the UI as an error.
#[tauri::command]
pub async fn delete_items(state: State<'_, AppState>, item_ids: Vec<String>) -> AppResult<()> {
    with_retry(&state, |c| {
        let item_ids = item_ids.clone();
        async move { c.delete_items(&item_ids).await }
    })
    .await
}

#[tauri::command]
pub async fn set_favorite(
    state: State<'_, AppState>,
    item_id: String,
    favorite: bool,
) -> AppResult<()> {
    with_retry(&state, |c| {
        let item_id = item_id.clone();
        async move { c.set_favorite(&item_id, favorite).await }
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeData {
    pub recently_played: Vec<AlbumDto>,
    pub recently_added: Vec<AlbumDto>,
    pub most_played: Vec<AlbumDto>,
    pub forgotten_favorites: Vec<AlbumDto>,
}

#[tauri::command]
pub async fn get_home(state: State<'_, AppState>) -> AppResult<HomeData> {
    const ROW_LEN: u32 = 12;
    with_retry(&state, |c| async move {
        let (recently_played, recently_added, most_played, forgotten_favorites) = tokio::try_join!(
            c.home_albums("DatePlayed", ROW_LEN),
            c.home_albums("DateCreated", ROW_LEN),
            c.home_albums("PlayCount", ROW_LEN),
            c.forgotten_favorite_albums(ROW_LEN),
        )?;
        Ok(HomeData {
            recently_played,
            recently_added,
            most_played,
            forgotten_favorites,
        })
    })
    .await
}

/// Listening stats, from the only per-user data Jellyfin exposes: each item's
/// last `DatePlayed` and `PlayCount`. There is no cumulative listening time
/// or per-play history to surface.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsData {
    pub recently_played: Vec<TrackDto>,
    pub most_played_tracks: Vec<TrackDto>,
    pub most_played_albums: Vec<AlbumDto>,
}

/// Size of each favorites section's first page. The page loads the rest on
/// demand through `get_favorite_{tracks,albums,artists}`.
const FAVORITES_FIRST_PAGE: u32 = 50;

/// The first page of every favorites section, each with its total.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoritesData {
    pub tracks: Page<TrackDto>,
    pub albums: Page<AlbumDto>,
    pub artists: Page<ArtistDto>,
}

#[tauri::command]
pub async fn get_favorites(state: State<'_, AppState>) -> AppResult<FavoritesData> {
    with_retry(&state, |c| async move {
        let (tracks, albums, artists) = tokio::try_join!(
            c.favorite_tracks(0, FAVORITES_FIRST_PAGE),
            c.favorite_albums(0, FAVORITES_FIRST_PAGE),
            c.favorite_artists(0, FAVORITES_FIRST_PAGE),
        )?;
        Ok(FavoritesData {
            tracks,
            albums,
            artists,
        })
    })
    .await
}

#[tauri::command]
pub async fn get_favorite_tracks(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<TrackDto>> {
    validate_page(limit)?;
    with_retry(&state, |c| async move {
        c.favorite_tracks(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn get_favorite_albums(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<AlbumDto>> {
    validate_page(limit)?;
    with_retry(&state, |c| async move {
        c.favorite_albums(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn get_favorite_artists(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<ArtistDto>> {
    validate_page(limit)?;
    with_retry(&state, |c| async move {
        c.favorite_artists(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn get_stats(state: State<'_, AppState>) -> AppResult<StatsData> {
    with_retry(&state, |c| async move {
        let (recently_played, most_played_tracks, most_played_albums) = tokio::try_join!(
            c.recently_played_tracks(40),
            c.most_played_tracks(40),
            c.home_albums("PlayCount", 12),
        )?;
        Ok(StatsData {
            recently_played,
            most_played_tracks,
            most_played_albums,
        })
    })
    .await
}
