pub mod dsp;
pub mod fade;
pub mod opus;
mod scrobble;
pub mod source;
pub mod tap;
pub mod waveform;

use crate::api::JellyfinClient;
use crate::error::{AppError, AppResult};
use crate::store::{keys, Store};
use dsp::AudioDsp;
pub use dsp::DspParams;
use fade::FadeHandle;
use serde::{Deserialize, Serialize};
use source::{DownloadProgress, OpenPurpose, OpenedSource, StreamAuth, TrackSource};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

pub type SharedSession = Arc<tokio::sync::RwLock<Option<Arc<JellyfinClient>>>>;

/// Worker tick interval; position events, prefetch triggers and end-of-track
/// detection run on it.
const TICK: Duration = Duration::from_millis(400);
/// How long to wait between attempts to open the audio output at startup when
/// no device is available yet.
const OUTPUT_RETRY_DELAY: Duration = Duration::from_secs(3);
/// Upper bound on commands buffered while the audio output is unavailable.
const DEFERRED_COMMAND_LIMIT: usize = 256;
/// Report playback progress to the server roughly every 10 s (25 ticks).
const REPORT_EVERY_TICKS: u32 = 25;
/// Start opening the next track's stream this long before the current ends.
const PREFETCH_OPEN_REMAINING_MS: u64 = 20_000;
/// Hand the decoded next track to rodio this long before the boundary; rodio
/// then plays the two sources back to back without a gap.
const APPEND_REMAINING_MS: u64 = 3_000;
/// Pause/resume gain ramp length.
const PAUSE_FADE_MS: u64 = 150;

/// User-facing playback behavior (persisted as one JSON blob).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CrossfadeMode {
    #[default]
    Off,
    Smart,
    Always,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaybackSettings {
    pub crossfade_mode: CrossfadeMode,
    /// 1..=12 s, clamped on use.
    pub crossfade_seconds: f32,
    /// None = system default output device.
    pub output_device: Option<String>,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            crossfade_mode: CrossfadeMode::Off,
            crossfade_seconds: 6.0,
            output_device: None,
        }
    }
}

/// Optional integrations, consumed by the player worker (persisted as JSON).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoDjSeedMode {
    #[default]
    Track,
    Artist,
    Genre,
    Album,
    Playlist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExtrasSettings {
    /// ListenBrainz user token; scrobbling is on when set. Lives in the
    /// credential manager — the store/export only ever sees `None`. In
    /// `set_extras_settings`: `Some("")` clears the stored token, `None`
    /// keeps it.
    pub listenbrainz_token: Option<String>,
    /// Append an Instant Mix when the queue runs out.
    pub auto_dj_enabled: bool,
    pub auto_dj_seed_mode: AutoDjSeedMode,
    pub auto_dj_max_tracks: u32,
    pub auto_dj_recent_tracks: u32,
    /// UI-only, recomputed on read: a token is stored (the token itself is
    /// never sent to the WebView).
    pub listenbrainz_configured: bool,
}

impl Default for ExtrasSettings {
    fn default() -> Self {
        Self {
            listenbrainz_token: None,
            auto_dj_enabled: false,
            auto_dj_seed_mode: AutoDjSeedMode::Track,
            auto_dj_max_tracks: 25,
            auto_dj_recent_tracks: 50,
            listenbrainz_configured: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoDjReason {
    pub kind: AutoDjSeedMode,
    pub label: String,
}

/// One entry in the play queue. Everything the player and the UI need to show
/// and start a track without asking the server again. Persisted as-is.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueTrack {
    pub item_id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    #[serde(default)]
    pub track_number: Option<i32>,
    #[serde(default)]
    pub disc_number: Option<i32>,
    pub duration_ms: u64,
    pub image_item_id: Option<String>,
    pub image_tag: Option<String>,
    #[serde(default)]
    pub image_blur_hash: Option<String>,
    /// The track's stream, without the parameters of a single attempt at
    /// playing it: the play session and a start position are added when a
    /// source is opened (`spawn_open`, `source::open_track_source`).
    pub stream_url: String,
    /// Identity of this queue line, minted when it was queued and kept across
    /// moves. The UI keys its rows by it, which is how two copies of the same
    /// track stay apart; it never reaches the server. Empty in queues
    /// persisted before it existed — the UI falls back to the item id there.
    #[serde(default, alias = "playSessionId")]
    pub entry_id: String,
    /// Jellyfin `NormalizationGain` (dB), input to volume normalization.
    #[serde(default)]
    pub normalization_gain: Option<f32>,
    /// The track's artists as linkable refs (empty in queues from old builds).
    #[serde(default)]
    pub artists: Vec<crate::api::types::ArtistRef>,
    #[serde(default)]
    pub genres: Vec<crate::api::types::GenreRef>,
    #[serde(default)]
    pub source_playlist_id: Option<String>,
    #[serde(default)]
    pub auto_dj_reason: Option<AutoDjReason>,
    /// Favorite state snapshot at queue-build time (the player heart applies
    /// optimistic overrides on top). Defaults false in queues from old builds.
    #[serde(default)]
    pub is_favorite: bool,
}

/// Pure transition policy. The audio hot path only acts on this answer.
///
/// Unknown/live durations and tracks that cannot accommodate the full fade on
/// both sides always remain gapless. Smart mode preserves an album's declared
/// track/disc order and crossfades every other transition (mixes/radio queues
/// naturally consist of such unrelated transitions).
fn should_crossfade(
    mode: CrossfadeMode,
    fade_ms: u64,
    current: &QueueTrack,
    next: &QueueTrack,
) -> bool {
    if mode == CrossfadeMode::Off
        || fade_ms == 0
        || current.duration_ms == 0
        || next.duration_ms == 0
        || current.duration_ms < fade_ms.saturating_mul(2)
        || next.duration_ms < fade_ms.saturating_mul(2)
    {
        return false;
    }
    if mode == CrossfadeMode::Always {
        return true;
    }

    !is_album_sequence(current, next)
}

fn is_album_sequence(current: &QueueTrack, next: &QueueTrack) -> bool {
    let same_album = current
        .album_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .is_some_and(|id| next.album_id.as_deref() == Some(id));
    if !same_album {
        return false;
    }

    match (
        current.disc_number,
        current.track_number,
        next.disc_number,
        next.track_number,
    ) {
        (Some(disc), Some(track), Some(next_disc), Some(next_track)) => {
            // checked_add so a hostile/corrupt item with track/disc == i32::MAX
            // can't overflow (debug panic / release wrap).
            (next_disc == disc && track.checked_add(1) == Some(next_track))
                || (disc.checked_add(1) == Some(next_disc) && next_track == 1)
        }
        _ => false,
    }
}

/// Whether the tick should start opening the next track (`next_id`) ahead of
/// the boundary: inside the prefetch window, nothing opened or opening yet
/// (`busy`), and not already appended to the sink. The gapless hand-off takes
/// the prefetched source, which leaves both slots empty until the transition —
/// opening the track again there would start a second stream (or transcode)
/// that is thrown away at the boundary.
fn should_start_prefetch(
    busy: bool,
    appended: Option<&str>,
    next_id: &str,
    duration_ms: u64,
    remaining_ms: u64,
) -> bool {
    !busy
        && appended != Some(next_id)
        && (duration_ms == 0 || remaining_ms <= PREFETCH_OPEN_REMAINING_MS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackStatus {
    Idle,
    /// A play command was accepted and the source is being opened on a
    /// background thread.
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Loop the whole queue (wraps at both ends).
    All,
    /// Loop the current track.
    One,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShuffleMode {
    #[default]
    Off,
    Tracks,
    Albums,
}

/// Persisted shuffle + repeat state (`keys::PLAY_MODE`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PlayMode {
    shuffle_mode: ShuffleMode,
    repeat: RepeatMode,
}

/// Successor in play order. `order` is the shuffle permutation of queue indices
/// (empty = linear order). `repeat_all` wraps at the end. None = genuine end.
fn order_next(len: usize, index: usize, order: &[usize], repeat_all: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if order.is_empty() {
        if index + 1 < len {
            Some(index + 1)
        } else if repeat_all {
            Some(0)
        } else {
            None
        }
    } else {
        let pos = order.iter().position(|&i| i == index)?;
        if pos + 1 < order.len() {
            Some(order[pos + 1])
        } else if repeat_all {
            order.first().copied()
        } else {
            None
        }
    }
}

/// Predecessor in play order (mirror of `order_next`).
fn order_prev(len: usize, index: usize, order: &[usize], repeat_all: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if order.is_empty() {
        if index > 0 {
            Some(index - 1)
        } else if repeat_all {
            Some(len - 1)
        } else {
            None
        }
    } else {
        let pos = order.iter().position(|&i| i == index)?;
        if pos > 0 {
            Some(order[pos - 1])
        } else if repeat_all {
            order.last().copied()
        } else {
            None
        }
    }
}

/// Remap a physical queue index through a move (remove `from`, insert at `to`).
fn remap_move(v: usize, from: usize, to: usize) -> usize {
    if v == from {
        to
    } else if from < to && v > from && v <= to {
        v - 1
    } else if from > to && v >= to && v < from {
        v + 1
    } else {
        v
    }
}

/// Remap an index after removing a sorted set of physical queue indices.
fn remap_after_removals(index: usize, removed: &[usize]) -> Option<usize> {
    if removed.binary_search(&index).is_ok() {
        None
    } else {
        Some(index - removed.partition_point(|&i| i < index))
    }
}

/// Remap a shuffle permutation after removing physical queue indices.
fn remap_order_after_removals(order: &[usize], removed: &[usize]) -> Vec<usize> {
    order
        .iter()
        .filter_map(|&index| remap_after_removals(index, removed))
        .collect()
}

/// Tracks before the current one in actual play order. With shuffle disabled,
/// physical queue order is the play order.
fn played_indices(len: usize, current: usize, order: &[usize]) -> Vec<usize> {
    if len == 0 || current >= len {
        return Vec::new();
    }
    if !order.is_empty() {
        if let Some(position) = order.iter().position(|&index| index == current) {
            return order[..position]
                .iter()
                .copied()
                .filter(|&index| index < len)
                .collect();
        }
    }
    (0..current).collect()
}

/// Duplicate occurrences to remove. The current occurrence wins over an
/// earlier copy; all other item ids keep their first occurrence.
fn duplicate_indices(tracks: &[QueueTrack], current: usize) -> Vec<usize> {
    let mut keep = std::collections::HashMap::<&str, usize>::new();
    if let Some(track) = tracks.get(current) {
        keep.insert(track.item_id.as_str(), current);
    }
    for (index, track) in tracks.iter().enumerate() {
        keep.entry(track.item_id.as_str()).or_insert(index);
    }
    tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| (keep[track.item_id.as_str()] != index).then_some(index))
        .collect()
}

/// Build a shuffle permutation of `0..len` with `current` first and the rest
/// Fisher–Yates-shuffled from `seed` (time-seeded at the call site).
fn build_shuffle_order(len: usize, current: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    if current != 0 && current < len {
        order.swap(0, current);
    }
    if order.len() > 2 {
        let sub = &mut order[1..];
        let mut state = seed | 1;
        for i in (1..sub.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let j = (state % (i as u64 + 1)) as usize;
            sub.swap(i, j);
        }
    }
    order
}

fn shuffle_slice<T>(values: &mut [T], state: &mut u64) {
    for i in (1..values.len()).rev() {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        values.swap(i, (*state % (i as u64 + 1)) as usize);
    }
}

/// Shuffle whole albums while preserving disc/track order inside each block.
/// The current track's block comes first and starts at the current track; the
/// album's earlier tracks follow the end of the block, so they still play.
/// Tracks without an album id deliberately form singleton blocks.
fn build_album_shuffle_order(tracks: &[QueueTrack], current: usize, seed: u64) -> Vec<usize> {
    if tracks.is_empty() || current >= tracks.len() {
        return Vec::new();
    }
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        let key = track
            .album_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("__track_{index}"));
        if let Some((_, indices)) = groups.iter_mut().find(|(group, _)| group == &key) {
            indices.push(index);
        } else {
            groups.push((key, vec![index]));
        }
    }
    for (_, indices) in &mut groups {
        indices.sort_by_key(|&index| {
            let track = &tracks[index];
            (
                track.disc_number.unwrap_or(1),
                track.track_number.unwrap_or(i32::MAX),
                index,
            )
        });
    }
    let current_group = groups
        .iter()
        .position(|(_, indices)| indices.contains(&current))
        .unwrap_or(0);
    groups.swap(0, current_group);
    // Started mid-album: continue the album, then wrap to its start. Tracks
    // before the current one in play order would never play with repeat off
    // (and count as played).
    if let Some(at) = groups[0].1.iter().position(|&index| index == current) {
        groups[0].1.rotate_left(at);
    }
    let mut state = seed | 1;
    if groups.len() > 2 {
        shuffle_slice(&mut groups[1..], &mut state);
    }
    groups
        .into_iter()
        .flat_map(|(_, indices)| indices)
        .collect()
}

fn choose_auto_dj_seed(track: &QueueTrack, requested: AutoDjSeedMode) -> (String, AutoDjReason) {
    let candidate = match requested {
        AutoDjSeedMode::Artist => track
            .artists
            .first()
            .map(|item| (item.id.clone(), item.name.clone())),
        AutoDjSeedMode::Genre => track
            .genres
            .first()
            .map(|item| (item.id.clone(), item.name.clone())),
        AutoDjSeedMode::Album => track
            .album_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .map(|id| (id.clone(), track.album.clone())),
        AutoDjSeedMode::Playlist => track
            .source_playlist_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .map(|id| (id.clone(), String::new())),
        AutoDjSeedMode::Track => Some((track.item_id.clone(), track.name.clone())),
    };
    let (kind, id, label) = candidate
        .map(|(id, label)| (requested, id, label))
        .unwrap_or_else(|| {
            (
                AutoDjSeedMode::Track,
                track.item_id.clone(),
                track.name.clone(),
            )
        });
    (id, AutoDjReason { kind, label })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub status: PlaybackStatus,
    pub current: Option<QueueTrack>,
    pub index: usize,
    pub queue_len: usize,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub shuffle: bool,
    pub shuffle_mode: ShuffleMode,
    pub repeat: RepeatMode,
    /// Some while a sleep timer is armed; ms until it pauses playback
    /// (0 while waiting for the end of the current track).
    pub sleep_remaining_ms: Option<u64>,
    /// Downloaded part of the current track as `[start, end]` in ms, for the
    /// seek bar. Estimated from bytes, so approximate for variable bitrates;
    /// `None` when the stream length is unknown (live transcode).
    pub buffered_ms: Option<(u64, u64)>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            current: None,
            index: 0,
            queue_len: 0,
            position_ms: 0,
            duration_ms: 0,
            volume: 1.0,
            shuffle: false,
            shuffle_mode: ShuffleMode::Off,
            repeat: RepeatMode::Off,
            sleep_remaining_ms: None,
            buffered_ms: None,
        }
    }
}

/// Map a downloaded byte fraction onto the track's timeline.
/// A fresh play session id. See `spawn_open` for what the server does with it.
fn new_play_session() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn buffered_range_ms(fraction: Option<(f64, f64)>, duration_ms: u64) -> Option<(u64, u64)> {
    let (start, end) = fraction?;
    if duration_ms == 0 {
        return None;
    }
    let at = |f: f64| (f.clamp(0.0, 1.0) * duration_ms as f64).round() as u64;
    Some((at(start), at(end)))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub tracks: Vec<QueueTrack>,
    pub index: usize,
    /// Shuffle play order as indices into `tracks`; empty while shuffle is
    /// off (then `tracks` is the play order). `queue_move` positions refer to
    /// this order.
    pub order: Vec<usize>,
    pub can_undo: bool,
    pub played_count: usize,
    pub duplicate_count: usize,
}

