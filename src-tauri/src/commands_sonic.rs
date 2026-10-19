//! Sonic similarity commands: the library scan, a radio around one track and
//! a path from one track to another (`sonic.rs`).

use crate::api::types::TrackDto;
use crate::commands::{to_queue_tracks, with_retry};
use crate::error::{AppError, AppResult};
use crate::player::PlayerCommand;
use crate::sonic::SonicIndex;
use crate::sonic_scan::ScanStatus;
use crate::AppState;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::State;

/// Length of a sonic radio, the seed included.
const RADIO_TRACKS: usize = 40;
/// Bounds for the steps between the two ends of a path.
const PATH_STEPS: std::ops::RangeInclusive<u32> = 2..=40;

/// Marker the UI recognises: the track has no sound vector yet.
const NOT_ANALYSED: &str = "sonic:not-analysed";

#[tauri::command]
pub fn sonic_status(state: State<'_, AppState>) -> ScanStatus {
    state.sonic_scan.status()
}

#[tauri::command]
pub fn sonic_set_scan(state: State<'_, AppState>, enabled: bool) {
    state.sonic_scan.set_enabled(&state.store, enabled);
}

async fn index(state: &AppState) -> AppResult<Arc<SonicIndex>> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || crate::sonic::index(&store))
        .await
        .map_err(|e| AppError::Other(format!("sonic index: {e}")))
}

/// Fetch the tracks behind `ids` and queue them in exactly that order (the
/// order is the point), dropping any the server no longer has.
async fn play_in_order(state: &AppState, ids: Vec<String>) -> AppResult<()> {
    let queue = with_retry(state, |c| {
        let ids = ids.clone();
        async move {
            let mut tracks = c.tracks_by_ids(&ids).await?;
            let position = |id: &str| ids.iter().position(|x| x == id).unwrap_or(usize::MAX);
            tracks.sort_by_key(|t| position(&t.id));
            to_queue_tracks(&c, without_copies(tracks))
        }
    })
    .await?;
    if queue.is_empty() {
        return Err(AppError::Other(
            "none of these tracks are on the server".into(),
        ));
    }
    state.player.send(PlayerCommand::PlayQueue {
        tracks: queue,
        start_index: 0,
    })
}

/// The same recording twice in a library (two files, two item ids) sounds
/// the same, so it lands next to itself. Keep the first and the last track
/// (the seed, a path's destination) and drop later copies of anything
/// already in the list, by artist and title.
pub(crate) fn without_copies(tracks: Vec<TrackDto>) -> Vec<TrackDto> {
    let key = |t: &TrackDto| (t.artist.trim().to_lowercase(), t.name.trim().to_lowercase());
    let mut seen: HashSet<(String, String)> = HashSet::new();
    // Both ends are kept, so copies of either count as seen from the start.
    for end in tracks.first().into_iter().chain(tracks.last()) {
        seen.insert(key(end));
    }
    let count = tracks.len();
    tracks
        .into_iter()
        .enumerate()
        .filter(|(i, t)| *i == 0 || *i + 1 == count || seen.insert(key(t)))
        .map(|(_, t)| t)
        .collect()
}

/// Play `item_id`, then the tracks that sound most like it, in an order that
/// flows.
#[tauri::command]
pub async fn sonic_radio(state: State<'_, AppState>, item_id: String) -> AppResult<()> {
    let index = index(&state).await?;
    if !index.contains(&item_id) {
        return Err(AppError::Other(NOT_ANALYSED.into()));
    }
    let mut ids = vec![item_id.clone()];
    ids.extend(index.radio(&item_id, RADIO_TRACKS - 1, &HashSet::new()));
    play_in_order(&state, ids).await
}

/// Play a path from `from_id` to `to_id` with `steps` tracks in between.
#[tauri::command]
pub async fn sonic_path(
    state: State<'_, AppState>,
    from_id: String,
    to_id: String,
    steps: u32,
) -> AppResult<()> {
    if from_id == to_id {
        return Err(AppError::Other("a path needs two different tracks".into()));
    }
    let steps = steps.clamp(*PATH_STEPS.start(), *PATH_STEPS.end()) as usize;
    let index = index(&state).await?;
    let middle = index
        .path(&from_id, &to_id, steps, &HashSet::new())
        .ok_or_else(|| AppError::Other(NOT_ANALYSED.into()))?;
    let mut ids = vec![from_id];
    ids.extend(middle);
    ids.push(to_id);
    play_in_order(&state, ids).await
}

#[cfg(test)]
mod tests {
    use super::without_copies;
    use crate::api::types::TrackDto;

    fn track(id: &str, artist: &str, name: &str) -> TrackDto {
        TrackDto {
            id: id.into(),
            artist: artist.into(),
            name: name.into(),
            ..TrackDto::default()
        }
    }

    #[test]
    fn copies_of_a_recording_are_dropped_but_both_ends_stay() {
        let tracks = vec![
            track("seed", "A", "One"),
            track("x", "a", "one "),
            track("y", "B", "Two"),
            track("z", "B", "two"),
            track("w", "C", "End"),
            track("end", "C", "End"),
        ];
        let ids: Vec<String> = without_copies(tracks).into_iter().map(|t| t.id).collect();
        // The first copy of the seed is still a different file of it: gone.
        assert_eq!(ids, ["seed", "y", "end"]);
    }
}
