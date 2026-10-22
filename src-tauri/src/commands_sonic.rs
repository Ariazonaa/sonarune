//! Sonic similarity commands: the library scan, a radio around one track and
//! a path from one track to another (`sonic.rs`).

use crate::api::types::TrackDto;
use crate::commands::{to_queue_tracks, with_retry};
use crate::error::{AppError, AppResult};
use crate::identity::{Owner, Server};
use crate::player::PlayTicket;
use crate::sonic::SonicIndex;
use crate::sonic_scan::ScanStatus;
use crate::AppState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::State;

/// Length of a sonic radio, the seed included.
const RADIO_TRACKS: usize = 40;
/// Bounds for the steps between the two ends of a path.
const PATH_STEPS: std::ops::RangeInclusive<u32> = 2..=40;

/// Marker the UI recognises: the track has no sound vector yet.
const NOT_ANALYSED: &str = "sonic:not-analysed";
/// Marker the UI recognises: the track has been analysed, and it is too
/// short or too quiet to have a sound to compare.
const NO_SOUND: &str = "sonic:no-sound";
/// Marker the UI recognises: too little of the library is analysed to fill a
/// radio or a path. Right after the upgrade, with the scan off, the index
/// holds only what was played since — a "radio" of two tracks would replace
/// the queue with no word why.
const TOO_FEW_ANALYSED: &str = "sonic:too-few";

/// How far two files of one recording may differ in length and still count
/// as copies: a fade, a gap the release trimmed. Different recordings that
/// only share a title — "I. Allegro" of another work, the "Intro" of another
/// album — rarely come this close.
const COPY_LENGTH_SLACK_MS: u64 = 3_000;

/// Why one of `ids` is not in `index`: analysed without a sound to compare,
/// or not analysed (yet).
fn not_in(state: &AppState, index: &SonicIndex, ids: &[&str]) -> AppError {
    let silent = ids
        .iter()
        .any(|id| !index.contains(id) && crate::sonic::without_sound(&state.store, id));
    AppError::Other(if silent { NO_SOUND } else { NOT_ANALYSED }.into())
}

#[tauri::command]
pub fn sonic_status(state: State<'_, AppState>) -> ScanStatus {
    state.sonic_scan.status()
}

#[tauri::command]
pub fn sonic_set_scan(state: State<'_, AppState>, enabled: bool) {
    state.sonic_scan.set_enabled(&state.store, enabled);
}

/// The index of the session's server, holding `needed` if they have been
/// analysed by now.
async fn index(state: &AppState, needed: &[&str]) -> AppResult<Arc<SonicIndex>> {
    let server = {
        let session = state.session.read().await;
        Server::of(session.as_deref().ok_or(AppError::NotConnected)?)
    };
    let store = state.store.clone();
    let needed: Vec<String> = needed.iter().map(|id| id.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let needed: Vec<&str> = needed.iter().map(String::as_str).collect();
        crate::sonic::index_containing(&store, &server, &needed)
    })
    .await
    .map_err(|e| AppError::Other(format!("sonic index: {e}")))
}