pub enum PlayerCommand {
    /// Replace queue and start playing at `start_index`.
    PlayQueue {
        tracks: Vec<QueueTrack>,
        start_index: usize,
    },
    /// Load a queue without starting playback (session restore).
    RestoreQueue {
        tracks: Vec<QueueTrack>,
        index: usize,
    },
    /// Insert right after the current track.
    PlayNext(Vec<QueueTrack>),
    /// Append to the end of the queue.
    PlayLast(Vec<QueueTrack>),
    RemoveAt {
        index: usize,
        /// Identity check against stale UI indices (e.g. double-click on the
        /// remove button must not delete the track that slid into the slot).
        item_id: String,
    },
    JumpTo(usize),
    MoveTrack {
        from: usize,
        to: usize,
    },
    /// Remove every track and stop.
    ClearQueue,
    /// Drop the queue completely and leave NO undo snapshot. Used on sign-out
    /// and server change: a plain `ClearQueue` remembers the queue for undo,
    /// which would hand the previous account's tracks to the next one.
    ResetQueue,
    /// Flush the final playback report and end the worker. The sender waits on
    /// `ack` so the process does not exit while the report is still in flight.
    Shutdown {
        ack: mpsc::SyncSender<()>,
    },
    /// Stop and report, acknowledging when the report has actually been sent.
    /// Used on sign-out, which drops the session right afterwards.
    StopAck {
        ack: mpsc::SyncSender<()>,
    },
    /// Remove tracks before the current one in actual play order.
    RemovePlayed,
    /// Keep one occurrence per item id, always preserving the current one.
    RemoveDuplicates,
    /// Restore the exact queue state before the last removal/clear operation.
    UndoQueue,
    SetShuffleMode(ShuffleMode),
    SetRepeat(RepeatMode),
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
    Seek {
        position_ms: u64,
    },
    SetVolume {
        volume: f32,
    },
    SetPlaybackSettings(PlaybackSettings),
    SetExtrasSettings(ExtrasSettings),
    /// None clears the timer; `end_of_track` pauses at the next boundary
    /// after the minutes elapsed (or immediately at the boundary when 0).
    SetSleepTimer {
        minutes: Option<u32>,
        end_of_track: bool,
        fade_seconds: u32,
    },
    /// The active output stream died (device unplugged); sent by the cpal
    /// error callback, never by the UI.
    OutputFailed,
    /// Auto-DJ instant-mix fetch finished (sent by its background task).
    /// `tracks` is empty on failure so the seed latch can be released.
    AutoDjResult {
        request_track: String,
        reason: AutoDjReason,
        tracks: Vec<QueueTrack>,
    },
    Stop,
    /// A missing SMTC cover finished downloading into the disk cache
    /// (sent by a background task, never by the UI).
    RefreshSmtcArtwork {
        item_id: String,
    },
    /// A background open finished (sent by the open threads, never by the UI).
    SourceReady {
        generation: u64,
        /// The worker's `prefetch_generation` at spawn time; only a prefetch
        /// checks it.
        prefetch_generation: u64,
        purpose: OpenPurpose,
        track: Box<QueueTrack>,
        /// The play session this source was opened under; it is in the stream
        /// URL the server answered, so the reports have to use the same one.
        play_session: String,
        result: AppResult<Box<OpenedSource>>,
    },
}

#[derive(Serialize, Deserialize)]
struct PersistedQueue {
    tracks: Vec<QueueTrack>,
    index: usize,
    /// Server the entries were built against. Their `stream_url` is absolute,
    /// so a snapshot must never be restored under a different session — see
    /// [`load_persisted_queue`]. Absent in snapshots from older builds, which
    /// are therefore discarded rather than trusted.
    #[serde(default)]
    server_url: Option<String>,
    /// User the entries were built for. The stream URL carries `userId`, and
    /// the queue is listening history — neither belongs to the next account
    /// that signs in on the same server.
    #[serde(default)]
    user_id: Option<String>,
    /// Names this track list for the index saved apart from it
    /// ([`PersistedQueueIndex`]). Absent in snapshots from older builds,
    /// which restore at their own `index`.
    #[serde(default)]
    snapshot_id: Option<String>,
}

/// The queue index, saved apart from the track list (`keys::QUEUE_INDEX`): it
/// moves on every track change, and writing it into the snapshot would
/// re-serialize the whole queue each time. Applies only to the snapshot it
/// names.
#[derive(Serialize, Deserialize)]
struct PersistedQueueIndex {
    snapshot_id: String,
    index: usize,
}

/// What `sync_queue` last wrote: the track list as of `revision` (saved under
/// `snapshot_id`) and the index stored for it.
struct PersistedQueueMark {
    revision: u64,
    snapshot_id: String,
    index: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum QueueWrite {
    /// The track list changed (or was never written): the whole snapshot.
    Snapshot,
    /// Only the index moved: the small index entry.
    Index,
    Nothing,
}

fn queue_write(written: Option<&PersistedQueueMark>, revision: u64, index: usize) -> QueueWrite {
    match written {
        Some(mark) if mark.revision == revision => {
            if mark.index == index {
                QueueWrite::Nothing
            } else {
                QueueWrite::Index
            }
        }
        _ => QueueWrite::Snapshot,
    }
}

/// Index to restore a snapshot at: the separately saved one when it belongs to
/// this very track list, otherwise the snapshot's own.
fn restored_index(snapshot: &PersistedQueue, saved_index: Option<&str>) -> usize {
    saved_index
        .and_then(|json| serde_json::from_str::<PersistedQueueIndex>(json).ok())
        .filter(|saved| snapshot.snapshot_id.as_deref() == Some(saved.snapshot_id.as_str()))
        .map_or(snapshot.index, |saved| saved.index)
}

struct Prefetched {
    item_id: String,
    /// Session the prefetch was opened under, handed on to the report when
    /// this source actually starts playing.
    play_session: String,
    source: TrackSource,
    fade: FadeHandle,
    tap_enabled: Arc<std::sync::atomic::AtomicBool>,
    download: Arc<DownloadProgress>,
}

#[derive(Clone)]
struct QueueUndo {
    tracks: Vec<QueueTrack>,
    index: usize,
    order: Vec<usize>,
    last_auto_dj_seed: Option<String>,
    status: PlaybackStatus,
}

#[derive(Clone)]
pub struct PlayerHandle {
    tx: Sender<PlayerCommand>,
    state: Arc<Mutex<PlayerState>>,
    queue: Arc<Mutex<QueueSnapshot>>,
    dsp: Arc<AudioDsp>,
    tap: Arc<tap::VisualizerTap>,
    waveforms: Arc<waveform::WaveformCache>,
}

/// `player:waveform` — the seek bar's peaks for one track, sent once its
/// analysis finished (see `waveform.rs`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WaveformEvent<'a> {
    item_id: &'a str,
    peaks: &'a [u8],
}

impl PlayerHandle {
    pub fn send(&self, cmd: PlayerCommand) -> AppResult<()> {
        self.tx
            .send(cmd)
            .map_err(|_| AppError::Audio("player thread is gone".into()))
    }

    /// Ask the worker to flush its final playback report, then wait (bounded)
    /// for it. Called on the quit paths before `app.exit(0)`; a timeout just
    /// means we stop waiting, never that the app fails to close.
    pub fn shutdown(&self, timeout: Duration) {
        let (ack, done) = mpsc::sync_channel(1);
        if self.tx.send(PlayerCommand::Shutdown { ack }).is_err() {
            return;
        }
        if done.recv_timeout(timeout).is_err() {
            tracing::warn!("player did not finish its shutdown report in time");
        }
    }

    /// Stop playback and wait (bounded) until the "stopped" report has been
    /// sent. Blocking — call it off the async runtime.
    pub fn stop_and_report(&self, timeout: Duration) {
        let (ack, done) = mpsc::sync_channel(1);
        if self.tx.send(PlayerCommand::StopAck { ack }).is_err() {
            return;
        }
        if done.recv_timeout(timeout).is_err() {
            tracing::warn!("player did not confirm the stop report in time");
        }
    }

    pub fn state(&self) -> PlayerState {
        self.state.lock().unwrap().clone()
    }

    pub fn queue_snapshot(&self) -> QueueSnapshot {
        self.queue.lock().unwrap().clone()
    }

    pub fn dsp(&self) -> &Arc<AudioDsp> {
        &self.dsp
    }

    pub fn tap(&self) -> &Arc<tap::VisualizerTap> {
        &self.tap
    }

    /// Peaks of an already analysed track — for a UI that missed the event
    /// (reload, a window opened later).
    pub fn waveform(&self, item_id: &str) -> Option<Vec<u8>> {
        self.waveforms.get(item_id).map(|peaks| peaks.to_vec())
    }
}

