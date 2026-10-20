//! Discovery hub and dynamic smart-view commands.

use crate::api::types::{AlbumDto, ArtistDto, GenreDto, Page, SmartFilter, TrackDto};
use crate::api::validate_smart_filter;
use crate::commands::{to_queue_tracks, with_retry};
use crate::error::{AppError, AppResult};
use crate::player::PlayTicket;
use crate::AppState;
use serde::Serialize;
use std::collections::HashSet;
use tauri::State;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverRow<T> {
    pub items: Vec<T>,
    pub error: Option<String>,
}

impl<T> DiscoverRow<T> {
    fn from_result(result: AppResult<Vec<T>>) -> Self {
        match result {
            Ok(items) => Self { items, error: None },
            Err(error) => Self {
                items: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecadeDto {
    pub start_year: i32,
    pub label: String,
}

fn decades_from_years(years: impl IntoIterator<Item = i32>) -> Vec<DecadeDto> {
    let mut starts = years
        .into_iter()
        .filter(|year| (1..=9999).contains(year))
        .map(|year| year - year.rem_euclid(10))
        .filter(|year| *year >= 10)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    starts.sort_unstable_by(|a, b| b.cmp(a));
    starts
        .into_iter()
        .map(|start_year| DecadeDto {
            start_year,
            label: format!("{start_year}s"),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarArtistsRow {
    pub seed: Option<ArtistDto>,
    pub items: Vec<ArtistDto>,
    pub error: Option<String>,
}

impl SimilarArtistsRow {
    fn from_result(result: AppResult<(Option<ArtistDto>, Vec<ArtistDto>)>) -> Self {
        match result {
            Ok((seed, items)) => Self {
                seed,
                items,
                error: None,
            },
            Err(error) => Self {
                seed: None,
                items: Vec::new(),
                error: Some(error.to_string()),
            },
        }
    }
}

/// The Discover hub's rows. "Long not heard" is not among them: it ranks the
/// whole play history, which takes a scan, so the page asks for it on its own
/// (`get_long_not_heard_albums`) instead of every row waiting for that scan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverData {
    pub random_albums: DiscoverRow<AlbumDto>,
    pub genres: DiscoverRow<GenreDto>,
    pub instant_mix_seeds: DiscoverRow<TrackDto>,
    pub similar_artists: SimilarArtistsRow,
    pub decades: DiscoverRow<DecadeDto>,
}

pub(crate) fn validate_page(limit: u32) -> AppResult<()> {
    if !(1..=200).contains(&limit) {
        return Err(AppError::Other("limit must be between 1 and 200".into()));
    }
    Ok(())
}

pub(crate) fn shuffle_items<T>(items: &mut [T]) {
    for upper in (1..items.len()).rev() {
        let random = uuid::Uuid::new_v4().as_u128();
        let other = (random % (upper as u128 + 1)) as usize;
        items.swap(upper, other);
    }
}

/// Upper bound for a queue built from a query (a smart view, a genre, a
/// decade): a library-wide view matches tens of thousands of tracks, and every
/// queue edit writes and sends the whole list. Five thousand tracks are two
/// weeks of music; shuffled, they are a random five thousand of the matches.
const MAX_QUERY_QUEUE: usize = 5_000;

/// Most albums one Discover play resolves (one request per album).
const MAX_PLAY_ALBUMS: usize = 100;
/// Most tracks one Discover play queues.
const MAX_PLAY_TRACKS: usize = 500;

/// At most `max` of `ids` for one play: the first ones, or a random sample
/// when the result is shuffled anyway. The "see all" pages keep loading more
/// on demand, and refusing a list past the cap made Play and Shuffle fail
/// after a "load more" or two.
fn play_selection(mut ids: Vec<String>, shuffle: bool, max: usize) -> Vec<String> {
    if ids.len() > max {
        if shuffle {
            shuffle_items(&mut ids);
        }
        ids.truncate(max);
    }
    ids
}

async fn start_queue(
    state: &AppState,
    ticket: PlayTicket,
    tracks: Vec<crate::player::QueueTrack>,
) -> AppResult<()> {
    if tracks.is_empty() {
        return Err(AppError::Other("the selection has no tracks".into()));
    }
    state.player.play_queue(ticket, tracks, 0).await
}

#[tauri::command]
pub async fn get_genres(state: State<'_, AppState>) -> AppResult<Vec<GenreDto>> {
    with_retry(&state, |c| async move { c.genres().await }).await
}

/// Names for genre ids the genre list does not have — the ids an older smart
/// view was saved with (see `JellyfinClient::genres_by_id`).
#[tauri::command]
pub async fn get_genres_by_id(
    state: State<'_, AppState>,
    genre_ids: Vec<String>,
) -> AppResult<Vec<GenreDto>> {
    with_retry(&state, |c| {
        let genre_ids = genre_ids.clone();
        async move { c.genres_by_id(&genre_ids).await }
    })
    .await
}

/// Index of the first item whose name sorts at/after letter.
/// kind: "albums" | "artists".
#[tauri::command]
pub async fn letter_index(
    state: State<'_, AppState>,
    kind: String,
    letter: String,
    genre_id: Option<String>,
) -> AppResult<i64> {
    with_retry(&state, |c| {
        let kind = kind.clone();
        let letter = letter.clone();
        let genre_id = genre_id.clone();
        async move {
            c.items_before_letter(&kind, &letter, genre_id.as_deref())
                .await
        }
    })
    .await
}

#[tauri::command]
pub async fn get_similar_artists(
    state: State<'_, AppState>,
    artist_id: String,
    limit: u32,
) -> AppResult<Vec<ArtistDto>> {
    validate_page(limit)?;
    with_retry(&state, |c| {
        let artist_id = artist_id.clone();
        async move { c.similar_artists(&artist_id, limit).await }
    })
    .await
}

/// Load every Discover preview independently. A server/plugin failure in one
/// recommendation endpoint leaves the other rows usable.
#[tauri::command]
pub async fn get_discover(state: State<'_, AppState>) -> AppResult<DiscoverData> {
    const ALBUM_ROW: u32 = 12;
    const CHIP_ROW: u32 = 16;
    const ARTIST_ROW: u32 = 12;

    let random_albums = with_retry(&state, |client| async move {
        Ok(client.discover_random_albums(0, ALBUM_ROW).await?.items)
    });
    let genres = with_retry(&state, |client| async move { client.genres().await });
    let mix_seeds = with_retry(&state, |client| async move {
        Ok(client.discover_mix_seeds(0, ALBUM_ROW).await?.items)
    });
    let similar = with_retry(&state, |client| async move {
        let seed = client.discover_artist_seed().await?;
        let items = match seed.as_ref() {
            Some(seed) => client.similar_artists(&seed.id, ARTIST_ROW).await?,
            None => Vec::new(),
        };
        Ok((seed, items))
    });
    let decades = with_retry(&state, |client| async move {
        Ok(decades_from_years(client.music_years().await?)
            .into_iter()
            .take(CHIP_ROW as usize)
            .collect())
    });

    let (random_albums, genres, mix_seeds, similar, decades) =
        tokio::join!(random_albums, genres, mix_seeds, similar, decades);

    Ok(DiscoverData {
        random_albums: DiscoverRow::from_result(random_albums),
        genres: DiscoverRow::from_result(genres),
        instant_mix_seeds: DiscoverRow::from_result(mix_seeds),
        similar_artists: SimilarArtistsRow::from_result(similar),
        decades: DiscoverRow::from_result(decades),
    })
}

#[tauri::command]
pub async fn get_discover_random_albums(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<AlbumDto>> {
    validate_page(limit)?;
    with_retry(&state, |client| async move {
        client.discover_random_albums(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn get_discover_mix_seeds(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<TrackDto>> {
    validate_page(limit)?;
    with_retry(&state, |client| async move {
        client.discover_mix_seeds(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn get_discover_similar_artists(
    state: State<'_, AppState>,
    artist_id: Option<String>,
    limit: u32,
) -> AppResult<SimilarArtistsRow> {
    validate_page(limit)?;
    let result = with_retry(&state, |client| {
        let artist_id = artist_id.clone();
        async move {
            let seed = match artist_id {
                Some(id) if !id.trim().is_empty() => Some(client.artist(&id).await?),
                Some(_) => return Err(AppError::Other("artist id must not be empty".into())),
                None => client.discover_artist_seed().await?,
            };
            let items = match seed.as_ref() {
                Some(seed) => client.similar_artists(&seed.id, limit).await?,
                None => Vec::new(),
            };
            Ok((seed, items))
        }
    })
    .await;
    Ok(SimilarArtistsRow::from_result(result))
}

#[tauri::command]
pub async fn get_discover_decades(state: State<'_, AppState>) -> AppResult<Vec<DecadeDto>> {
    with_retry(&state, |client| async move {
        Ok(decades_from_years(client.music_years().await?))
    })
    .await
}

#[tauri::command]
pub async fn get_discover_decade_albums(
    state: State<'_, AppState>,
    start_year: i32,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<AlbumDto>> {
    validate_page(limit)?;
    with_retry(&state, |client| async move {
        client.decade_albums(start_year, start_index, limit).await
    })
    .await
}

/// `reuse_recent`: a ranking from the last few minutes will do for a first
/// page too (the Discover row); otherwise a first page ranks afresh.
#[tauri::command]
pub async fn get_long_not_heard_albums(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
    reuse_recent: Option<bool>,
) -> AppResult<Page<AlbumDto>> {
    validate_page(limit)?;
    let reuse_recent = reuse_recent.unwrap_or(false);
    with_retry(&state, |client| async move {
        client
            .long_not_heard_albums(start_index, limit, reuse_recent)
            .await
    })
    .await
}

#[tauri::command]
pub async fn play_discover_albums(
    state: State<'_, AppState>,
    album_ids: Vec<String>,
    shuffle: bool,
) -> AppResult<()> {
    if album_ids.is_empty() {
        return Err(AppError::Other("albumIds must not be empty".into()));
    }
    let ticket = state.player.play_ticket();
    let album_ids = play_selection(album_ids, shuffle, MAX_PLAY_ALBUMS);
    let queue = with_retry(&state, |client| {
        let album_ids = album_ids.clone();
        async move {
            let mut tracks = client.tracks_for_albums(&album_ids).await?;
            if shuffle {
                shuffle_items(&mut tracks);
            }
            to_queue_tracks(&client, tracks)
        }
    })
    .await?;
    start_queue(&state, ticket, queue).await
}

#[tauri::command]
pub async fn play_discover_tracks(
    state: State<'_, AppState>,
    track_ids: Vec<String>,
    shuffle: bool,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Err(AppError::Other("trackIds must not be empty".into()));
    }
    let ticket = state.player.play_ticket();
    let track_ids = play_selection(track_ids, shuffle, MAX_PLAY_TRACKS);
    let queue = with_retry(&state, |client| {
        let track_ids = track_ids.clone();
        async move {
            let mut tracks = client.tracks_by_ids(&track_ids).await?;
            if shuffle {
                shuffle_items(&mut tracks);
            }
            to_queue_tracks(&client, tracks)
        }
    })
    .await?;
    start_queue(&state, ticket, queue).await
}

#[tauri::command]
pub async fn play_discover_genre(
    state: State<'_, AppState>,
    genre_id: String,
    shuffle: bool,
) -> AppResult<()> {
    let filter = SmartFilter {
        genre_ids: vec![genre_id],
        ..SmartFilter::default()
    };
    play_smart_filter(&state, filter, shuffle).await
}

#[tauri::command]
pub async fn play_discover_decade(
    state: State<'_, AppState>,
    start_year: i32,
    shuffle: bool,
) -> AppResult<()> {
    let filter = SmartFilter {
        year_from: Some(start_year),
        year_to: start_year.checked_add(9),
        ..SmartFilter::default()
    };
    play_smart_filter(&state, filter, shuffle).await
}

#[tauri::command]
pub async fn query_smart_view(
    state: State<'_, AppState>,
    filter: SmartFilter,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<TrackDto>> {
    validate_page(limit)?;
    validate_smart_filter(&filter)?;
    with_retry(&state, |client| {
        let filter = filter.clone();
        async move { client.smart_tracks_page(&filter, start_index, limit).await }
    })
    .await
}

async fn play_smart_filter(
    state: &AppState,
    filter: SmartFilter,
    should_shuffle: bool,
) -> AppResult<()> {
    validate_smart_filter(&filter)?;
    let ticket = state.player.play_ticket();
    let queue = with_retry(state, |client| {
        let filter = filter.clone();
        async move {
            let mut tracks = client.smart_tracks(&filter).await?;
            if should_shuffle {
                shuffle_items(&mut tracks);
            }
            tracks.truncate(MAX_QUERY_QUEUE);
            to_queue_tracks(&client, tracks)
        }
    })
    .await?;
    start_queue(state, ticket, queue).await
}

#[tauri::command]
pub async fn play_smart_view(
    state: State<'_, AppState>,
    filter: SmartFilter,
    shuffle: bool,
) -> AppResult<()> {
    play_smart_filter(&state, filter, shuffle).await
}

#[tauri::command]
pub async fn materialize_smart_view(
    state: State<'_, AppState>,
    name: String,
    filter: SmartFilter,
) -> AppResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name must not be empty".into()));
    }
    validate_smart_filter(&filter)?;
    with_retry(&state, |client| {
        let name = name.clone();
        let filter = filter.clone();
        async move {
            let track_ids = client
                .smart_tracks(&filter)
                .await?
                .into_iter()
                .map(|track| track.id)
                .collect::<Vec<_>>();
            if track_ids.is_empty() {
                return Err(AppError::Other(
                    "the smart view has no tracks to materialize".into(),
                ));
            }
            client.create_playlist(&name, &track_ids).await
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decades_are_unique_newest_first() {
        assert_eq!(
            decades_from_years([1999, 2000, 2007, 2007, 2024, 0, 10_000]),
            vec![
                DecadeDto {
                    start_year: 2020,
                    label: "2020s".into(),
                },
                DecadeDto {
                    start_year: 2000,
                    label: "2000s".into(),
                },
                DecadeDto {
                    start_year: 1990,
                    label: "1990s".into(),
                },
            ]
        );
    }

    #[test]
    fn a_long_play_list_is_capped_instead_of_refused() {
        let ids = (0..120).map(|n| n.to_string()).collect::<Vec<_>>();
        // Play keeps the page's order: the first ones.
        assert_eq!(play_selection(ids.clone(), false, 100), ids[..100].to_vec());
        // Shuffle draws from all of them, each at most once.
        let sample = play_selection(ids.clone(), true, 100);
        assert_eq!(sample.len(), 100);
        assert_eq!(sample.iter().collect::<HashSet<_>>().len(), 100);
        assert!(sample.iter().all(|id| ids.contains(id)));
        // Within the cap nothing changes.
        assert_eq!(
            play_selection(ids[..5].to_vec(), true, 100),
            ids[..5].to_vec()
        );
    }

    #[test]
    fn pagination_limits_are_validated() {
        assert!(validate_page(1).is_ok());
        assert!(validate_page(200).is_ok());
        assert_eq!(
            validate_page(0).unwrap_err().to_string(),
            "limit must be between 1 and 200"
        );
        assert!(validate_page(201).is_err());
    }
}
