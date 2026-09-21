//! Discovery hub and dynamic smart-view commands.

use crate::api::types::{AlbumDto, ArtistDto, GenreDto, Page, SmartFilter, TrackDto};
use crate::api::validate_smart_filter;
use crate::commands::{to_queue_tracks, with_retry};
use crate::error::{AppError, AppResult};
use crate::player::PlayerCommand;
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverData {
    pub random_albums: DiscoverRow<AlbumDto>,
    pub genres: DiscoverRow<GenreDto>,
    pub instant_mix_seeds: DiscoverRow<TrackDto>,
    pub similar_artists: SimilarArtistsRow,
    pub decades: DiscoverRow<DecadeDto>,
    pub long_not_heard: DiscoverRow<AlbumDto>,
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

fn start_queue(state: &AppState, tracks: Vec<crate::player::QueueTrack>) -> AppResult<()> {
    if tracks.is_empty() {
        return Err(AppError::Other("the selection has no tracks".into()));
    }
    state.player.send(PlayerCommand::PlayQueue {
        tracks,
        start_index: 0,
    })
}

#[tauri::command]
pub async fn get_genres(state: State<'_, AppState>) -> AppResult<Vec<GenreDto>> {
    with_retry(&state, |c| async move { c.genres().await }).await
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
    let long_not_heard = with_retry(&state, |client| async move {
        Ok(client.long_not_heard_albums(0, ALBUM_ROW).await?.items)
    });

    let (random_albums, genres, mix_seeds, similar, decades, long_not_heard) = tokio::join!(
        random_albums,
        genres,
        mix_seeds,
        similar,
        decades,
        long_not_heard
    );

    Ok(DiscoverData {
        random_albums: DiscoverRow::from_result(random_albums),
        genres: DiscoverRow::from_result(genres),
        instant_mix_seeds: DiscoverRow::from_result(mix_seeds),
        similar_artists: SimilarArtistsRow::from_result(similar),
        decades: DiscoverRow::from_result(decades),
        long_not_heard: DiscoverRow::from_result(long_not_heard),
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

#[tauri::command]
pub async fn get_long_not_heard_albums(
    state: State<'_, AppState>,
    start_index: u32,
    limit: u32,
) -> AppResult<Page<AlbumDto>> {
    validate_page(limit)?;
    with_retry(&state, |client| async move {
        client.long_not_heard_albums(start_index, limit).await
    })
    .await
}

#[tauri::command]
pub async fn play_discover_albums(
    state: State<'_, AppState>,
    album_ids: Vec<String>,
    shuffle: bool,
) -> AppResult<()> {
    if album_ids.is_empty() || album_ids.len() > 100 {
        return Err(AppError::Other(
            "albumIds must contain between 1 and 100 ids".into(),
        ));
    }
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
    start_queue(&state, queue)
}

#[tauri::command]
pub async fn play_discover_tracks(
    state: State<'_, AppState>,
    track_ids: Vec<String>,
    shuffle: bool,
) -> AppResult<()> {
    if track_ids.is_empty() || track_ids.len() > 500 {
        return Err(AppError::Other(
            "trackIds must contain between 1 and 500 ids".into(),
        ));
    }
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
    start_queue(&state, queue)
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
    let queue = with_retry(state, |client| {
        let filter = filter.clone();
        async move {
            let mut tracks = client.smart_tracks(&filter).await?;
            if should_shuffle {
                shuffle_items(&mut tracks);
            }
            to_queue_tracks(&client, tracks)
        }
    })
    .await?;
    start_queue(state, queue)
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