/// Spawn the player thread. It owns the audio output for the whole app
/// lifetime; the WebView never touches audio.
pub fn spawn(
    app: tauri::AppHandle,
    store: Arc<Store>,
    session: SharedSession,
    hwnd: Option<isize>,
    cover_dir: PathBuf,
) -> PlayerHandle {
    let (tx, rx) = mpsc::channel::<PlayerCommand>();
    let shared_state = Arc::new(Mutex::new(PlayerState::default()));
    let shared_queue = Arc::new(Mutex::new(QueueSnapshot {
        tracks: Vec::new(),
        index: 0,
        order: Vec::new(),
        can_undo: false,
        played_count: 0,
        duplicate_count: 0,
    }));
    let dsp_params = store
        .get(keys::AUDIO_DSP)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    let dsp = Arc::new(AudioDsp::new(dsp_params));
    let visualizer_tap = Arc::new(tap::VisualizerTap::new());
    let waveforms = Arc::new(waveform::WaveformCache::default());
    let handle = PlayerHandle {
        tx: tx.clone(),
        state: shared_state.clone(),
        queue: shared_queue.clone(),
        dsp: dsp.clone(),
        tap: visualizer_tap.clone(),
        waveforms: waveforms.clone(),
    };

    std::thread::Builder::new()
        .name("jellysic-player".into())
        .spawn(move || {
            // Building the worker opens the audio device. If that fails, do
            // NOT return: dropping `rx` here would make every later command
            // fail with "player thread is gone" for the rest of the process,
            // and the runtime device-recovery lives inside the loop below that
            // would never start. Keep retrying instead, so a device that shows
            // up late (or is picked in settings) revives playback.
            // Commands that arrive before the output exists are kept, not
            // dropped: `lib.rs` sends exactly one `RestoreQueue` right after
            // spawn, so discarding it would silently lose the user's queue
            // whenever the audio device is late.
            let mut deferred: Vec<PlayerCommand> = Vec::new();
            let mut reported_output_error = false;
            let mut worker = loop {
                match Worker::new(
                    app.clone(),
                    store.clone(),
                    session.clone(),
                    shared_state.clone(),
                    shared_queue.clone(),
                    dsp.clone(),
                    visualizer_tap.clone(),
                    waveforms.clone(),
                    tx.clone(),
                    hwnd,
                    cover_dir.clone(),
                ) {
                    Ok(w) => break w,
                    Err(e) => {
                        tracing::error!("audio output init failed: {e}");
                        // Report once, not every 3 s — a missing device would
                        // otherwise flood the UI with the same banner.
                        if deferred.is_empty() && !reported_output_error {
                            reported_output_error = true;
                            let _ =
                                app.emit("player:error", format!("audio output unavailable: {e}"));
                        }
                        // Collect commands while waiting, and honour a quit
                        // request immediately.
                        let deadline = Instant::now() + OUTPUT_RETRY_DELAY;
                        loop {
                            let wait = deadline.saturating_duration_since(Instant::now());
                            match rx.recv_timeout(wait) {
                                Ok(PlayerCommand::Shutdown { ack }) => {
                                    let _ = ack.send(());
                                    return;
                                }
                                Ok(PlayerCommand::StopAck { ack }) => {
                                    let _ = ack.send(());
                                }
                                Ok(cmd) => {
                                    // Bound it: a long outage must not grow
                                    // this without limit.
                                    if deferred.len() < DEFERRED_COMMAND_LIMIT {
                                        deferred.push(cmd);
                                    }
                                }
                                Err(RecvTimeoutError::Timeout) => break,
                                Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    }
                }
            };
            // Everything that arrived while there was no output device.
            for cmd in deferred.drain(..) {
                worker.handle(cmd);
            }
            loop {
                match rx.recv_timeout(TICK) {
                    Ok(PlayerCommand::Shutdown { ack }) => {
                        worker.shutdown();
                        let _ = ack.send(());
                        break;
                    }
                    Ok(cmd) => worker.handle(cmd),
                    Err(RecvTimeoutError::Timeout) => worker.tick(),
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("failed to spawn player thread");

    handle
}

struct Worker {
    app: tauri::AppHandle,
    store: Arc<Store>,
    session: SharedSession,
    shared_state: Arc<Mutex<PlayerState>>,
    shared_queue: Arc<Mutex<QueueSnapshot>>,
    output: rodio::stream::MixerDeviceSink,
    media: Option<souvlaki::MediaControls>,
    dsp: Arc<AudioDsp>,
    tap: Arc<tap::VisualizerTap>,
    /// Handed to every open, so a fully downloaded track gets its seek-bar
    /// waveform (analysed off this thread, see `waveform.rs`).
    waveform: waveform::WaveformRequest,
    tx: Sender<PlayerCommand>,
    playback: PlaybackSettings,
    extras: ExtrasSettings,
    repeat: RepeatMode,
    shuffle: bool,
    shuffle_mode: ShuffleMode,
    /// Shuffle play order as physical queue indices (current-first permutation).
    /// Meaningful only while `shuffle`; empty otherwise (= linear order).
    order: Vec<usize>,
    /// Last track that seeded an Auto-DJ append (fires once per track).
    last_auto_dj_seed: Option<String>,
    /// Recently completed tracks excluded from new Auto-DJ mixes.
    recent_tracks: std::collections::VecDeque<String>,
    /// Exact state before the most recent removal/clear operation. It is not
    /// persisted and is invalidated by unrelated queue/order changes.
    queue_undo: Option<QueueUndo>,
    /// Wall-clock moment the sleep timer fires (None = not armed).
    sleep_at: Option<Instant>,
    sleep_end_of_track: bool,
    sleep_fade_duration: Duration,
    /// Item id whose source currently carries the sleep ramp.
    sleep_fade_item: Option<String>,
    sink: Option<rodio::Player>,
    /// Gain handle of the source in `sink`.
    current_fade: Option<FadeHandle>,
    /// Gain handle of a gapless-appended next source; promoted to
    /// `current_fade` at the transition.
    pending_fade: Option<FadeHandle>,
    /// Visualizer-feed toggle of the source in `sink`.
    current_tap: Option<Arc<std::sync::atomic::AtomicBool>>,
    pending_tap: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Download progress of the source in `sink` (the seek bar's buffered
    /// range); `pending_download` belongs to the gapless-appended source and
    /// is promoted with it — same life cycle as the fade and tap handles.
    current_download: Option<Arc<DownloadProgress>>,
    pending_download: Option<Arc<DownloadProgress>>,
    /// Play session of the gapless-appended source, promoted with it when the
    /// boundary is crossed — same life cycle as the fade, tap and download
    /// handles above.
    pending_play_session: Option<String>,
    /// Old sink still audible during a crossfade (kept steerable via its
    /// fade handle), dropped after its ramp.
    fading_out: Option<(rodio::Player, FadeHandle, Instant)>,
    /// Debounces OutputFailed storms from a dying device.
    last_output_rebuild: Instant,
    /// An OutputFailed arrived inside the debounce window; retried on a tick.
    output_failed_pending: bool,
    /// False after the output died and could not be rebuilt; the next play
    /// attempt re-opens it.
    output_alive: bool,
    /// Set by the stream's error callback the moment the device dies, ahead of
    /// the queued OutputFailed. rodio waits for the audio callback to carry out
    /// a native seek, and a dead stream never calls back.
    output_dead: Arc<std::sync::atomic::AtomicBool>,
    queue: Vec<QueueTrack>,
    /// Bumped by every change to `queue` (see `queue_mut`), so `sync_queue`
    /// re-writes the track list only when it actually changed.
    queue_revision: u64,
    /// What `sync_queue` last persisted.
    persisted_queue: Option<PersistedQueueMark>,
    index: usize,
    status: PlaybackStatus,
    volume: f32,
    /// Invalidates in-flight background opens; bumped whenever "what should
    /// play" changes.
    generation: u64,
    /// Invalidates only in-flight prefetches: a queue edit re-decides the next
    /// track, while the current track's own open is still wanted.
    prefetch_generation: u64,
    /// Next track, opened and decoded, waiting to be appended.
    prefetched: Option<Prefetched>,
    /// item_id of a prefetch open currently running on a background thread.
    prefetch_inflight: Option<String>,
    /// item_id of the next track already appended to the sink (gapless
    /// hand-off). Valid iff it still equals `queue[index + 1]` at transition
    /// time — that single invariant survives queue edits that shift indices
    /// (they shift `index` and the next slot together) and is immune to
    /// duplicate item_ids elsewhere in the queue.
    appended: Option<String>,
    /// Pause pressed while a source was still loading; applied on arrival.
    pending_pause: bool,
    /// Playback stopped because the queue ran out (not Stop, a sleep timer or
    /// a lost output): an Auto-DJ mix arriving now resumes into it.
    queue_ended: bool,
    /// Media-time of the current source's first sample. Non-zero only after a
    /// reopen at a position (`restart_at`; rodio's get_pos is
    /// source-relative).
    position_offset_ms: u64,
    /// Jellyfin play session of the playback currently being reported — the
    /// one in the stream URL of the source in `sink`, minted when that source
    /// was opened (`spawn_open`). Taken when the track stops, so the same id
    /// is never reported started twice.
    play_session_id: Option<String>,
    ticks_since_report: u32,
    /// Time actually listened to the current track (the ListenBrainz rule).
    listen: scrobble::ListenClock,
    cover_dir: PathBuf,
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        app: tauri::AppHandle,
        store: Arc<Store>,
        session: SharedSession,
        shared_state: Arc<Mutex<PlayerState>>,
        shared_queue: Arc<Mutex<QueueSnapshot>>,
        dsp: Arc<AudioDsp>,
        tap: Arc<tap::VisualizerTap>,
        waveforms: Arc<waveform::WaveformCache>,
        tx: Sender<PlayerCommand>,
        hwnd: Option<isize>,
        cover_dir: PathBuf,
    ) -> AppResult<Self> {
        let waveform = waveform::WaveformRequest {
            cache: waveforms,
            notify: {
                let app = app.clone();
                Arc::new(move |item_id: &str, peaks: &[u8]| {
                    let _ = app.emit("player:waveform", WaveformEvent { item_id, peaks });
                })
            },
        };
        let playback: PlaybackSettings = store
            .get(keys::PLAYBACK)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let mut extras: ExtrasSettings = store
            .get(keys::EXTRAS)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        // The token lives in the credential manager, not the store.
        if extras.listenbrainz_token.is_none() {
            extras.listenbrainz_token = crate::commands::listenbrainz_token();
        }
        let play_mode: PlayMode = store
            .get(keys::PLAY_MODE)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let (output, output_dead) = match open_output(&playback.output_device, tx.clone()) {
            Ok(opened) => {
                crate::audio_devices::clear_fallback();
                opened
            }
            Err(e) => {
                tracing::warn!("configured output device failed ({e}); using default");
                let opened = open_output(&None, tx.clone())?;
                if let Some(requested_device) = playback.output_device.clone() {
                    crate::audio_devices::mark_fallback(requested_device.clone());
                    let _ = app.emit(
                        "audio:output-fallback",
                        crate::audio_devices::AudioOutputFallbackEvent { requested_device },
                    );
                }
                opened
            }
        };
        let volume = store
            .get(keys::VOLUME)
            .and_then(|v| v.parse::<f32>().ok())
            // A tampered store/import must not produce >1.0 or NaN gain.
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 1.0))
            .unwrap_or(1.0);
        let media = init_media_controls(hwnd, tx.clone());
        Ok(Self {
            app,
            store,
            session,
            shared_state,
            shared_queue,
            output,
            media,
            dsp,
            tap,
            waveform,
            tx,
            playback,
            extras,
            repeat: play_mode.repeat,
            shuffle: play_mode.shuffle_mode != ShuffleMode::Off,
            shuffle_mode: play_mode.shuffle_mode,
            order: Vec::new(),
            last_auto_dj_seed: None,
            recent_tracks: std::collections::VecDeque::new(),
            queue_undo: None,
            sleep_at: None,
            sleep_end_of_track: false,
            sleep_fade_duration: Duration::from_secs(30),
            sleep_fade_item: None,
            sink: None,
            current_fade: None,
            pending_fade: None,
            current_tap: None,
            pending_tap: None,
            current_download: None,
            pending_download: None,
            pending_play_session: None,
            fading_out: None,
            last_output_rebuild: Instant::now(),
            output_failed_pending: false,
            output_alive: true,
            output_dead,
            queue: Vec::new(),
            queue_revision: 0,
            persisted_queue: None,
            index: 0,
            status: PlaybackStatus::Idle,
            volume,
            generation: 0,
            prefetch_generation: 0,
            prefetched: None,
            prefetch_inflight: None,
            appended: None,
            pending_pause: false,
            queue_ended: false,
            position_offset_ms: 0,
            play_session_id: None,
            ticks_since_report: 0,
            listen: scrobble::ListenClock::default(),
            cover_dir,
        })
    }

    fn handle(&mut self, cmd: PlayerCommand) {
        // The audio may have crossed a track boundary since the last tick;
        // resolve it first so every command operates on a consistent
        // index/position pair (otherwise Prev double-skips, Pause freezes the
        // stale track for the whole pause, etc.).
        self.maybe_transition();
        match cmd {
            PlayerCommand::PlayQueue {
                tracks,
                start_index,
            } => {
                self.queue_undo = None;
                self.report_stopped_playing();
                *self.queue_mut() = tracks;
                self.index = start_index.min(self.queue.len().saturating_sub(1));
                // A fresh playthrough re-arms Auto-DJ.
                self.last_auto_dj_seed = None;
                if self.shuffle {
                    self.rebuild_order();
                }
                self.sync_queue();
                self.start_current();
            }
            PlayerCommand::RestoreQueue { tracks, index } => {
                self.queue_undo = None;
                *self.queue_mut() = tracks;
                self.index = index.min(self.queue.len().saturating_sub(1));
                self.status = PlaybackStatus::Idle;
                if self.shuffle {
                    self.rebuild_order();
                }
                self.sync_queue();
                self.emit();
            }
            PlayerCommand::PlayNext(tracks) => {
                let at = if self.queue.is_empty() {
                    0
                } else {
                    (self.index + 1).min(self.queue.len())
                };
                let n = tracks.len();
                if n > 0 {
                    self.queue_undo = None;
                }
                self.queue_mut().splice(at..at, tracks);
                if self.shuffle && n > 0 {
                    // Shift indices at/after the insertion up, then slot the new
                    // ones right after the current track in play order.
                    for v in self.order.iter_mut() {
                        if *v >= at {
                            *v += n;
                        }
                    }
                    let pos = self
                        .order
                        .iter()
                        .position(|&i| i == self.index)
                        .map(|p| p + 1)
                        .unwrap_or(self.order.len());
                    self.order.splice(pos..pos, at..at + n);
                }
                self.sync_queue();
                self.emit();
            }
            PlayerCommand::PlayLast(tracks) => {
                let start = self.queue.len();
                if !tracks.is_empty() {
                    self.queue_undo = None;
                }
                self.queue_mut().extend(tracks);
                if self.shuffle {
                    self.order.extend(start..self.queue.len());
                }
                self.sync_queue();
                self.emit();
            }
            PlayerCommand::RemoveAt { index, item_id } => self.remove_at(index, &item_id),
            PlayerCommand::JumpTo(i) => {
                if i < self.queue.len() {
                    self.queue_undo = None;
                    self.report_stopped_playing();
                    self.index = i;
                    self.last_auto_dj_seed = None;
                    self.sync_queue();
                    self.start_current();
                }
            }
            PlayerCommand::MoveTrack { from, to } => self.move_track(from, to),
            PlayerCommand::ClearQueue => self.clear_queue(),
            PlayerCommand::ResetQueue => self.reset_queue(),
            PlayerCommand::RemovePlayed => self.remove_played(),
            PlayerCommand::RemoveDuplicates => self.remove_duplicates(),
            PlayerCommand::UndoQueue => self.undo_queue(),
            PlayerCommand::SetShuffleMode(mode) => {
                if mode != self.shuffle_mode {
                    self.queue_undo = None;
                    self.shuffle_mode = mode;
                    self.shuffle = mode != ShuffleMode::Off;
                    if self.shuffle {
                        self.rebuild_order();
                    } else {
                        self.order.clear();
                    }
                    self.persist_play_mode();
                    self.invalidate_next();
                    self.sync_queue();
                    self.emit();
                }
            }
            PlayerCommand::SetRepeat(mode) => {
                if mode != self.repeat {
                    let one_involved = self.repeat == RepeatMode::One || mode == RepeatMode::One;
                    self.repeat = mode;
                    self.persist_play_mode();
                    // Repeat-One toggles whether the next track is gapless-appended.
                    if one_involved {
                        self.invalidate_next();
                    }
                    self.emit();
                }
            }
            PlayerCommand::Play => {
                self.pending_pause = false;
                self.resume_or_start();
            }
            PlayerCommand::Pause => {
                if self.status == PlaybackStatus::Loading {
                    self.pending_pause = true;
                } else {
                    self.pause();
                }
            }
            PlayerCommand::Toggle => match self.status {
                PlaybackStatus::Playing => self.pause(),
                PlaybackStatus::Paused | PlaybackStatus::Idle => self.resume_or_start(),
                PlaybackStatus::Loading => self.pending_pause = !self.pending_pause,
            },
            PlayerCommand::Next => self.advance(1),
            PlayerCommand::Prev => {
                // Standard behavior: restart the track unless we are near its
                // beginning, then go to the previous one.
                if self.position_ms() > 3_000 {
                    self.seek_ms(0);
                } else {
                    self.advance(-1);
                }
            }
            PlayerCommand::Seek { position_ms } => self.seek_ms(position_ms),
            PlayerCommand::SetVolume { volume } => {
                if !volume.is_finite() {
                    return;
                }
                self.volume = volume.clamp(0.0, 1.0);
                if let Some(sink) = &self.sink {
                    sink.set_volume(self.volume);
                }
                if let Some((old, _, _)) = &self.fading_out {
                    old.set_volume(self.volume);
                }
                let _ = self.store.set(keys::VOLUME, &self.volume.to_string());
                self.emit();
            }
            PlayerCommand::SetPlaybackSettings(settings) => {
                let old_device = self.playback.output_device.clone();
                let device_changed = settings.output_device != old_device;
                self.playback = settings;
                if device_changed && !self.try_rebuild_output(self.playback.output_device.clone()) {
                    // Keep playing on the still-healthy old output and revert
                    // the persisted choice so config and reality agree.
                    self.playback.output_device = old_device;
                    self.persist_playback();
                }
            }
            PlayerCommand::OutputFailed => self.handle_output_failed(),
            PlayerCommand::AutoDjResult {
                request_track,
                reason,
                tracks,
            } => self.handle_auto_dj_result(request_track, reason, tracks),
            PlayerCommand::SetExtrasSettings(settings) => {
                self.extras = settings;
                let keep = self.extras.auto_dj_recent_tracks.clamp(1, 500) as usize;
                while self.recent_tracks.len() > keep {
                    self.recent_tracks.pop_front();
                }
            }
            PlayerCommand::SetSleepTimer {
                minutes,
                end_of_track,
                fade_seconds,
            } => {
                self.cancel_sleep_fade();
                // (None, false) clears; (None, true) = at the end of the
                // current track; (Some(m), eot) = after m minutes (then at
                // the next boundary when eot).
                self.sleep_at =
                    minutes.map(|min| Instant::now() + Duration::from_secs(u64::from(min) * 60));
                self.sleep_end_of_track = end_of_track;
                self.sleep_fade_duration = Duration::from_secs(u64::from(match fade_seconds {
                    10 | 30 | 60 => fade_seconds,
                    _ => 30,
                }));
                self.update_sleep_fade();
                self.emit();
            }
            PlayerCommand::Stop => {
                self.report_stopped_playing();
                self.stop_internal();
            }
            PlayerCommand::Shutdown { ack } => {
                // The worker loop intercepts this so it can also break out;
                // this arm keeps the match exhaustive and stays correct.
                self.shutdown();
                let _ = ack.send(());
            }
            PlayerCommand::StopAck { ack } => {
                // Sign-out: the session is torn down the moment this returns,
                // so the report has to go out now rather than as a spawned
                // task that would find no client left.
                self.report_stopped_blocking(true);
                self.stop_internal();
                let _ = ack.send(());
            }
            PlayerCommand::RefreshSmtcArtwork { item_id } => {
                if let Some(track) = self.queue.get(self.index).cloned() {
                    if track.item_id == item_id {
                        self.update_media_metadata(&track);
                    }
                }
            }
            PlayerCommand::SourceReady {
                generation,
                prefetch_generation,
                purpose,
                track,
                play_session,
                result,
            } => self.source_ready(
                generation,
                prefetch_generation,
                purpose,
                *track,
                play_session,
                result,
            ),
        }
    }

    fn source_ready(
        &mut self,
        generation: u64,
        prefetch_generation: u64,
        purpose: OpenPurpose,
        track: QueueTrack,
        play_session: String,
        result: AppResult<Box<OpenedSource>>,
    ) {
        if generation != self.generation {
            return; // stale open from before a queue change
        }
        match purpose {
            OpenPurpose::Play => {
                if self.queue.get(self.index).map(|t| t.item_id.as_str())
                    != Some(track.item_id.as_str())
                {
                    return;
                }
                match result {
                    Ok(opened) => {
                        let OpenedSource {
                            source,
                            fade,
                            tap_enabled,
                            download,
                            ..
                        } = *opened;
                        let sink = rodio::Player::connect_new(self.output.mixer());
                        sink.set_volume(self.volume);
                        sink.append(source);
                        // Honor a pause pressed while this source was loading.
                        if self.pending_pause {
                            self.pending_pause = false;
                            sink.pause();
                            self.status = PlaybackStatus::Paused;
                        } else {
                            self.status = PlaybackStatus::Playing;
                        }
                        self.sink = Some(sink);
                        self.current_fade = Some(fade);
                        self.current_tap = Some(tap_enabled);
                        self.current_download = Some(download);
                        self.position_offset_ms = 0;
                        self.ticks_since_report = 0;
                        self.report_start(&track, play_session);
                        if self.status == PlaybackStatus::Paused {
                            self.report_progress(true);
                        }
                        self.update_media_metadata(&track);
                        self.update_media_playback();
                        self.emit();
                    }
                    Err(e) => self.fail_start(&track, e),
                }
            }
            OpenPurpose::SeekRestart { .. } => {
                if self.queue.get(self.index).map(|t| t.item_id.as_str())
                    != Some(track.item_id.as_str())
                {
                    return;
                }
                match result {
                    Ok(opened) => {
                        let OpenedSource {
                            source,
                            fade,
                            tap_enabled,
                            download,
                            start_ms,
                        } = *opened;
                        let sink = rodio::Player::connect_new(self.output.mixer());
                        sink.set_volume(self.volume);
                        sink.append(source);
                        // Paused before the reopen, as adjusted by Play/Pause
                        // pressed while it loaded (see restart_at).
                        let paused = std::mem::take(&mut self.pending_pause);
                        if paused {
                            sink.pause();
                            self.status = PlaybackStatus::Paused;
                        } else {
                            self.status = PlaybackStatus::Playing;
                        }
                        self.sink = Some(sink);
                        self.current_fade = Some(fade);
                        self.current_tap = Some(tap_enabled);
                        self.current_download = Some(download);
                        self.position_offset_ms = start_ms;
                        self.listen.rebase(Instant::now(), start_ms);
                        // Same play session continues — no new start report.
                        self.report_progress(paused);
                        self.update_media_playback();
                        self.emit();
                    }
                    Err(e) => {
                        // This track's start was reported: close its session.
                        self.report_stopped_playing();
                        self.fail_start(&track, e);
                    }
                }
            }
            OpenPurpose::Prefetch => {
                if prefetch_generation != self.prefetch_generation {
                    return; // opened before a queue edit re-decided the next track
                }
                if self.prefetch_inflight.as_deref() == Some(track.item_id.as_str()) {
                    self.prefetch_inflight = None;
                }
                match result {
                    Ok(opened) => {
                        let OpenedSource {
                            source,
                            fade,
                            tap_enabled,
                            download,
                            ..
                        } = *opened;
                        self.prefetched = Some(Prefetched {
                            item_id: track.item_id,
                            play_session,
                            source,
                            fade,
                            tap_enabled,
                            download,
                        });
                    }
                    Err(e) => {
                        // Not fatal: the normal advance path retries as a
                        // fresh Play open when the track actually starts.
                        tracing::warn!("prefetch failed: {e}");
                    }
                }
            }
        }
    }

    /// Detect and resolve a crossed track boundary (gapless transition or
    /// plain end-of-track). Returns true when the player state changed.
    /// Slice view of the active play order (empty when not shuffling → linear).
    fn play_order(&self) -> &[usize] {
        if self.shuffle {
            &self.order
        } else {
            &[]
        }
    }

    /// Next track for the Next button / sequential advance. Honors shuffle order
    /// and Repeat-All wrap; ignores Repeat-One (Next must still move on).
    fn next_index(&self) -> Option<usize> {
        order_next(
            self.queue.len(),
            self.index,
            self.play_order(),
            self.repeat == RepeatMode::All,
        )
    }

    fn prev_index(&self) -> Option<usize> {
        order_prev(
            self.queue.len(),
            self.index,
            self.play_order(),
            self.repeat == RepeatMode::All,
        )
    }

    /// What auto-advance (track end / gapless) should play: Repeat-One replays
    /// the current track, otherwise the sequential next.
    fn auto_next_index(&self) -> Option<usize> {
        if self.repeat == RepeatMode::One {
            (!self.queue.is_empty()).then_some(self.index)
        } else {
            self.next_index()
        }
    }

    fn rebuild_order(&mut self) {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            ^ self.generation.wrapping_mul(0x2545_F491_4F6C_DD1D);
        self.order = match self.shuffle_mode {
            ShuffleMode::Off => Vec::new(),
            ShuffleMode::Tracks => build_shuffle_order(self.queue.len(), self.index, seed),
            ShuffleMode::Albums => build_album_shuffle_order(&self.queue, self.index, seed),
        };
    }

    /// Drop a not-yet-appended prefetch so the next boundary re-decides. An
    /// already gapless-appended source can't be pulled from the sink; the
    /// transition fallback re-picks the correct next if it no longer matches.
    fn invalidate_next(&mut self) {
        // Not `generation`: that would also drop the open of the current track
        // itself and leave the player in Loading.
        self.prefetch_generation = self.prefetch_generation.wrapping_add(1);
        self.prefetched = None;
        self.prefetch_inflight = None;
    }

    fn persist_play_mode(&self) {
        let pm = PlayMode {
            shuffle_mode: self.shuffle_mode,
            repeat: self.repeat,
        };
        if let Ok(json) = serde_json::to_string(&pm) {
            let _ = self.store.set(keys::PLAY_MODE, &json);
        }
    }

    fn maybe_transition(&mut self) -> bool {
        if self.status != PlaybackStatus::Playing {
            return false;
        }
        let Some(sink_len) = self.sink.as_ref().map(|s| s.len()) else {
            return false;
        };
        if let Some(item_id) = self.appended.clone() {
            if sink_len <= 1 {
                self.handle_transition(&item_id);
                return true;
            }
        } else if sink_len == 0 {
            self.handle_track_end();
            return true;
        }
        false
    }

    /// Periodic tick while playing: position updates, throttled progress
    /// reporting, prefetch scheduling, the gapless hand-off, and end-of-track
    /// bookkeeping.
    fn tick(&mut self) {
        // A deferred output failure is retried once the debounce has passed.
        if self.output_failed_pending && self.last_output_rebuild.elapsed() > Duration::from_secs(2)
        {
            self.handle_output_failed();
        }
        // Crossfade tails are dropped once their ramp is over.
        if let Some((_, _, drop_at)) = &self.fading_out {
            if Instant::now() >= *drop_at {
                self.fading_out = None;
            }
        }
        if self.maybe_transition() || self.status != PlaybackStatus::Playing {
            return;
        }
        if self.sink.is_none() {
            return;
        }
        self.listen.sample(Instant::now(), self.position_ms());

        self.update_sleep_fade();

        // Sleep timer: pause when the deadline passes (unless the user asked
        // to finish the running track first — handled at the boundary).
        if let Some(sleep_at) = self.sleep_at {
            if Instant::now() >= sleep_at && !self.sleep_end_of_track {
                self.sleep_at = None;
                self.sleep_fade_item = None;
                self.pause();
                return;
            }
        }

        self.ticks_since_report += 1;
        if self.ticks_since_report >= REPORT_EVERY_TICKS {
            self.ticks_since_report = 0;
            self.report_progress(false);
        }

        self.schedule_prefetch();
        self.maybe_auto_dj();
        self.update_media_playback();
        self.emit();
    }

    /// Start (or re-apply after pause/source change) the sample-time sleep
    /// ramp. The sink's user volume is untouched, so cancelling can reliably
    /// restore the FadeSource gain to one.
    fn update_sleep_fade(&mut self) {
        if self.status != PlaybackStatus::Playing || self.sleep_fade_item.is_some() {
            return;
        }
        let now = Instant::now();
        let remaining = if self.sleep_end_of_track {
            // A delayed end-of-track timer only starts fading once its wall
            // clock delay has elapsed.
            if self.sleep_at.is_some_and(|at| now < at) {
                return;
            }
            let Some(track) = self.queue.get(self.index) else {
                return;
            };
            if track.duration_ms == 0 {
                return;
            }
            Duration::from_millis(track.duration_ms.saturating_sub(self.position_ms()))
        } else {
            let Some(at) = self.sleep_at else {
                return;
            };
            at.saturating_duration_since(now)
        };
        if remaining.is_zero() || remaining > self.sleep_fade_duration {
            return;
        }
        let Some(fade) = &self.current_fade else {
            return;
        };
        let full = self.sleep_fade_duration.as_secs_f32();
        let gain = if full > 0.0 {
            (remaining.as_secs_f32() / full).clamp(0.0, 1.0)
        } else {
            0.0
        };
        fade.begin(gain, 0.0, remaining);
        self.sleep_fade_item = self
            .queue
            .get(self.index)
            .map(|track| track.item_id.clone());
    }

    fn cancel_sleep_fade(&mut self) {
        if self.sleep_fade_item.take().is_some() {
            if let Some(fade) = &self.current_fade {
                fade.ramp_to(1.0, Duration::from_millis(PAUSE_FADE_MS));
            }
        }
    }

    /// End-of-track sleep: fires at a boundary once the deadline (if any)
    /// has passed. Returns true when playback was put to sleep.
    fn sleep_at_boundary(&mut self) -> bool {
        if !self.sleep_end_of_track {
            return false;
        }
        let due = self.sleep_at.is_none_or(|at| Instant::now() >= at);
        if due {
            self.sleep_at = None;
            self.sleep_end_of_track = false;
            self.sleep_fade_item = None;
            self.pause();
        }
        due
    }

    /// Open the next track ahead of time and append it to the sink just
    /// before the boundary.
    fn schedule_prefetch(&mut self) {
        // A pending end-of-track sleep must reach a real track end: no
        // gapless append, no crossfade.
        if self.sleep_end_of_track || self.sleep_fade_item.is_some() {
            return;
        }
        // Repeat-One (and a single-track wrap) loops via handle_track_end, not a
        // gapless append of the same source — the identity guard can't
        // disambiguate a same-item_id "next".
        let next_idx = self.auto_next_index();
        if self.repeat == RepeatMode::One || next_idx == Some(self.index) {
            return;
        }
        let Some(current) = self.queue.get(self.index).cloned() else {
            return;
        };
        let duration = current.duration_ms;
        let remaining = duration.saturating_sub(self.position_ms());
        let next = next_idx.and_then(|i| self.queue.get(i).cloned());

        // Drop a prefetched source that no longer matches the upcoming track
        // (queue was edited since it was opened).
        if let Some(p) = &self.prefetched {
            if next.as_ref().map(|t| t.item_id.as_str()) != Some(p.item_id.as_str()) {
                self.prefetched = None;
            }
        }

        let Some(next) = next else {
            return;
        };

        if should_start_prefetch(
            self.prefetched.is_some() || self.prefetch_inflight.is_some(),
            self.appended.as_deref(),
            &next.item_id,
            duration,
            remaining,
        ) {
            self.spawn_open(OpenPurpose::Prefetch, next.clone());
        }

        let requested_fade_ms = (self.playback.crossfade_seconds.clamp(1.0, 12.0) * 1000.0) as u64;
        let effective_fade_ms = if should_crossfade(
            self.playback.crossfade_mode,
            requested_fade_ms,
            &current,
            &next,
        ) {
            requested_fade_ms
        } else {
            0
        };
        if effective_fade_ms >= 500 {
            // Crossfade: overlap the tracks on two players instead of the
            // gapless same-sink hand-off.
            if remaining > 0 && remaining <= effective_fade_ms {
                if let Some(p) = self.prefetched.take() {
                    if p.item_id == next.item_id {
                        self.begin_crossfade(p, remaining.min(effective_fade_ms));
                    } else {
                        self.prefetched = Some(p);
                    }
                }
            }
        } else if self.appended.is_none() && duration > 0 && remaining <= APPEND_REMAINING_MS {
            if let Some(p) = self.prefetched.take() {
                if p.item_id == next.item_id {
                    if let Some(sink) = &self.sink {
                        sink.append(p.source);
                        self.pending_fade = Some(p.fade);
                        self.pending_tap = Some(p.tap_enabled);
                        self.pending_download = Some(p.download);
                        self.pending_play_session = Some(p.play_session);
                        self.appended = Some(p.item_id);
                    }
                } else {
                    self.prefetched = Some(p);
                }
            }
        }
    }

    /// Fade the current track out while the next fades in on a fresh player;
    /// the queue pointer moves immediately (the new track is what's "current").
    fn begin_crossfade(&mut self, p: Prefetched, fade_ms: u64) {
        let play_session = p.play_session;
        if let Some(prev) = self.queue.get(self.index).cloned() {
            let position_ms = self.position_ms();
            self.report_stopped(&prev, position_ms);
        }
        // The tail must stay steerable (pause/seek during the fade) and stop
        // feeding the visualizer — only one track drives the visuals.
        if let Some(tap) = self.current_tap.take() {
            tap.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        if let (Some(fade), Some(old)) = (self.current_fade.take(), self.sink.take()) {
            fade.ramp_to(0.0, Duration::from_millis(fade_ms));
            self.fading_out = Some((
                old,
                fade,
                Instant::now() + Duration::from_millis(fade_ms + 500),
            ));
        }
        // A next source already gapless-appended to the old player (the queue
        // changed after the hand-off) would start at full volume under the
        // new track: silence it from its first sample.
        if let Some(fade) = self.pending_fade.take() {
            fade.begin(0.0, 0.0, Duration::ZERO);
        }
        if let Some(tap) = self.pending_tap.take() {
            tap.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        self.pending_download = None;
        self.pending_play_session = None;

        let sink = rodio::Player::connect_new(self.output.mixer());
        sink.set_volume(self.volume);
        p.fade.begin(0.0, 1.0, Duration::from_millis(fade_ms));
        sink.append(p.source);
        self.sink = Some(sink);
        self.current_fade = Some(p.fade);
        self.sleep_fade_item = None;
        self.current_tap = Some(p.tap_enabled);
        self.current_download = Some(p.download);
        self.position_offset_ms = 0;
        self.listen.restart(Instant::now(), 0);
        self.appended = None;

        self.queue_undo = None;
        self.index = self.auto_next_index().unwrap_or(self.index);
        self.sync_queue();
        if let Some(track) = self.queue.get(self.index).cloned() {
            self.ticks_since_report = 0;
            self.report_start(&track, play_session);
            self.update_media_metadata(&track);
        }
        self.update_media_playback();
        self.emit();
    }

    /// The appended source is now playing: move the queue pointer, report the
    /// finished track, start reporting the new one. The hand-off is honored
    /// only when the appended track is still what the queue says comes next;
    /// otherwise the queue order wins.
    fn handle_transition(&mut self, appended_item_id: &str) {
        self.appended = None;
        if let Some(prev) = self.queue.get(self.index).cloned() {
            self.report_stopped(&prev, prev.duration_ms);
        }
        let invalidated_undo = self.queue_undo.take().is_some();
        let target = self.auto_next_index();
        let next_matches = target
            .and_then(|i| self.queue.get(i))
            .is_some_and(|t| t.item_id == appended_item_id);
        if next_matches {
            self.index = target.unwrap();
            // The appended source plays its track from the very beginning.
            self.position_offset_ms = 0;
            self.listen.restart(Instant::now(), self.position_ms());
            self.current_fade = self.pending_fade.take();
            self.sleep_fade_item = None;
            self.current_tap = self.pending_tap.take();
            self.current_download = self.pending_download.take();
            self.sync_queue();
            if let Some(track) = self.queue.get(self.index).cloned() {
                self.ticks_since_report = 0;
                let play_session = self
                    .pending_play_session
                    .take()
                    .unwrap_or_else(new_play_session);
                self.report_start(&track, play_session);
                self.update_media_metadata(&track);
            }
            self.update_media_playback();
            self.emit();
            // "Sleep at end of track" armed while the next was already
            // appended: pause right at the start of the new track.
            self.sleep_at_boundary();
        } else {
            // The queue was edited after the hand-off (next track removed,
            // moved, or something inserted before it): the appended audio no
            // longer matches the play order. The finished track is still at
            // self.index, so cut over to whatever now follows it.
            if let Some(n) = self.auto_next_index() {
                self.index = n;
                self.sync_queue();
                self.start_current();
            } else {
                if invalidated_undo {
                    self.sync_queue();
                }
                self.stop_internal();
                self.queue_ended = true;
            }
        }
    }

    /// Current track finished with nothing appended (prefetch missed or was
    /// disabled): fall back to a regular start of the next track.
    fn handle_track_end(&mut self) {
        if let Some(prev) = self.queue.get(self.index).cloned() {
            self.report_stopped(&prev, prev.duration_ms);
        }
        let invalidated_undo = self.queue_undo.take().is_some();
        // Sleep timer with "finish the track": stop here, positioned at the
        // next track so resume continues naturally.
        if self.sleep_end_of_track && self.sleep_at.is_none_or(|at| Instant::now() >= at) {
            self.sleep_at = None;
            self.sleep_end_of_track = false;
            self.sleep_fade_item = None;
            // Position at the next track so resume continues naturally (but not
            // onto itself under Repeat-One).
            if let Some(n) = self.next_index() {
                self.index = n;
                self.sync_queue();
            } else if invalidated_undo {
                self.sync_queue();
            }
            self.stop_internal();
            return;
        }
        // Repeat-One / single-track wrap: auto_next_index() == current → restart.
        if let Some(n) = self.auto_next_index() {
            self.index = n;
            self.sync_queue();
            // Reuse a matching prefetched source to keep the gap minimal.
            if !self.start_from_prefetched() {
                self.start_current();
            }
        } else {
            if invalidated_undo {
                self.sync_queue();
            }
            self.stop_internal();
            self.queue_ended = true;
        }
    }

    /// Start the current queue track from an already-opened prefetched source.
    fn start_from_prefetched(&mut self) -> bool {
        let Some(current_id) = self.queue.get(self.index).map(|t| t.item_id.clone()) else {
            return false;
        };
        let Some(p) = self.prefetched.take() else {
            return false;
        };
        if p.item_id != current_id {
            self.prefetched = Some(p);
            return false;
        }
        let Some(track) = self.queue.get(self.index).cloned() else {
            return false;
        };
        self.generation = self.generation.wrapping_add(1);
        self.prefetch_inflight = None;
        self.appended = None;
        self.pending_fade = None;
        self.pending_tap = None;
        self.pending_download = None;
        self.pending_play_session = None;
        self.fading_out = None;
        let sink = rodio::Player::connect_new(self.output.mixer());
        sink.set_volume(self.volume);
        sink.append(p.source);
        self.sink = Some(sink);
        self.current_fade = Some(p.fade);
        self.current_tap = Some(p.tap_enabled);
        self.current_download = Some(p.download);
        self.position_offset_ms = 0;
        self.listen.restart(Instant::now(), 0);
        self.status = PlaybackStatus::Playing;
        self.ticks_since_report = 0;
        self.report_start(&track, p.play_session);
        self.update_media_metadata(&track);
        self.update_media_playback();
        self.emit();
        true
    }

    fn pause(&mut self) {
        if self.status != PlaybackStatus::Playing {
            return;
        }
        // Ramp everything audible to silence before pausing so there is no
        // click: the current source, a gapless-appended next source (the
        // boundary may be crossed during the sleep), and a crossfade tail.
        // Blocking the command loop for the ramp is fine: commands queue.
        if self.sink.is_some() {
            if let Some(fade) = &self.current_fade {
                fade.ramp_to(0.0, Duration::from_millis(PAUSE_FADE_MS));
            }
            if let Some(fade) = &self.pending_fade {
                fade.ramp_to(0.0, Duration::from_millis(PAUSE_FADE_MS));
            }
            if let Some((_, fade, _)) = &self.fading_out {
                fade.ramp_to(0.0, Duration::from_millis(PAUSE_FADE_MS));
            }
            std::thread::sleep(Duration::from_millis(PAUSE_FADE_MS + 20));
            // The track boundary may have been crossed while we slept;
            // resolve it so index/position/fade handles are current.
            self.maybe_transition();
            match self.status {
                PlaybackStatus::Playing => {}
                PlaybackStatus::Loading => {
                    // Transition cut over to a fresh open: pause it on arrival.
                    self.pending_pause = true;
                    return;
                }
                _ => return,
            }
        }
        self.fading_out = None;
        if let Some(sink) = &self.sink {
            sink.pause();
        }
        self.status = PlaybackStatus::Paused;
        self.report_progress(true);
        self.update_media_playback();
        self.emit();
    }

    fn resume_or_start(&mut self) {
        match self.status {
            PlaybackStatus::Paused => {
                if !self.sleep_end_of_track && self.sleep_at.is_some_and(|at| Instant::now() >= at)
                {
                    self.sleep_at = None;
                    self.cancel_sleep_fade();
                    self.emit();
                    return;
                }
                if let Some(sink) = &self.sink {
                    sink.play();
                }
                if let Some(fade) = &self.current_fade {
                    fade.ramp_to(1.0, Duration::from_millis(PAUSE_FADE_MS));
                }
                // The appended next source was muted by pause() too.
                if let Some(fade) = &self.pending_fade {
                    fade.ramp_to(1.0, Duration::from_millis(PAUSE_FADE_MS));
                }
                self.status = PlaybackStatus::Playing;
                // pause() temporarily replaced the sleep ramp with its
                // click-free pause ramp. Recompute the right gain/remaining
                // time from the timer instead of resuming at full volume.
                self.sleep_fade_item = None;
                self.update_sleep_fade();
                self.report_progress(false);
                self.update_media_playback();
                self.emit();
            }
            PlaybackStatus::Idle => {
                // Nothing loaded in the sink yet — start the current queue
                // position (e.g. after a restored session).
                if !self.queue.is_empty() {
                    self.start_current();
                }
            }
            PlaybackStatus::Playing | PlaybackStatus::Loading => {}
        }
    }

    fn advance(&mut self, delta: i64) {
        let target = if delta >= 0 {
            self.next_index()
        } else {
            self.prev_index()
        };
        let Some(target) = target else {
            return;
        };
        self.queue_undo = None;
        self.report_stopped_playing();
        self.index = target;
        self.sync_queue();
        self.start_current();
    }

    fn remove_at(&mut self, i: usize, item_id: &str) {
        if self.queue.get(i).map(|t| t.item_id.as_str()) != Some(item_id) {
            return;
        }
        self.remember_queue_undo();
        self.invalidate_next();
        let removing_current = i == self.index;
        // Where playback continues if we remove the current track: the play-
        // order successor (under shuffle that's not the physical neighbor),
        // captured before the mutation reindexes everything.
        let successor = if removing_current && self.status == PlaybackStatus::Playing {
            self.next_index()
        } else {
            None
        };
        if removing_current {
            self.report_stopped_playing();
            self.sink = None;
        }
        self.queue_mut().remove(i);
        if self.shuffle {
            self.order.retain(|&v| v != i);
            for v in self.order.iter_mut() {
                if *v > i {
                    *v -= 1;
                }
            }
        }
        if i < self.index {
            self.index -= 1;
        }
        self.index = self.index.min(self.queue.len().saturating_sub(1));
        if removing_current {
            // Remap the successor through the removal shift and continue there,
            // instead of whatever physical slot clamping landed on.
            if let Some(t) = successor.filter(|_| !self.queue.is_empty()) {
                self.index = if t > i { t - 1 } else { t };
                self.sync_queue();
                self.start_current();
                return;
            }
            self.status = PlaybackStatus::Idle;
            self.update_media_playback();
        }
        self.sync_queue();
        self.emit();
    }

    /// Move the entry at play-order position `from` to `to` (remove, then
    /// insert) — the order the queue panel shows. Under shuffle that reorders
    /// the shuffle order only: the stored queue keeps its order, which comes
    /// back when shuffle is turned off. Otherwise the stored entry moves.
    fn move_track(&mut self, from: usize, to: usize) {
        let len = self.queue.len();
        if from >= len || to >= len || from == to {
            return;
        }
        self.queue_undo = None;
        if self.shuffle && self.order.len() == len {
            let entry = self.order.remove(from);
            self.order.insert(to, entry);
            self.sync_queue();
            self.emit();
            return;
        }
        let queue = self.queue_mut();
        let track = queue.remove(from);
        queue.insert(to, track);
        if self.shuffle {
            for v in self.order.iter_mut() {
                *v = remap_move(*v, from, to);
            }
        }
        // Keep `index` pointing at the same (possibly shifted) current track.
        if from == self.index {
            self.index = to;
        } else if from < self.index && to >= self.index {
            self.index -= 1;
        } else if from > self.index && to <= self.index {
            self.index += 1;
        }
        self.sync_queue();
        self.emit();
    }

    fn remember_queue_undo(&mut self) {
        self.queue_undo = Some(QueueUndo {
            tracks: self.queue.clone(),
            index: self.index,
            order: self.order.clone(),
            last_auto_dj_seed: self.last_auto_dj_seed.clone(),
            status: self.status,
        });
    }

    /// Sign-out / server change: everything about the previous session goes,
    /// including the undo snapshot and the recently-played memory.
    fn reset_queue(&mut self) {
        self.stop_internal();
        self.queue_mut().clear();
        self.index = 0;
        self.order.clear();
        self.last_auto_dj_seed = None;
        self.queue_undo = None;
        self.recent_tracks.clear();
        self.sync_queue();
        self.emit();
    }

    fn clear_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.remember_queue_undo();
        self.report_stopped_playing();
        self.stop_internal();
        self.queue_mut().clear();
        self.index = 0;
        self.order.clear();
        self.last_auto_dj_seed = None;
        self.sync_queue();
        self.emit();
    }

    fn remove_played(&mut self) {
        let order = if self.shuffle { &self.order[..] } else { &[] };
        let indices = played_indices(self.queue.len(), self.index, order);
        self.remove_indices(indices);
    }

    fn remove_duplicates(&mut self) {
        let indices = duplicate_indices(&self.queue, self.index);
        self.remove_indices(indices);
    }

    /// Remove non-current tracks in one atomic mutation, preserving current
    /// playback and remapping both the physical index and shuffle order.
    fn remove_indices(&mut self, mut indices: Vec<usize>) {
        indices.retain(|&index| index < self.queue.len() && index != self.index);
        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty() {
            return;
        }

        self.remember_queue_undo();
        self.invalidate_next();
        let removed: std::collections::HashSet<usize> = indices.iter().copied().collect();
        let queue = self.queue_mut();
        *queue = std::mem::take(queue)
            .into_iter()
            .enumerate()
            .filter_map(|(index, track)| (!removed.contains(&index)).then_some(track))
            .collect();
        if self.shuffle {
            self.order = remap_order_after_removals(&self.order, &indices);
        }
        self.index = remap_after_removals(self.index, &indices).unwrap_or(0);
        self.sync_queue();
        self.emit();
    }

    fn undo_queue(&mut self) {
        let Some(undo) = self.queue_undo.take() else {
            return;
        };
        let live_current = self
            .queue
            .get(self.index)
            .map(|track| track.item_id.as_str());
        let restored_current = undo
            .tracks
            .get(undo.index)
            .map(|track| track.item_id.as_str());
        let restart = live_current != restored_current
            || (self.sink.is_none()
                && matches!(
                    undo.status,
                    PlaybackStatus::Playing | PlaybackStatus::Paused | PlaybackStatus::Loading
                ));

        if restart {
            if self.status != PlaybackStatus::Idle {
                self.report_stopped_playing();
            }
            self.stop_internal();
        }
        *self.queue_mut() = undo.tracks;
        self.index = undo.index.min(self.queue.len().saturating_sub(1));
        self.order = if self.shuffle { undo.order } else { Vec::new() };
        self.last_auto_dj_seed = undo.last_auto_dj_seed;
        self.invalidate_next();
        self.sync_queue();

        if restart && !self.queue.is_empty() {
            match undo.status {
                PlaybackStatus::Playing | PlaybackStatus::Loading => self.start_current(),
                PlaybackStatus::Paused => {
                    self.start_current();
                    self.pending_pause = true;
                }
                PlaybackStatus::Idle => {
                    self.status = PlaybackStatus::Idle;
                    self.emit();
                }
            }
        } else {
            self.emit();
        }
    }

    /// Stop current playback and asynchronously open the track at the current
    /// queue index. Never blocks the command loop.
    fn start_current(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.appended = None;
        self.pending_pause = false;
        self.queue_ended = false;
        self.pending_fade = None;
        self.current_fade = None;
        self.sleep_fade_item = None;
        self.pending_tap = None;
        self.current_tap = None;
        self.pending_download = None;
        self.current_download = None;
        self.fading_out = None;
        self.position_offset_ms = 0;
        self.listen.restart(Instant::now(), 0);
        self.sink = None;
        // The output may have died while idle; bring it back before playing.
        if !self.output_alive {
            // Once a requested device has failed, stay on the default output
            // until the user explicitly changes the setting. Reappearance
            // alone must never steal playback back to the old device.
            let requested_device = if crate::audio_devices::fallback_device().is_some() {
                None
            } else {
                self.playback.output_device.clone()
            };
            let reopened = open_output(&requested_device, self.tx.clone())
                .or_else(|_| open_output(&None, self.tx.clone()));
            match reopened {
                Ok((output, dead)) => {
                    self.output = output;
                    self.output_dead = dead;
                    self.output_alive = true;
                    self.last_output_rebuild = Instant::now();
                }
                Err(e) => {
                    let _ = self
                        .app
                        .emit("player:error", format!("Audio output failed: {e}"));
                    self.status = PlaybackStatus::Idle;
                    self.emit();
                    return;
                }
            }
        }
        let Some(track) = self.queue.get(self.index).cloned() else {
            self.stop_internal();
            return;
        };
        self.status = PlaybackStatus::Loading;
        self.emit();
        self.spawn_open(OpenPurpose::Play, track);
    }

    fn stop_internal(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.appended = None;
        self.pending_pause = false;
        self.queue_ended = false;
        self.pending_fade = None;
        self.current_fade = None;
        self.pending_tap = None;
        self.current_tap = None;
        self.pending_download = None;
        self.current_download = None;
        self.fading_out = None;
        self.position_offset_ms = 0;
        // Every stop path reports first, so nothing measured is lost here.
        self.listen = scrobble::ListenClock::default();
        self.sink = None;
        self.status = PlaybackStatus::Idle;
        self.update_media_playback();
        self.emit();
    }

    fn fail_start(&mut self, track: &QueueTrack, e: AppError) {
        // No title in the log: the diagnostics export promises no track
        // metadata, and its quote redaction trips over apostrophes.
        tracing::error!("failed to start a track: {e}");
        let _ = self.app.emit(
            "player:error",
            format!("Cannot play \"{}\": {e}", track.name),
        );
        self.sink = None;
        self.status = PlaybackStatus::Idle;
        self.update_media_playback();
        self.emit();
    }

    /// Open a track's stream + decoder on a short-lived background thread and
    /// deliver the result back through the command channel.
    fn spawn_open(&mut self, purpose: OpenPurpose, track: QueueTrack) {
        let Some(client) = self.session_client() else {
            if matches!(purpose, OpenPurpose::Play) {
                self.fail_start(&track, AppError::NotConnected);
            }
            return;
        };
        // One play session per *attempt* at a track, not per queue line. The
        // server takes the id as the identity of a playback: it ties the
        // /Sessions/Playing* reports to the transcode job it started for this
        // stream URL, and once we report a session stopped, that playback is
        // over as far as the server is concerned. Playing the same entry again
        // -- repeat, jumping back, a queue restored from the store -- has to
        // be a new one. A reopen (seek inside a transcode, an output-device
        // switch) is the same attempt and keeps its session.
        let play_session = match purpose {
            OpenPurpose::SeekRestart { .. } => self
                .play_session_id
                .clone()
                .unwrap_or_else(new_play_session),
            OpenPurpose::Play | OpenPurpose::Prefetch => new_play_session(),
        };
        if matches!(purpose, OpenPurpose::Prefetch) {
            self.prefetch_inflight = Some(track.item_id.clone());
        }
        let start_ms = match purpose {
            OpenPurpose::SeekRestart { position_ms } => Some(position_ms),
            _ => None,
        };
        let auth = StreamAuth::from_client(&client);
        let dsp = self.dsp.clone();
        let tap = self.tap.clone();
        let waveform = self.waveform.clone();
        let generation = self.generation;
        let prefetch_generation = self.prefetch_generation;
        let tx = self.tx.clone();
        let session = play_session.clone();
        let spawned = std::thread::Builder::new()
            .name("jellysic-open".into())
            .spawn(move || {
                // A decoder panicking on a malformed file (symphonia on a WAV
                // header with sample rate 0) must still answer, or the player
                // waits in Loading for good.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source::open_track_source(
                        &auth,
                        &track,
                        &session,
                        dsp,
                        tap,
                        start_ms,
                        Some(waveform),
                    )
                }))
                .unwrap_or_else(|_| Err(AppError::Audio("the decoder crashed".into())))
                .map(Box::new);
                let _ = tx.send(PlayerCommand::SourceReady {
                    generation,
                    prefetch_generation,
                    purpose,
                    track: Box::new(track),
                    play_session,
                    result,
                });
            });
        if spawned.is_err() {
            // Nothing will deliver a SourceReady, so a Play open would leave
            // the UI in Loading for good. Surface it like any other start
            // failure instead.
            tracing::error!("failed to spawn open thread");
            self.prefetch_inflight = None;
            if matches!(purpose, OpenPurpose::Play | OpenPurpose::SeekRestart { .. }) {
                let track = self.queue.get(self.index).cloned();
                if let Some(track) = track {
                    self.fail_start(&track, AppError::Audio("cannot start opener thread".into()));
                }
            }
        }
    }

    fn session_client(&self) -> Option<Arc<JellyfinClient>> {
        // Plain OS thread, never inside the async runtime -> blocking is fine.
        self.session.blocking_read().clone()
    }

    /// The active stream died. Debounced retry: a dying device spams errors,
    /// but the one-shot error of a freshly rebuilt stream must not be lost —
    /// it is deferred to a tick instead of dropped.
    fn handle_output_failed(&mut self) {
        if self.last_output_rebuild.elapsed() <= Duration::from_secs(2) {
            self.output_failed_pending = true;
            return;
        }
        self.output_failed_pending = false;
        tracing::warn!("audio output failed; falling back to the default device");
        if self.try_rebuild_output(None) {
            if let Some(requested_device) = self.playback.output_device.clone() {
                // Keep the persisted preference: Settings can show the
                // missing device, while the runtime stays on system default.
                crate::audio_devices::mark_fallback(requested_device.clone());
                let _ = self.app.emit(
                    "audio:output-fallback",
                    crate::audio_devices::AudioOutputFallbackEvent { requested_device },
                );
            }
        } else {
            self.output_alive = false;
            let _ = self.app.emit(
                "player:error",
                "Audio output failed and no fallback device is available.".to_string(),
            );
            // Close the Jellyfin play session like any other stop.
            if self.status != PlaybackStatus::Idle {
                self.report_stopped_playing();
            }
            self.stop_internal();
        }
    }

    /// Swap the audio output and pick playback back up. Returns false (and
    /// keeps the previous output untouched) when the new device cannot open.
    fn try_rebuild_output(&mut self, device: Option<String>) -> bool {
        self.last_output_rebuild = Instant::now();
        match open_output(&device, self.tx.clone()) {
            Ok((output, dead)) => {
                crate::audio_devices::clear_fallback();
                let position_ms = self.position_ms();
                self.output = output;
                self.output_dead = dead;
                self.output_alive = true;
                match self.status {
                    PlaybackStatus::Idle => {
                        self.sink = None;
                        self.current_fade = None;
                        self.pending_fade = None;
                        self.current_download = None;
                        self.pending_download = None;
                        self.fading_out = None;
                    }
                    // An in-flight open belongs to the old output; restart the
                    // track cleanly (loses a pending seek target, but keeps
                    // reporting correct).
                    PlaybackStatus::Loading => self.start_current(),
                    PlaybackStatus::Playing | PlaybackStatus::Paused => {
                        self.restart_at(position_ms)
                    }
                }
                true
            }
            Err(e) => {
                tracing::error!("cannot open audio output: {e}");
                let _ = self
                    .app
                    .emit("player:error", format!("Audio output failed: {e}"));
                false
            }
        }
    }

    fn persist_playback(&self) {
        if let Ok(json) = serde_json::to_string(&self.playback) {
            let _ = self.store.set(keys::PLAYBACK, &json);
        }
    }

    // --- Playback reporting (fire-and-forget; never blocks audio) ---

    /// `session_id` is the play session the source was opened under: it is in
    /// the stream URL the server is serving, so the reports have to name the
    /// same one or the server cannot tie them to its transcode job.
    fn report_start(&mut self, track: &QueueTrack, session_id: String) {
        self.play_session_id = Some(session_id.clone());
        if let Some(client) = self.session_client() {
            let item_id = track.item_id.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = client.report_start(&item_id, &session_id).await {
                    tracing::debug!("report start failed: {e}");
                }
            });
        }
        if let Some(token) = self.extras.listenbrainz_token.clone() {
            scrobble::submit_playing_now(token, track.clone());
        }
    }

    fn report_progress(&self, is_paused: bool) {
        let (Some(session_id), Some(track)) =
            (self.play_session_id.clone(), self.queue.get(self.index))
        else {
            return;
        };
        if let Some(client) = self.session_client() {
            let item_id = track.item_id.clone();
            let position_ms = self.position_ms();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = client
                    .report_progress(&item_id, &session_id, position_ms, is_paused)
                    .await
                {
                    tracing::debug!("report progress failed: {e}");
                }
            });
        }
    }

    /// Report a specific track as stopped at a specific position. `track` is
    /// the current queue entry, the one the listen clock measured.
    fn report_stopped(&mut self, track: &QueueTrack, position_ms: u64) {
        // Measure up to where it stopped (its full length at a natural end);
        // the listen rule then goes by time listened, not by that position.
        self.listen.sample(Instant::now(), position_ms);
        let listened_ms = self.listen.take();
        if self.recent_tracks.back() != Some(&track.item_id) {
            self.recent_tracks.push_back(track.item_id.clone());
        }
        let keep = self.extras.auto_dj_recent_tracks.clamp(1, 500) as usize;
        while self.recent_tracks.len() > keep {
            self.recent_tracks.pop_front();
        }
        if let Some(token) = self.extras.listenbrainz_token.clone() {
            if scrobble::should_scrobble(listened_ms, track.duration_ms) {
                scrobble::submit_listen(token, track.clone());
            }
        }
        let Some(session_id) = self.play_session_id.take() else {
            return;
        };
        if let Some(client) = self.session_client() {
            let item_id = track.item_id.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = client
                    .report_stopped(&item_id, &session_id, position_ms)
                    .await
                {
                    tracing::debug!("report stopped failed: {e}");
                }
            });
        }
    }

    /// Final report before the process exits.
    ///
    /// Unlike [`Self::report_stopped`] this *waits* for the requests. Both quit
    /// paths call `app.exit(0)`, which tears down the async runtime at once —
    /// a spawned report would never reach the network, leaving the Jellyfin
    /// session (and its transcode job) alive until the server times it out,
    /// and dropping the ListenBrainz listen for the track being played.
    fn shutdown(&mut self) {
        self.report_stopped_blocking(false);
        // Deliberately NOT stop_internal(): that ends in `emit()`, which
        // updates the tray, and the tray menu has to be touched on the main
        // thread. The main thread is at this very moment blocked waiting for
        // our acknowledgement (`quit()` -> `PlayerHandle::shutdown`), so going
        // through it would stall the whole quit until the 2.5 s timeout.
        // Nobody is watching the UI during shutdown anyway; just release the
        // audio device.
        self.generation = self.generation.wrapping_add(1);
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.appended = None;
        self.current_fade = None;
        self.fading_out = None;
        self.sink = None;
        self.status = PlaybackStatus::Idle;
    }

    /// Like [`Self::report_stopped_playing`], but waits for the requests
    /// instead of spawning them — for the moments where the runtime or the
    /// session is about to disappear (quit, sign-out).
    ///
    /// `resolve_boundary` settles a crossed track boundary first. Quit passes
    /// false: resolving it emits state, which updates the tray on the main
    /// thread — and that thread is blocked in `quit()` waiting for us.
    fn report_stopped_blocking(&mut self, resolve_boundary: bool) {
        if resolve_boundary {
            self.maybe_transition();
        }
        let position_ms = self.position_ms();
        let Some(track) = self.queue.get(self.index).cloned() else {
            return;
        };
        self.listen.sample(Instant::now(), position_ms);
        let listened_ms = self.listen.take();
        // Jellyfin first: the caller waits only briefly, and a slow
        // ListenBrainz must not use up the time the session report needs.
        if let (Some(session_id), Some(client)) =
            (self.play_session_id.take(), self.session_client())
        {
            let item_id = track.item_id.clone();
            tauri::async_runtime::block_on(async move {
                if let Err(e) = client
                    .report_stopped(&item_id, &session_id, position_ms)
                    .await
                {
                    tracing::debug!("final report stopped failed: {e}");
                }
            });
        }
        if let Some(token) = self.extras.listenbrainz_token.clone() {
            if scrobble::should_scrobble(listened_ms, track.duration_ms) {
                scrobble::submit_listen_blocking(token, track);
            }
        }
    }

    /// Report the currently playing track as stopped at the live position
    /// (used before the queue pointer moves).
    fn report_stopped_playing(&mut self) {
        let position_ms = self.position_ms();
        if let Some(track) = self.queue.get(self.index).cloned() {
            self.report_stopped(&track, position_ms);
        }
    }

    // --- Windows SMTC (media overlay + hardware media keys) ---

    fn update_media_metadata(&mut self, track: &QueueTrack) {
        let cover = self.cover_file_url(track);
        if cover.is_none() {
            self.fetch_smtc_artwork(track);
        }
        let Some(controls) = self.media.as_mut() else {
            return;
        };
        let _ = controls.set_metadata(souvlaki::MediaMetadata {
            title: Some(&track.name),
            artist: Some(&track.artist),
            album: Some(&track.album),
            duration: Some(Duration::from_millis(track.duration_ms)),
            cover_url: cover.as_deref(),
        });
    }

    /// The cover is not in the jfimg disk cache yet (e.g. the album was never
    /// browsed): fetch it in the background and refresh the SMTC metadata
    /// once it lands. The file-exists check above makes this run at most once
    /// per track start.
    fn fetch_smtc_artwork(&self, track: &QueueTrack) {
        if self.media.is_none() {
            return;
        }
        let (Some(item_id), Some(tag)) = (track.image_item_id.clone(), track.image_tag.clone())
        else {
            return;
        };
        let Some(client) = self.session_client() else {
            return;
        };
        // item_id/tag are server-supplied; never let them steer the path.
        let Some(name) = crate::cache::cover_file_name(&item_id, &tag, 360) else {
            tracing::warn!("refusing unsafe cover cache name from server");
            return;
        };
        let path = self.cover_dir.join(name);
        let track_item_id = track.item_id.clone();
        let tx = self.tx.clone();
        tauri::async_runtime::spawn(async move {
            match client.fetch_image(&item_id, &tag, 360).await {
                Ok(bytes) => {
                    // `name` is validated, so this is always the cache dir
                    // itself — never a directory the server picked.
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    // Atomically: the jfimg handler serves any non-empty cache
                    // file as immutable, so it must never catch a half-written
                    // one.
                    if crate::cache::write_file_atomic(&path, &bytes).is_ok() {
                        let _ = tx.send(PlayerCommand::RefreshSmtcArtwork {
                            item_id: track_item_id,
                        });
                    }
                }
                Err(e) => tracing::debug!("smtc artwork fetch failed: {e}"),
            }
        });
    }

    fn update_media_playback(&mut self) {
        let position = souvlaki::MediaPosition(Duration::from_millis(self.position_ms()));
        let Some(controls) = self.media.as_mut() else {
            return;
        };
        let playback = match self.status {
            PlaybackStatus::Playing | PlaybackStatus::Loading => souvlaki::MediaPlayback::Playing {
                progress: Some(position),
            },
            PlaybackStatus::Paused => souvlaki::MediaPlayback::Paused {
                progress: Some(position),
            },
            PlaybackStatus::Idle => souvlaki::MediaPlayback::Stopped,
        };
        let _ = controls.set_playback(playback);
    }

    /// Queue is on its last track and Auto-DJ is on: append a server mix
    /// seeded by it (once per track; the latch is released on failure so a
    /// transient error is retried on a later tick).
    fn maybe_auto_dj(&mut self) {
        if !self.extras.auto_dj_enabled
            || self.sleep_end_of_track // playback is about to stop on purpose
            || self.queue.is_empty()
            // Only at a genuine end of playback — repeat/shuffle still have a next.
            || self.next_index().is_some()
        {
            return;
        }
        let Some(current) = self.queue.get(self.index) else {
            return;
        };
        let remaining = current.duration_ms.saturating_sub(self.position_ms());
        if current.duration_ms == 0 || remaining > 30_000 {
            return;
        }
        if self.last_auto_dj_seed.as_deref() == Some(current.item_id.as_str()) {
            return;
        }
        self.last_auto_dj_seed = Some(current.item_id.clone());
        let Some(client) = self.session_client() else {
            return;
        };
        let request_track = current.item_id.clone();
        let (seed, reason) = choose_auto_dj_seed(current, self.extras.auto_dj_seed_mode);
        let max_tracks = self.extras.auto_dj_max_tracks.clamp(5, 100) as usize;
        let mut existing: std::collections::HashSet<String> =
            self.queue.iter().map(|t| t.item_id.clone()).collect();
        existing.extend(self.recent_tracks.iter().cloned());
        let tx = self.tx.clone();
        tauri::async_runtime::spawn(async move {
            let tracks = match client
                .instant_mix(&seed, (max_tracks as u32).saturating_mul(3))
                .await
            {
                Ok(tracks) => {
                    let fresh: Vec<_> = tracks
                        .into_iter()
                        .filter(|t| !existing.contains(&t.id))
                        .take(max_tracks)
                        .collect();
                    let mut queue = crate::commands::to_queue_tracks(&client, fresh)
                        .unwrap_or_else(|e| {
                            tracing::warn!("auto-dj queue build failed: {e}");
                            Vec::new()
                        });
                    for track in &mut queue {
                        track.auto_dj_reason = Some(reason.clone());
                    }
                    queue
                }
                Err(e) => {
                    tracing::warn!("auto-dj mix failed: {e}");
                    Vec::new()
                }
            };
            let _ = tx.send(PlayerCommand::AutoDjResult {
                request_track,
                reason,
                tracks,
            });
        });
    }

    /// The Auto-DJ fetch came back. Append the mix — and if the seeding track
    /// already finished (queue ran out while the server was thinking), resume
    /// playback into the first appended track instead of staying Idle.
    fn handle_auto_dj_result(
        &mut self,
        request_track: String,
        _reason: AutoDjReason,
        tracks: Vec<QueueTrack>,
    ) {
        if tracks.is_empty() {
            // Failure or nothing new: release the latch so a later tick of the
            // same track can retry.
            if self.last_auto_dj_seed.as_deref() == Some(request_track.as_str()) {
                self.last_auto_dj_seed = None;
            }
            return;
        }
        // The queue may have changed while the fetch ran: only append when the
        // seeding track is still last in *play* order (under shuffle that isn't
        // the physically last track), and drop duplicates.
        let last_in_order = if self.shuffle {
            self.order.last().and_then(|&i| self.queue.get(i))
        } else {
            self.queue.last()
        };
        if last_in_order.map(|t| t.item_id.as_str()) != Some(request_track.as_str()) {
            return;
        }
        let existing: std::collections::HashSet<&str> =
            self.queue.iter().map(|t| t.item_id.as_str()).collect();
        let fresh: Vec<QueueTrack> = tracks
            .into_iter()
            .filter(|t| !existing.contains(t.item_id.as_str()))
            .collect();
        if fresh.is_empty() {
            return;
        }
        // Only a queue that ran out on its own resumes into the mix — not a
        // Stop, a sleep timer or a lost output in the meantime.
        let ended = self.queue_ended
            && self.status == PlaybackStatus::Idle
            && self.sink.is_none()
            && self.next_index().is_none();
        let start = self.queue.len();
        self.queue_undo = None;
        self.queue_mut().extend(fresh);
        if self.shuffle {
            self.order.extend(start..self.queue.len());
        }
        self.sync_queue();
        if ended {
            self.index = start;
            self.sync_queue();
            self.start_current();
        } else {
            self.emit();
        }
    }

    /// SMTC artwork must be reachable outside the WebView; reuse the jfimg
    /// disk cache when the UI has already fetched the cover.
    fn cover_file_url(&self, track: &QueueTrack) -> Option<String> {
        let (item_id, tag) = (track.image_item_id.as_ref()?, track.image_tag.as_ref()?);
        for size in [360, 480, 300, 96] {
            // Validate before touching the filesystem: an unchecked component
            // could make this `exists()` probe an arbitrary path — including a
            // UNC path, where the probe alone leaks an NTLM handshake.
            let path = self
                .cover_dir
                .join(crate::cache::cover_file_name(item_id, tag, size)?);
            if path.exists() {
                // NOT a real file URI. souvlaki does not hand this to
                // `RandomAccessStreamReference::CreateFromUri` (an earlier
                // comment here claimed it did); for anything starting with
                // `file://` it strips exactly that prefix and passes the rest
                // to `StorageFile::GetFileFromPathAsync`, which wants a plain
                // Win32 path. So the two slashes and the native separators are
                // both load-bearing: `file:///C:/…` would arrive as `/C:/…`,
                // and a leading slash or forward slashes make the call fail —
                // and the failure aborts `set_metadata` before it pushes the
                // track, leaving the media overlay on the previous one.
                return Some(format!("file://{}", path.display()));
            }
        }
        None
    }

    fn position_ms(&self) -> u64 {
        self.position_offset_ms
            + self
                .sink
                .as_ref()
                .map(|s| s.get_pos().as_millis() as u64)
                .unwrap_or(0)
    }

    fn seek_ms(&mut self, position_ms: u64) {
        // A crossfade tail playing across a seek would be confusing: fade it
        // out quickly instead of hard-cutting (pop).
        if let Some((_, fade, drop_at)) = &mut self.fading_out {
            fade.ramp_to(0.0, Duration::from_millis(50));
            *drop_at = Instant::now() + Duration::from_millis(150);
        }
        // What played up to here was listened to; the jump is not.
        self.listen.sample(Instant::now(), self.position_ms());
        let Some(sink) = &self.sink else {
            return;
        };
        // Seek natively only when the whole file is on disk. rodio carries out
        // a native seek on the audio callback, and a decoder there waiting for
        // the server to deliver the target range stalls all audio — and this
        // thread, which waits for the callback. Anything else reopens the
        // stream at the target, positioned on the open thread (`restart_at`);
        // a live transcode always goes that way. A dead output never runs the
        // callback at all.
        let downloaded = self
            .current_download
            .as_ref()
            .is_some_and(|download| download.is_complete());
        let output_dead = self.output_dead.load(std::sync::atomic::Ordering::Relaxed);
        if downloaded && !output_dead {
            match sink.try_seek(Duration::from_millis(position_ms)) {
                Ok(()) => {
                    // rodio's position now reads the absolute target, also for
                    // a source that was reopened mid-track.
                    self.position_offset_ms = 0;
                    self.listen.rebase(Instant::now(), position_ms);
                    // A seek exits any crossfade context: snap to full volume
                    // instead of finishing a possibly long fade-in. A running
                    // sleep timer re-arms its fade-out from the new position.
                    if let Some(fade) = &self.current_fade {
                        fade.ramp_to(1.0, Duration::from_millis(PAUSE_FADE_MS));
                    }
                    self.sleep_fade_item = None;
                    self.update_sleep_fade();
                    self.report_progress(self.status == PlaybackStatus::Paused);
                    self.update_media_playback();
                    self.emit();
                    return;
                }
                Err(e) => {
                    tracing::info!("native seek failed ({e}); restarting stream at target");
                }
            }
        }
        self.restart_at(position_ms);
    }

    /// Reopen the current track at `position_ms` (seek on a live transcode, a
    /// new output device). A transcode starts there via `startTimeTicks`, a
    /// direct-play file is positioned on the open thread; the offset tracks
    /// media time either way.
    fn restart_at(&mut self, position_ms: u64) {
        let Some(track) = self.queue.get(self.index).cloned() else {
            return;
        };
        let resume_paused = self.status == PlaybackStatus::Paused;
        // Listening continues from the target; the jump there is not counted.
        self.listen.sample(Instant::now(), self.position_ms());
        self.listen.rebase(Instant::now(), position_ms);
        self.generation = self.generation.wrapping_add(1);
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.appended = None;
        self.pending_fade = None;
        self.current_fade = None;
        self.pending_tap = None;
        self.current_tap = None;
        self.pending_download = None;
        self.current_download = None;
        self.fading_out = None;
        self.sink = None;
        // The fresh source carries no sleep ramp yet; the next tick re-arms it.
        self.sleep_fade_item = None;
        // Arrive paused when it was paused; Play/Pause pressed while loading
        // adjusts this like for any other open.
        self.pending_pause = resume_paused;
        // Show the seek target while loading (get_pos is gone with the sink).
        self.position_offset_ms = position_ms;
        self.status = PlaybackStatus::Loading;
        self.emit();
        self.spawn_open(OpenPurpose::SeekRestart { position_ms }, track);
    }

    /// The queue for changing it: marks the track list as changed for
    /// `sync_queue`. Every change to `queue` goes through here.
    fn queue_mut(&mut self) -> &mut Vec<QueueTrack> {
        self.queue_revision = self.queue_revision.wrapping_add(1);
        &mut self.queue
    }

    /// Persist the queue, refresh the shared mirror, notify the UI.
    fn sync_queue(&mut self) {
        let order = if self.shuffle { &self.order[..] } else { &[] };
        let queue_snapshot = QueueSnapshot {
            tracks: self.queue.clone(),
            index: self.index,
            order: order.to_vec(),
            can_undo: self.queue_undo.is_some(),
            played_count: played_indices(self.queue.len(), self.index, order).len(),
            duplicate_count: duplicate_indices(&self.queue, self.index).len(),
        };
        *self.shared_queue.lock().unwrap() = queue_snapshot.clone();
        self.persist_queue();
        let _ = self.app.emit("player:queue", queue_snapshot);
    }

    /// Write the queue to the store. The track list can run to thousands of
    /// entries and rarely changes, while the index moves on every track
    /// change: the list is re-written only when it changed, a moved index is
    /// saved as a small entry of its own.
    fn persist_queue(&mut self) {
        match queue_write(
            self.persisted_queue.as_ref(),
            self.queue_revision,
            self.index,
        ) {
            QueueWrite::Nothing => {}
            QueueWrite::Index => {
                let Some(written) = self.persisted_queue.as_mut() else {
                    return;
                };
                let entry = PersistedQueueIndex {
                    snapshot_id: written.snapshot_id.clone(),
                    index: self.index,
                };
                if let Ok(json) = serde_json::to_string(&entry) {
                    if self.store.set(keys::QUEUE_INDEX, &json).is_ok() {
                        written.index = self.index;
                    }
                }
            }
            QueueWrite::Snapshot => {
                // A fresh id, so an index entry left from the previous list
                // never applies to this one.
                let snapshot_id = uuid::Uuid::new_v4().to_string();
                let persisted = PersistedQueue {
                    tracks: self.queue.clone(),
                    index: self.index,
                    server_url: self.store.get(keys::SERVER_URL),
                    user_id: self.store.get(keys::USER_ID),
                    snapshot_id: Some(snapshot_id.clone()),
                };
                if let Ok(json) = serde_json::to_string(&persisted) {
                    if self.store.set(keys::QUEUE, &json).is_ok() {
                        self.persisted_queue = Some(PersistedQueueMark {
                            revision: self.queue_revision,
                            snapshot_id,
                            index: self.index,
                        });
                    }
                }
            }
        }
    }

    fn emit(&self) {
        let current = self.queue.get(self.index).cloned();
        let duration_ms = current.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        let sleep_remaining_ms = if self.sleep_end_of_track && self.sleep_at.is_none() {
            Some(0)
        } else {
            self.sleep_at
                .map(|at| at.saturating_duration_since(Instant::now()).as_millis() as u64)
        };
        // A seek-by-restart source starts mid-track and has no known length;
        // only a source that plays its track from the start maps bytes to time.
        let buffered_ms = if self.position_offset_ms == 0 {
            buffered_range_ms(
                self.current_download.as_ref().and_then(|d| d.fraction()),
                duration_ms,
            )
        } else {
            None
        };
        let state = PlayerState {
            status: self.status,
            current,
            index: self.index,
            queue_len: self.queue.len(),
            position_ms: self.position_ms(),
            duration_ms,
            volume: self.volume,
            shuffle: self.shuffle,
            shuffle_mode: self.shuffle_mode,
            repeat: self.repeat,
            sleep_remaining_ms,
            buffered_ms,
        };
        *self.shared_state.lock().unwrap() = state.clone();
        let _ = self.app.emit("player:state", &state);
        crate::tray::update_player(&self.app, &state);
    }
}