/// Fetch the tracks behind `ids` and queue them in exactly that order (the
/// order is the point), dropping any the server no longer has. Shuffle does
/// not reorder them either: see [`crate::player::PlayerHandle::play_in_order`].
/// Fewer than `min_tracks` left is refused as too few analysed.
async fn play_in_order(
    state: &AppState,
    ticket: PlayTicket,
    ids: Vec<String>,
    min_tracks: usize,
) -> AppResult<()> {
    let (queue, owner, gone) = with_retry(state, |c| {
        let ids = ids.clone();
        async move {
            let mut tracks = c.tracks_by_ids(&ids).await?;
            let found: HashSet<&str> = tracks.iter().map(|t| t.id.as_str()).collect();
            let gone: Vec<String> = ids
                .iter()
                .filter(|id| !found.contains(id.as_str()))
                .cloned()
                .collect();
            let position = |id: &str| ids.iter().position(|x| x == id).unwrap_or(usize::MAX);
            tracks.sort_by_key(|t| position(&t.id));
            let queue = to_queue_tracks(&c, without_copies(tracks))?;
            Ok((queue, Owner::of(&c).ok_or(AppError::NotConnected)?, gone))
        }
    })
    .await?;
    // Tracks the server does not have any more (deleted, or moved to a new
    // item id): out of the index, or every radio keeps reaching for them —
    // unless another account here may still see them.
    let store = state.store.clone();
    let released =
        tokio::task::spawn_blocking(move || crate::sonic::release_gone(&store, &owner, gone)).await;
    match released {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!("cannot release the analyses of gone tracks: {e}"),
        Err(e) => tracing::warn!("cannot release the analyses of gone tracks: {e}"),
    }
    if queue.is_empty() {
        return Err(AppError::Other(
            "none of these tracks are on the server".into(),
        ));
    }
    // What this account can play of it: the index also holds what other
    // accounts here analysed, and copies drop out. A radio of a handful is
    // the queue replaced for nothing.
    if queue.len() < min_tracks {
        return Err(AppError::Other(TOO_FEW_ANALYSED.into()));
    }
    state.player.play_in_order(ticket, queue).await
}

/// Recordings by artist and title, with every length seen under that name.
/// Two files of one recording (album and best-of) sound the same, so a radio
/// puts them next to each other; a name alone would also take a different
/// recording for one.
#[derive(Debug, Clone, Default)]
pub(crate) struct Recordings(HashMap<(String, String), Vec<u64>>);

impl Recordings {
    /// Remember a recording. False when it is a copy of one seen before.
    pub(crate) fn insert(&mut self, artist: &str, name: &str, duration_ms: u64) -> bool {
        let lengths = self.0.entry(recording_key(artist, name)).or_default();
        if lengths.iter().any(|&seen| same_length(seen, duration_ms)) {
            return false;
        }
        lengths.push(duration_ms);
        true
    }
}

fn recording_key(artist: &str, name: &str) -> (String, String) {
    (artist.trim().to_lowercase(), name.trim().to_lowercase())
}

/// An unknown length (0) tells nothing apart, so there the name decides.
fn same_length(a: u64, b: u64) -> bool {
    a == 0 || b == 0 || a.abs_diff(b) <= COPY_LENGTH_SLACK_MS
}

/// The same recording twice in a library (two files, two item ids) sounds
/// the same, so it lands next to itself. Keep the first and the last track
/// (the seed, a path's destination) and drop later copies of anything
/// already in the list, by artist, title and length.
pub(crate) fn without_copies(tracks: Vec<TrackDto>) -> Vec<TrackDto> {
    let mut seen = Recordings::default();
    // Both ends are kept, so copies of either count as seen from the start.
    for end in tracks.first().into_iter().chain(tracks.last()) {
        seen.insert(&end.artist, &end.name, end.duration_ms);
    }
    let count = tracks.len();
    tracks
        .into_iter()
        .enumerate()
        .filter(|(i, t)| {
            *i == 0 || *i + 1 == count || seen.insert(&t.artist, &t.name, t.duration_ms)
        })
        .map(|(_, t)| t)
        .collect()
}

/// Drop every track that is a copy of a recording in `heard` — what plays,
/// is queued or was just heard — or of one earlier in the list. Unlike
/// [`without_copies`] no place is kept: an Auto-DJ batch has no seed in it.
/// Its first track is the one closest to what plays now, and a second file
/// of what plays now is closest of all.
pub(crate) fn without_copies_of(tracks: Vec<TrackDto>, heard: &Recordings) -> Vec<TrackDto> {
    let mut seen = heard.clone();
    tracks
        .into_iter()
        .filter(|t| seen.insert(&t.artist, &t.name, t.duration_ms))
        .collect()
}