/// Open an output stream on the named device (None = system default) with an
/// error callback that reports device death back to the player loop.
/// cpal's `name()` is deprecated in favor of id()/description(), but the
/// human-readable name is exactly what the settings dropdown shows.
#[allow(deprecated)]
fn open_output(
    device_name: &Option<String>,
    tx: Sender<PlayerCommand>,
) -> AppResult<(
    rodio::stream::MixerDeviceSink,
    Arc<std::sync::atomic::AtomicBool>,
)> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};

    let builder = match device_name {
        Some(name) => {
            let device = rodio::cpal::default_host()
                .output_devices()
                .map_err(|e| AppError::Audio(format!("cannot list output devices: {e}")))?
                .find(|d| d.name().map(|n| &n == name).unwrap_or(false))
                .ok_or_else(|| AppError::Audio(format!("output device '{name}' not found")))?;
            rodio::stream::DeviceSinkBuilder::from_device(device)
                .map_err(|e| AppError::Audio(format!("cannot use output device: {e}")))?
        }
        None => rodio::stream::DeviceSinkBuilder::from_default_device()
            .map_err(|e| AppError::Audio(format!("cannot open default output: {e}")))?,
    };
    let dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = dead.clone();
    let sink = builder
        .with_error_callback(move |err| {
            tracing::warn!("audio stream error: {err}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(PlayerCommand::OutputFailed);
        })
        .open_stream()
        .map_err(|e| AppError::Audio(format!("cannot open audio output: {e}")))?;
    Ok((sink, dead))
}

/// Output device names for the settings UI.
#[allow(deprecated)]
pub fn output_device_names() -> Vec<String> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    rodio::cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn init_media_controls(
    hwnd: Option<isize>,
    tx: Sender<PlayerCommand>,
) -> Option<souvlaki::MediaControls> {
    let hwnd = hwnd?;
    let config = souvlaki::PlatformConfig {
        dbus_name: "jellysic",
        display_name: "Jellysic",
        hwnd: Some(hwnd as *mut std::ffi::c_void),
    };
    let mut controls = match souvlaki::MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("media controls unavailable: {e:?}");
            return None;
        }
    };
    let result = controls.attach(move |event| {
        let cmd = match event {
            souvlaki::MediaControlEvent::Play => Some(PlayerCommand::Play),
            souvlaki::MediaControlEvent::Pause => Some(PlayerCommand::Pause),
            souvlaki::MediaControlEvent::Toggle => Some(PlayerCommand::Toggle),
            souvlaki::MediaControlEvent::Next => Some(PlayerCommand::Next),
            souvlaki::MediaControlEvent::Previous => Some(PlayerCommand::Prev),
            souvlaki::MediaControlEvent::Stop => Some(PlayerCommand::Stop),
            souvlaki::MediaControlEvent::SetPosition(souvlaki::MediaPosition(pos)) => {
                Some(PlayerCommand::Seek {
                    position_ms: pos.as_millis() as u64,
                })
            }
            _ => None,
        };
        if let Some(cmd) = cmd {
            let _ = tx.send(cmd);
        }
    });
    if let Err(e) = result {
        tracing::warn!("media controls attach failed: {e:?}");
        return None;
    }
    Some(controls)
}