/// Play `item_id`, then the tracks that sound most like it, in an order that
/// flows.
#[tauri::command]
pub async fn sonic_radio(state: State<'_, AppState>, item_id: String) -> AppResult<()> {
    // In click order, before the index loads. A refusal below drops it, and
    // an older request still fetching plays after all (see `PlayTicket`).
    let ticket = state.player.play_ticket();
    let index = index(&state, &[&item_id]).await?;
    if !index.contains(&item_id) {
        return Err(not_in(&state, &index, &[&item_id]));
    }
    let neighbours = index.radio(&item_id, RADIO_TRACKS - 1, &HashSet::new());
    // Not a radio's worth analysed: what came back is everything there is,
    // not what sounds alike.
    if neighbours.len() + 1 < RADIO_TRACKS {
        return Err(AppError::Other(TOO_FEW_ANALYSED.into()));
    }
    let mut ids = vec![item_id.clone()];
    ids.extend(neighbours);
    play_in_order(&state, ticket, ids, RADIO_TRACKS / 2).await
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
    // As for the radio: a refusal gives way to an older request.
    let ticket = state.player.play_ticket();
    let index = index(&state, &[&from_id, &to_id]).await?;
    let middle = index
        .path(&from_id, &to_id, steps, &HashSet::new())
        .ok_or_else(|| not_in(&state, &index, &[&from_id, &to_id]))?;
    // The path ran out of tracks on the way: too few are analysed to walk it.
    if middle.len() < steps {
        return Err(AppError::Other(TOO_FEW_ANALYSED.into()));
    }
    let mut ids = vec![from_id];
    ids.extend(middle);
    ids.push(to_id);
    // Both ends and at least half the way between them.
    play_in_order(&state, ticket, ids, steps / 2 + 2).await
}

#[cfg(test)]
mod tests {
    use super::{without_copies, without_copies_of, Recordings};
    use crate::api::types::TrackDto;

    fn track(id: &str, artist: &str, name: &str) -> TrackDto {
        TrackDto {
            id: id.into(),
            artist: artist.into(),
            name: name.into(),
            ..TrackDto::default()
        }
    }

    fn timed(id: &str, artist: &str, name: &str, duration_ms: u64) -> TrackDto {
        TrackDto {
            duration_ms,
            ..track(id, artist, name)
        }
    }

    fn ids(tracks: Vec<TrackDto>) -> Vec<String> {
        tracks.into_iter().map(|t| t.id).collect()
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
        // The first copy of the seed is still a different file of it: gone.
        assert_eq!(ids(without_copies(tracks)), ["seed", "y", "end"]);
    }

    #[test]
    fn recordings_that_only_share_a_title_are_not_copies() {
        let tracks = vec![
            timed("seed", "Mozart", "I. Allegro", 610_000),
            timed("k467", "Mozart", "I. Allegro", 845_000),
            timed("k488", "Mozart", "I. Allegro", 683_000),
            // Another file of the seed: a second or two off, the same take.
            timed("copy", "Mozart", "I. Allegro", 611_500),
            timed("end", "Mozart", "II. Andante", 420_000),
        ];
        assert_eq!(ids(without_copies(tracks)), ["seed", "k467", "k488", "end"]);
    }

    #[test]
    fn an_auto_dj_batch_drops_copies_of_what_was_heard_even_first() {
        let mut heard = Recordings::default();
        heard.insert("Artist", "Song", 200_000);
        let batch = vec![
            // The compilation copy of the track that is playing: closest of
            // all, so the continuation puts it first.
            timed("copy", "artist", "Song ", 201_000),
            timed("a", "Other", "Tune", 180_000),
            timed("a2", "Other", "Tune", 180_400),
            // A live take that runs longer is another recording.
            timed("live", "Artist", "Song", 260_000),
        ];
        assert_eq!(ids(without_copies_of(batch, &heard)), ["a", "live"]);
        // The set it was given is not changed by it.
        assert!(heard.insert("Other", "Tune", 180_000));
    }

    #[test]
    fn an_unknown_length_leaves_the_name_to_decide() {
        let mut seen = Recordings::default();
        assert!(seen.insert("A", "Song", 0));
        assert!(!seen.insert("a", "song", 312_000));
        assert!(seen.insert("A", "Other", 312_000));
    }
}