/// Load the persisted queue snapshot (used on startup).
///
/// The snapshot is only handed back when it belongs to the server that is
/// configured now. Every entry carries an absolute `stream_url`, so restoring
/// a queue from another server would point playback at a foreign host — which
/// would then be handed the current session's token. Snapshots from builds
/// before this field existed carry no server and are dropped.
///
/// The index may have been saved after the track list (`Worker::persist_queue`).
pub fn load_persisted_queue(store: &Store) -> Option<(Vec<QueueTrack>, usize)> {
    let json = store.get(keys::QUEUE)?;
    let snapshot: PersistedQueue = serde_json::from_str(&json).ok()?;
    let server = store.get(keys::SERVER_URL)?;
    let user = store.get(keys::USER_ID)?;
    if snapshot.server_url.as_deref() != Some(server.as_str())
        || snapshot.user_id.as_deref() != Some(user.as_str())
    {
        tracing::info!("discarding persisted queue: it belongs to a different server or account");
        let _ = store.delete(keys::QUEUE);
        let _ = store.delete(keys::QUEUE_INDEX);
        return None;
    }
    let index = restored_index(&snapshot, store.get(keys::QUEUE_INDEX).as_deref());
    Some((snapshot.tracks, index))
}

#[cfg(test)]
mod tests {
    use super::{
        buffered_range_ms, build_album_shuffle_order, build_shuffle_order, choose_auto_dj_seed,
        duplicate_indices, load_persisted_queue, order_next, order_prev, played_indices,
        queue_write, remap_after_removals, remap_move, remap_order_after_removals, restored_index,
        should_crossfade, should_start_prefetch, AutoDjSeedMode, CrossfadeMode, PersistedQueue,
        PersistedQueueMark, PlayMode, QueueTrack, QueueWrite, ShuffleMode,
    };
    use crate::store::{keys, Store};
    use proptest::prelude::*;
    use rodio::buffer::SamplesBuffer;

    #[test]
    fn buffered_range_maps_byte_fractions_onto_the_timeline() {
        assert_eq!(
            buffered_range_ms(Some((0.0, 0.5)), 200_000),
            Some((0, 100_000))
        );
        assert_eq!(
            buffered_range_ms(Some((0.25, 1.0)), 240_000),
            Some((60_000, 240_000))
        );
        // Unknown length or unknown duration: nothing to draw.
        assert_eq!(buffered_range_ms(None, 200_000), None);
        assert_eq!(buffered_range_ms(Some((0.0, 0.5)), 0), None);
    }

    // Property tests: the pure queue-index helpers must hold their invariants
    // for arbitrary lengths, positions, and removal masks — not just the
    // hand-picked examples above.
    proptest! {
        /// A shuffle order is always a permutation of `0..len` with the current
        /// track first, whatever the seed.
        #[test]
        fn prop_shuffle_order_is_permutation_current_first(
            len in 1usize..64,
            seed in any::<u64>(),
        ) {
            let current = (seed as usize) % len;
            let order = build_shuffle_order(len, current, seed);
            prop_assert_eq!(order.len(), len);
            prop_assert_eq!(order[0], current);
            let mut sorted = order;
            sorted.sort_unstable();
            prop_assert_eq!(sorted, (0..len).collect::<Vec<_>>());
        }

        /// Reordering by `remap_move` is a bijection on `0..len`, so applying it
        /// to every index yields a permutation (no index is lost or doubled).
        #[test]
        fn prop_move_remap_is_permutation(
            len in 1usize..64,
            from_raw in 0usize..64,
            to_raw in 0usize..64,
        ) {
            let (from, to) = (from_raw % len, to_raw % len);
            let mut mapped: Vec<usize> = (0..len).map(|v| remap_move(v, from, to)).collect();
            mapped.sort_unstable();
            prop_assert_eq!(mapped, (0..len).collect::<Vec<_>>());
        }

        /// Removing a set of physical indices keeps the shuffle order a valid
        /// permutation of the survivors (`0..kept`) and preserves their relative
        /// play order.
        #[test]
        fn prop_removal_remap_preserves_permutation(
            mask in prop::collection::vec(any::<bool>(), 1..48),
            seed in any::<u64>(),
        ) {
            let len = mask.len();
            let order = build_shuffle_order(len, (seed as usize) % len, seed);
            let removed: Vec<usize> = (0..len).filter(|&i| mask[i]).collect();
            let kept = len - removed.len();

            let remapped = remap_order_after_removals(&order, &removed);
            prop_assert_eq!(remapped.len(), kept);
            let mut sorted = remapped.clone();
            sorted.sort_unstable();
            prop_assert_eq!(sorted, (0..kept).collect::<Vec<_>>());

            // Survivors keep their original relative order.
            let survivors: Vec<usize> = order.iter().copied().filter(|i| !removed.contains(i)).collect();
            let expected: Vec<usize> = survivors
                .iter()
                .map(|&i| remap_after_removals(i, &removed).unwrap())
                .collect();
            prop_assert_eq!(remapped, expected);
        }
    }

    #[test]
    fn linear_next_and_prev() {
        assert_eq!(order_next(3, 0, &[], false), Some(1));
        assert_eq!(order_next(3, 2, &[], false), None, "off stops at the end");
        assert_eq!(order_next(3, 2, &[], true), Some(0), "all wraps to start");
        assert_eq!(order_prev(3, 0, &[], false), None);
        assert_eq!(order_prev(3, 0, &[], true), Some(2), "all wraps to end");
        assert_eq!(order_next(0, 0, &[], true), None, "empty queue");
    }

    #[test]
    fn shuffle_next_follows_order() {
        // Play order 2 -> 0 -> 1.
        assert_eq!(order_next(3, 2, &[2, 0, 1], false), Some(0));
        assert_eq!(order_next(3, 0, &[2, 0, 1], false), Some(1));
        assert_eq!(order_next(3, 1, &[2, 0, 1], false), None, "last in order");
        assert_eq!(
            order_next(3, 1, &[2, 0, 1], true),
            Some(2),
            "wrap to order head"
        );
        assert_eq!(order_prev(3, 0, &[2, 0, 1], false), Some(2));
        assert_eq!(
            order_prev(3, 2, &[2, 0, 1], true),
            Some(1),
            "wrap to order tail"
        );
    }

    #[test]
    fn shuffle_order_is_full_permutation_current_first() {
        for (len, current) in [(1, 0), (2, 1), (6, 3), (20, 7)] {
            let order = build_shuffle_order(len, current, 0xC0FFEE ^ (len as u64) << 8);
            assert_eq!(order[0], current, "current track plays first");
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(
                sorted,
                (0..len).collect::<Vec<_>>(),
                "every index appears exactly once"
            );
        }
    }

    #[test]
    fn remap_move_shifts_indices() {
        // [a,b,c,d] move 0->2 => [b,c,a,d]
        assert_eq!(remap_move(0, 0, 2), 2);
        assert_eq!(remap_move(1, 0, 2), 0);
        assert_eq!(remap_move(2, 0, 2), 1);
        assert_eq!(remap_move(3, 0, 2), 3);
        // [a,b,c,d] move 3->1 => [a,d,b,c]
        assert_eq!(remap_move(3, 3, 1), 1);
        assert_eq!(remap_move(1, 3, 1), 2);
        assert_eq!(remap_move(2, 3, 1), 3);
        assert_eq!(remap_move(0, 3, 1), 0);
    }

    fn queue_track(item_id: &str) -> QueueTrack {
        QueueTrack {
            item_id: item_id.into(),
            name: item_id.into(),
            artist: String::new(),
            album: String::new(),
            album_id: None,
            track_number: None,
            disc_number: None,
            duration_ms: 1,
            image_item_id: None,
            image_tag: None,
            image_blur_hash: None,
            stream_url: String::new(),
            entry_id: String::new(),
            normalization_gain: None,
            artists: Vec::new(),
            genres: Vec::new(),
            source_playlist_id: None,
            auto_dj_reason: None,
            is_favorite: false,
        }
    }

    #[test]
    fn played_tracks_follow_actual_shuffle_order() {
        assert_eq!(played_indices(5, 2, &[]), vec![0, 1]);
        // Play order 2 -> 0 -> 3 -> 1 -> 4; current is 3.
        assert_eq!(played_indices(5, 3, &[2, 0, 3, 1, 4]), vec![2, 0]);
    }

    #[test]
    fn duplicate_cleanup_always_keeps_current_occurrence() {
        let tracks = ["a", "b", "a", "a", "c", "b"].map(queue_track);
        assert_eq!(duplicate_indices(&tracks, 2), vec![0, 3, 5]);
    }

    #[test]
    fn bulk_removal_preserves_shuffle_permutation_and_current() {
        let removed = [0, 3];
        assert_eq!(remap_after_removals(2, &removed), Some(1));
        let order = remap_order_after_removals(&[2, 0, 4, 1, 3], &removed);
        assert_eq!(order, vec![1, 2, 0]);
        let mut sorted = order;
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2]);
    }

    #[test]
    fn smart_crossfade_preserves_album_sequences_and_rejects_short_tracks() {
        let mut first = queue_track("a1");
        first.album_id = Some("album".into());
        first.disc_number = Some(1);
        first.track_number = Some(1);
        first.duration_ms = 180_000;
        let mut second = queue_track("a2");
        second.album_id = Some("album".into());
        second.disc_number = Some(1);
        second.track_number = Some(2);
        second.duration_ms = 180_000;
        assert!(!should_crossfade(
            CrossfadeMode::Smart,
            6_000,
            &first,
            &second
        ));
        second.album_id = Some("other".into());
        assert!(should_crossfade(
            CrossfadeMode::Smart,
            6_000,
            &first,
            &second
        ));
        second.duration_ms = 10_000;
        assert!(!should_crossfade(
            CrossfadeMode::Always,
            6_000,
            &first,
            &second
        ));
    }

    #[test]
    fn album_shuffle_keeps_blocks_sorted_and_is_a_permutation() {
        let mut a2 = queue_track("a2");
        a2.album_id = Some("a".into());
        a2.track_number = Some(2);
        let mut b1 = queue_track("b1");
        b1.album_id = Some("b".into());
        b1.track_number = Some(1);
        let mut a1 = queue_track("a1");
        a1.album_id = Some("a".into());
        a1.track_number = Some(1);
        let loose = queue_track("loose");
        let tracks = vec![a2, b1, a1, loose];
        for seed in 0..32 {
            let order = build_album_shuffle_order(&tracks, 2, seed);
            assert_eq!(
                &order[..2],
                &[2, 0],
                "current album remains internally sorted"
            );
            let mut sorted = order;
            sorted.sort_unstable();
            assert_eq!(sorted, vec![0, 1, 2, 3]);
        }
    }

    #[test]
    fn album_shuffle_started_mid_album_plays_the_rest_of_the_album_first() {
        // Issue #12: turning album shuffle on at track 3 put tracks 1-2 before
        // the current one in play order — never played with repeat off, yet
        // counted as played.
        let track = |id: &str, album: &str, number: i32| {
            let mut t = queue_track(id);
            t.album_id = Some(album.into());
            t.track_number = Some(number);
            t
        };
        let tracks = vec![
            track("x1", "x", 1),
            track("y2", "y", 2),
            track("x2", "x", 2),
            track("x3", "x", 3),
            track("y1", "y", 1),
            track("x4", "x", 4),
            queue_track("loose"),
            track("x5", "x", 5),
        ];
        let current = 3; // x3
        for seed in 0..32 {
            let order = build_album_shuffle_order(&tracks, current, seed);
            let ids: Vec<&str> = order.iter().map(|&i| tracks[i].item_id.as_str()).collect();
            assert_eq!(&ids[..5], &["x3", "x4", "x5", "x1", "x2"]);
            let y1 = ids.iter().position(|&id| id == "y1").unwrap();
            let y2 = ids.iter().position(|&id| id == "y2").unwrap();
            assert_eq!(y2, y1 + 1, "other albums stay whole and sorted");
            assert!(played_indices(tracks.len(), current, &order).is_empty());
            let mut sorted = order;
            sorted.sort_unstable();
            assert_eq!(sorted, (0..tracks.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn prefetch_opens_the_next_track_only_once() {
        assert!(should_start_prefetch(false, None, "b", 200_000, 15_000));
        assert!(
            !should_start_prefetch(false, None, "b", 200_000, 60_000),
            "too early"
        );
        assert!(
            should_start_prefetch(false, None, "b", 0, 0),
            "unknown length opens at once"
        );
        assert!(
            !should_start_prefetch(true, None, "b", 200_000, 15_000),
            "already open or opening"
        );
        // The gapless hand-off took the prefetched source: both slots are
        // empty again, but the track already sits in the sink.
        assert!(!should_start_prefetch(
            false,
            Some("b"),
            "b",
            200_000,
            2_000
        ));
        // The queue changed after the hand-off: the new next track is opened.
        assert!(should_start_prefetch(false, Some("b"), "c", 200_000, 2_000));
    }

    #[test]
    fn queue_tracks_are_rewritten_only_when_they_changed() {
        assert_eq!(
            queue_write(None, 0, 0),
            QueueWrite::Snapshot,
            "never written"
        );
        let written = PersistedQueueMark {
            revision: 3,
            snapshot_id: "a".into(),
            index: 1,
        };
        assert_eq!(queue_write(Some(&written), 3, 1), QueueWrite::Nothing);
        assert_eq!(
            queue_write(Some(&written), 3, 2),
            QueueWrite::Index,
            "track change"
        );
        assert_eq!(queue_write(Some(&written), 4, 1), QueueWrite::Snapshot);
        assert_eq!(queue_write(Some(&written), 4, 2), QueueWrite::Snapshot);
    }

    #[test]
    fn a_saved_index_applies_only_to_its_own_snapshot() {
        // Snapshots from builds that kept the index inside restore as before.
        let old: PersistedQueue = serde_json::from_str(
            r#"{"tracks":[],"index":2,"server_url":"https://music.home","user_id":"u"}"#,
        )
        .unwrap();
        assert_eq!(restored_index(&old, None), 2);
        assert_eq!(
            restored_index(&old, Some(r#"{"snapshot_id":"a","index":5}"#)),
            2
        );

        let json = serde_json::to_string(&PersistedQueue {
            tracks: vec![queue_track("t")],
            index: 2,
            server_url: None,
            user_id: None,
            snapshot_id: Some("a".into()),
        })
        .unwrap();
        let current: PersistedQueue = serde_json::from_str(&json).unwrap();
        assert_eq!(restored_index(&current, None), 2);
        assert_eq!(
            restored_index(&current, Some(r#"{"snapshot_id":"a","index":5}"#)),
            5
        );
        assert_eq!(
            restored_index(&current, Some(r#"{"snapshot_id":"b","index":5}"#)),
            2,
            "left over from an earlier track list"
        );
        assert_eq!(restored_index(&current, Some("not json")), 2);
    }

    #[test]
    fn a_persisted_queue_restores_its_saved_index_for_its_own_account_only() {
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = TempDir(
            std::env::temp_dir().join(format!("jellysic-queue-test-{}", uuid::Uuid::new_v4())),
        );
        let store = Store::open(&dir.0).unwrap();
        store.set(keys::SERVER_URL, "https://music.home").unwrap();
        store.set(keys::USER_ID, "u1").unwrap();
        let snapshot = PersistedQueue {
            tracks: vec![queue_track("a"), queue_track("b"), queue_track("c")],
            index: 0,
            server_url: Some("https://music.home".into()),
            user_id: Some("u1".into()),
            snapshot_id: Some("s1".into()),
        };
        store
            .set(keys::QUEUE, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        store
            .set(keys::QUEUE_INDEX, r#"{"snapshot_id":"s1","index":2}"#)
            .unwrap();
        let (tracks, index) = load_persisted_queue(&store).expect("same account");
        assert_eq!((tracks.len(), index), (3, 2));

        store.set(keys::USER_ID, "u2").unwrap();
        assert!(load_persisted_queue(&store).is_none(), "another account");
        assert!(store.get(keys::QUEUE).is_none());
        assert!(store.get(keys::QUEUE_INDEX).is_none());
    }

    #[test]
    fn stored_play_mode_still_reads_with_the_dropped_shuffle_flag() {
        // Builds up to 2026-09 wrote a now-unused `shuffle` flag next to the mode.
        let stored: PlayMode =
            serde_json::from_str(r#"{"shuffle":false,"shuffleMode":"albums","repeat":"all"}"#)
                .unwrap();
        assert_eq!(stored.shuffle_mode, ShuffleMode::Albums);
    }

    #[test]
    fn auto_dj_seed_falls_back_when_requested_metadata_is_missing() {
        let mut track = queue_track("track");
        track.name = "Song".into();
        let (id, reason) = choose_auto_dj_seed(&track, AutoDjSeedMode::Genre);
        assert_eq!(id, "track");
        assert_eq!(reason.kind, AutoDjSeedMode::Track);
        track.artists.push(crate::api::types::ArtistRef {
            id: "artist".into(),
            name: "Artist".into(),
        });
        let (id, reason) = choose_auto_dj_seed(&track, AutoDjSeedMode::Artist);
        assert_eq!(id, "artist");
        assert_eq!(reason.label, "Artist");
    }

    /// Gapless regression: two sources appended to rodio's queue must play
    /// back to back sample-continuously — this queue is exactly what
    /// `Player::append` builds on for the gapless hand-off.
    #[test]
    fn queued_sources_are_sample_continuous() {
        let sample_rate = rodio::SampleRate::new(48_000).unwrap();
        let channels = rodio::ChannelCount::new(1).unwrap();
        // A continuous sine split mid-wave across two buffers.
        let full: Vec<f32> = (0..9_600).map(|i| (i as f32 * 0.05).sin() * 0.8).collect();
        let first = SamplesBuffer::new(channels, sample_rate, full[..4_800].to_vec());
        let second = SamplesBuffer::new(channels, sample_rate, full[4_800..].to_vec());

        let (input, output) = rodio::queue::queue(false);
        input.append(first);
        input.append(second);

        let played: Vec<f32> = output.take(full.len()).collect();
        assert_eq!(played.len(), full.len(), "no samples may be dropped");
        for (i, (a, b)) in full.iter().zip(played.iter()).enumerate() {
            assert!(
                (a - b).abs() < 1e-6,
                "sample {i} discontinuous: expected {a}, got {b}"
            );
        }
    }
}
