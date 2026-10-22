pub mod analysis;
pub mod dsp;
mod errors;
pub mod fade;
mod limiter;
pub mod opus;
mod scrobble;
pub mod source;
pub mod tap;
pub mod tempo;
pub mod timbre;
mod uniform;
pub mod waveform;

use crate::api::JellyfinClient;
use crate::audio_devices::OutputDevice;
use crate::error::{AppError, AppResult};
use crate::store::{keys, Store};
use dsp::AudioDsp;
pub use dsp::DspParams;
use errors::PlayerErrorEvent;
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
/// A stream that ran this long before it failed gets its own device back
/// first, before the fallback to the default (see `handle_output_failed`).
const OUTPUT_SETTLED: Duration = Duration::from_secs(10);
/// How often "system default" looks whether the default device changed.
const DEFAULT_OUTPUT_POLL: Duration = Duration::from_secs(2);
/// A new default that did not open is tried again after this long, up to
/// `DEFAULT_OUTPUT_ATTEMPTS` times in all: Bluetooth headphones become the
/// default a moment before they can play.
const DEFAULT_OUTPUT_RETRY: Duration = Duration::from_secs(5);
const DEFAULT_OUTPUT_ATTEMPTS: u32 = 4;
/// The output closes once nothing has been audible for this long (see
/// `IdleClock`). A running render stream holds Windows' "An audio stream is
/// currently in use" power request, which keeps the machine from going to
/// sleep — playing silence, paused in the tray overnight, as much as music.
/// Five minutes: past a phone call or a word at the door, after which Play
/// should be instant, and short enough that a machine left alone sleeps not
/// much later than its power plan says. Play after that opens the device
/// again and reloads the paused track at its place, which takes a moment.
const IDLE_OUTPUT_CLOSE: Duration = Duration::from_secs(5 * 60);
/// Upper bound on commands buffered while the audio output is unavailable.
const DEFERRED_COMMAND_LIMIT: usize = 256;
/// Report playback progress to the server this often — on the clock, not in
/// ticks: a planned transition can wake the loop early.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// Start opening the next track's stream this long before the current ends.
const PREFETCH_OPEN_REMAINING_MS: u64 = 20_000;
/// With beat transitions: earlier, so the next track is downloaded and
/// analysed before its planned start (up to ~20 s before the end).
const PREFETCH_OPEN_BEAT_MS: u64 = 50_000;
/// A planned transition is started on a wake-up this early, and the rest is
/// made up with silence in front of the next track: thread wake-ups on
/// Windows are only good to ~16 ms, samples are exact.
const PLAN_LEAD_MS: u64 = 25;
/// A plan whose moment passed by more than this (a seek, a stall) is
/// dropped for a plain crossfade: started any later, the beats would flam.
const PLAN_LATE_MS: u64 = 30;
/// Hand the decoded next track to rodio this long before the boundary; rodio
/// then plays the two sources back to back without a gap.
const APPEND_REMAINING_MS: u64 = 3_000;
/// Pause/resume gain ramp length.
const PAUSE_FADE_MS: u64 = 150;
/// A player taken off the output (skip, stop, reopen) ramps to silence over
/// this long before it is dropped; dropping it while it plays cuts the
/// waveform off mid-swing.
const RELEASE_FADE_MS: u64 = 50;

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
    /// None = system default output device. Otherwise the device id
    /// (`OutputDevice::id`); builds before 0.6.1 stored cpal's bare endpoint
    /// name here, which is still found and replaced by the id once that
    /// device opens.
    pub output_device: Option<String>,
    /// The chosen device's name as last seen, for Settings and the toasts
    /// while the device is away, and to find it when Windows lists it under
    /// a new id (see `resolve_output`).
    pub output_device_name: Option<String>,
    /// Land crossfades on the beat: the next track starts so that its first
    /// beat falls on a beat of the current one (see `analysis.rs`). Only
    /// where a crossfade happens anyway and both tracks have a beat grid.
    pub beat_transitions: bool,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            crossfade_mode: CrossfadeMode::Off,
            crossfade_seconds: 6.0,
            output_device: None,
            output_device_name: None,
            beat_transitions: false,
        }
    }
}

/// Optional integrations, consumed by the player worker (persisted as JSON).
///
/// A seed this build does not know (written by a newer one) reads as the
/// track seed, which is what every seed falls back to anyway: an unknown
/// value must not cost the whole settings blob or the persisted queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoDjSeedMode {
    Artist,
    Genre,
    Album,
    Playlist,
    /// The client's own sonic similarity (`crate::sonic`): what sounds like
    /// the last few tracks, ordered to flow. Falls back to a track mix from
    /// the server while too little of the library is analysed.
    Sonic,
    /// Last only because serde wants the stand-in for unknown values there.
    #[default]
    #[serde(other)]
    Track,
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
    /// Keep the client's own play history (`history.rs`): one row per listen,
    /// on this machine only.
    pub record_history: bool,
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
            record_history: true,
            listenbrainz_configured: false,
        }
    }
}

/// Where the store keeps the sound seed: 0.6.0 has none and throws the whole
/// extras blob away over a seed it does not know, so it is written as the
/// track seed it falls back to, plus this flag only newer builds read.
const STORED_BY_SOUND: &str = "autoDjBySound";

impl ExtrasSettings {
    /// The settings as the store keeps them (`keys::EXTRAS`). The UI gets
    /// them as they are; only the stored form spells the sound seed so that
    /// an older build still reads the rest.
    pub fn to_stored_json(&self) -> serde_json::Result<String> {
        let mut value = serde_json::to_value(self)?;
        if self.auto_dj_seed_mode == AutoDjSeedMode::Sonic {
            value["autoDjSeedMode"] = serde_json::to_value(AutoDjSeedMode::Track)?;
            value[STORED_BY_SOUND] = serde_json::Value::Bool(true);
        }
        serde_json::to_string(&value)
    }

    /// Read what [`Self::to_stored_json`] wrote — or an older build did.
    pub fn from_stored_json(json: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        let by_sound = value
            .get(STORED_BY_SOUND)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut settings: Self = serde_json::from_value(value).ok()?;
        if by_sound && settings.auto_dj_seed_mode == AutoDjSeedMode::Track {
            settings.auto_dj_seed_mode = AutoDjSeedMode::Sonic;
        }
        Some(settings)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AutoDjReasonWire", into = "AutoDjReasonWire")]
pub struct AutoDjReason {
    pub kind: AutoDjSeedMode,
    pub label: String,
}

/// How a reason is written, in the persisted queue and to the UI alike. 0.6.0
/// reads the stored queue with a closed list of seeds and loses all of it over
/// one it does not know, so the sound seed goes out as the track seed it falls
/// back to, flagged `bySound`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoDjReasonWire {
    kind: AutoDjSeedMode,
    label: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    by_sound: bool,
}

impl From<AutoDjReasonWire> for AutoDjReason {
    fn from(wire: AutoDjReasonWire) -> Self {
        Self {
            kind: if wire.by_sound {
                AutoDjSeedMode::Sonic
            } else {
                wire.kind
            },
            label: wire.label,
        }
    }
}

impl From<AutoDjReason> for AutoDjReasonWire {
    fn from(reason: AutoDjReason) -> Self {
        let by_sound = reason.kind == AutoDjSeedMode::Sonic;
        Self {
            kind: if by_sound {
                AutoDjSeedMode::Track
            } else {
                reason.kind
            },
            label: reason.label,
            by_sound,
        }
    }
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
    /// The album's artist as the track is tagged, for the play history.
    /// `None` in queues from older builds.
    #[serde(default)]
    pub album_artist: Option<String>,
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
    /// The album's own gain, for album-mode normalization. Usually `None`:
    /// only Jellyfin after 10.11 sends it. Empty in queues from older builds.
    #[serde(default)]
    pub album_normalization_gain: Option<f32>,
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
/// Whose a listen is: the account of the session the track played in. A
/// session the server ended (a refused token, a forced move to https)
/// leaves the track playing and the account signed in as far as the store
/// goes; after a sign-out the store names nobody.
fn listener(session: Option<&JellyfinClient>, store: &Store) -> Option<crate::identity::Owner> {
    session
        .and_then(crate::identity::Owner::of)
        .or_else(|| crate::identity::Owner::current(store))
}

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

    // A missing disc number is disc 1, as everywhere else (album shuffle, the
    // album page): Jellyfin leaves it empty for files without a disc tag,
    // which is common for single-disc rips.
    let (disc, next_disc) = (
        current.disc_number.unwrap_or(1),
        next.disc_number.unwrap_or(1),
    );
    match (current.track_number, next.track_number) {
        (Some(track), Some(next_track)) => {
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
/// that is thrown away at the boundary. A next track whose prefetch already
/// failed (`failed`) is not tried again every tick either: the boundary opens
/// it as a plain Play, and that is the retry.
fn should_start_prefetch(
    busy: bool,
    appended: Option<&str>,
    failed: Option<&str>,
    next_id: &str,
    duration_ms: u64,
    remaining_ms: u64,
    window_ms: u64,
) -> bool {
    !busy
        && appended != Some(next_id)
        && failed != Some(next_id)
        && (duration_ms == 0 || remaining_ms <= window_ms)
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

/// "Stop after this track" / "stop after this album", armed on one queue
/// entry. Unlike the sleep timer this never fades: the track is supposed to
/// end the way it was recorded, and then it is quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopAfter {
    #[default]
    Off,
    Track,
    Album,
}

/// The gain an album is played at when the server does not name one.
///
/// Jellyfin only sends an album gain after 10.11, so for most servers there is
/// nothing to read. The album's own tracks are in the queue, though, and the
/// **median** of their gains is one constant offset for the whole album —
/// which is the point of album mode: the quiet piece stays quieter than the
/// loud one. It is not the album's measured loudness, so the absolute level
/// can sit a little off; the relative one, which is what anybody switches
/// album mode on for, is right.
///
/// `None` when no track of that album carries a gain at all.
fn album_gain_from_queue(queue: &[QueueTrack], album_id: &str) -> Option<f32> {
    let mut gains: Vec<f32> = queue
        .iter()
        .filter(|track| track.album_id.as_deref() == Some(album_id))
        .filter_map(|track| track.normalization_gain)
        .filter(|gain| gain.is_finite())
        .collect();
    if gains.is_empty() {
        return None;
    }
    gains.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = gains.len() / 2;
    Some(if gains.len().is_multiple_of(2) {
        (gains[middle - 1] + gains[middle]) / 2.0
    } else {
        gains[middle]
    })
}

/// One queue line as a command names it: by its entry id, which tells two
/// copies of the same track apart, and by its item id for a line persisted
/// before entry ids existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryRef {
    pub entry_id: String,
    pub item_id: String,
}

impl EntryRef {
    fn of(track: &QueueTrack) -> Self {
        Self {
            entry_id: track.entry_id.clone(),
            item_id: track.item_id.clone(),
        }
    }

    fn matches(&self, track: &QueueTrack) -> bool {
        if self.entry_id.is_empty() || track.entry_id.is_empty() {
            track.item_id == self.item_id
        } else {
            track.entry_id == self.entry_id
        }
    }
}

/// The entries an album stop armed on `armed` covers, in play order: from
/// `armed` to the last track of the album's run it sits in.
///
/// A run is the album's entries back to back in play order, split where the
/// album starts over: a track the run already had begins the next copy. The
/// split is counted from where the album's entries begin, so a stop armed in
/// the middle of the first copy ends with that copy, not in the second one.
/// The run never wraps around, so the album has a last track under Repeat
/// All too. An entry without an album covers itself only.
fn album_run(tracks: &[QueueTrack], order: &[usize], armed: usize) -> Vec<usize> {
    let alone = vec![armed];
    let Some(album) = tracks
        .get(armed)
        .and_then(|track| track.album_id.as_deref())
        .filter(|id| !id.is_empty())
    else {
        return alone;
    };
    let len = if order.is_empty() {
        tracks.len()
    } else {
        order.len()
    };
    let at = |position: usize| {
        if order.is_empty() {
            position
        } else {
            order[position]
        }
    };
    let Some(armed_at) = (0..len).find(|&position| at(position) == armed) else {
        return alone;
    };
    let of_album = |index: usize| {
        tracks
            .get(index)
            .is_some_and(|track| track.album_id.as_deref() == Some(album))
    };
    let mut first = armed_at;
    while first > 0 && of_album(at(first - 1)) {
        first -= 1;
    }
    let mut had = std::collections::HashSet::new();
    let mut last = armed_at;
    for position in first..len {
        let index = at(position);
        if !of_album(index) {
            break;
        }
        if !had.insert(tracks[index].item_id.as_str()) {
            // The album starts over here: after the armed entry, that is
            // where its run ended; before it, the armed one's run starts.
            if position > armed_at {
                break;
            }
            had.clear();
            had.insert(tracks[index].item_id.as_str());
        }
        last = position.max(armed_at);
    }
    (armed_at..=last).map(at).collect()
}

/// Does an armed stop fire when `finished` ends?
///
/// `at` is the entry the stop was resolved to: the row itself for a track,
/// the last entry of the album's run for an album (see [`album_run`] and
/// [`stop_entry`]) — the row the queue marks, where the music ends.
/// Repeat-One keeps restarting the entry that plays, so there the album ends
/// with whichever of its tracks that is.
///
/// Free function, and deliberately so: this is the whole rule, and it is worth
/// testing without a player thread around it.
fn stop_after_due(
    mode: StopAfter,
    at: Option<&EntryRef>,
    armed_album: Option<&str>,
    finished: &QueueTrack,
    repeat_one: bool,
) -> bool {
    match mode {
        StopAfter::Off => false,
        StopAfter::Track => at.is_some_and(|at| at.matches(finished)),
        StopAfter::Album => {
            // No album, no album end. The command refuses to arm this, and the
            // rule refuses to fire on it: a loose track has no last track.
            let Some(armed_album) = armed_album else {
                return false;
            };
            if repeat_one {
                return finished.album_id.as_deref() == Some(armed_album);
            }
            at.is_some_and(|at| at.matches(finished))
        }
    }
}

/// The entry an armed stop ends after once the queue changed: the last entry
/// of the ones it was armed over (`run`, in play order) that is still queued.
/// The album's last track removed, the album ends one track earlier; put
/// back by an undo, it ends there again. `None` when none is left: then
/// there is nothing to stop after.
fn stop_entry<'a>(run: &'a [EntryRef], queue: &[QueueTrack]) -> Option<&'a EntryRef> {
    let queued = |entry: &EntryRef| queue.iter().any(|track| entry.matches(track));
    let (end, rest) = run.split_last()?;
    if queued(end) {
        return Some(end);
    }
    // Searching further is rare (the end was removed) and can meet a
    // replaced queue of thousands: look the entry ids up once. An entry
    // without one goes by its item, as `EntryRef::matches` does.
    let ids: std::collections::HashSet<&str> =
        queue.iter().map(|track| track.entry_id.as_str()).collect();
    let by_item = ids.contains("");
    rest.iter().rev().find(|entry| {
        if entry.entry_id.is_empty() || by_item {
            queued(entry)
        } else {
            ids.contains(entry.entry_id.as_str())
        }
    })
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

/// Where the queue pointer goes when the current entry `removed` leaves the
/// queue: on to its successor in play order, or with none back to its
/// predecessor — as an index after the removal, and `true` when it is the
/// successor (only there may playback go on). `None` when the queue is empty
/// then. Whatever slot the removal leaves the pointer on can sit anywhere in a
/// shuffle order, with everything before it counting as played.
fn landing_after_removal(
    len: usize,
    removed: usize,
    order: &[usize],
    repeat_all: bool,
) -> Option<(usize, bool)> {
    let shifted = |index: usize| if index > removed { index - 1 } else { index };
    let other = |index: &usize| *index != removed;
    if let Some(next) = order_next(len, removed, order, repeat_all).filter(other) {
        return Some((shifted(next), true));
    }
    order_prev(len, removed, order, repeat_all)
        .filter(other)
        .map(|previous| (shifted(previous), false))
}

/// Whether the music goes on with the successor when the current entry is
/// removed: when it was playing, or loading to play. A load with a pause
/// waiting for it (Pause pressed while it loaded, a seek made while paused)
/// was paused, and the successor stays put like after a paused track.
fn plays_on_after_removal(status: PlaybackStatus, pending_pause: bool) -> bool {
    match status {
        PlaybackStatus::Playing => true,
        PlaybackStatus::Loading => !pending_pause,
        PlaybackStatus::Paused | PlaybackStatus::Idle => false,
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

/// How many tracks [`played_indices`] lists, without listing them: the count
/// goes with every track change, the list only with a cleanup.
fn played_count(len: usize, current: usize, order: &[usize]) -> usize {
    if len == 0 || current >= len {
        return 0;
    }
    if !order.is_empty() {
        if let Some(position) = order.iter().position(|&index| index == current) {
            return order[..position]
                .iter()
                .filter(|&&index| index < len)
                .count();
        }
    }
    current
}

/// How many entries [`duplicate_indices`] removes. Which copy stays does not
/// change how many go, so this follows the track list alone, not the current
/// track.
fn duplicate_count(tracks: &[QueueTrack]) -> usize {
    let distinct: std::collections::HashSet<&str> =
        tracks.iter().map(|track| track.item_id.as_str()).collect();
    tracks.len() - distinct.len()
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
///
/// An album queued twice is two blocks, one per copy, not one block that
/// plays every track twice in a row: the n-th time a track of an album shows
/// up, it belongs to that album's n-th block.
fn build_album_shuffle_order(tracks: &[QueueTrack], current: usize, seed: u64) -> Vec<usize> {
    if tracks.is_empty() || current >= tracks.len() {
        return Vec::new();
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: std::collections::HashMap<(&str, usize), usize> =
        std::collections::HashMap::new();
    let mut copies: std::collections::HashMap<(&str, &str), usize> =
        std::collections::HashMap::new();
    for (index, track) in tracks.iter().enumerate() {
        let group = match track.album_id.as_deref().filter(|id| !id.is_empty()) {
            Some(album) => {
                let copy = copies.entry((album, track.item_id.as_str())).or_insert(0);
                let key = (album, *copy);
                *copy += 1;
                *group_of.entry(key).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                })
            }
            None => {
                groups.push(Vec::new());
                groups.len() - 1
            }
        };
        groups[group].push(index);
    }
    for indices in &mut groups {
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
        .position(|indices| indices.contains(&current))
        .unwrap_or(0);
    groups.swap(0, current_group);
    // Started mid-album: continue the album, then wrap to its start. Tracks
    // before the current one in play order would never play with repeat off
    // (and count as played).
    if let Some(at) = groups[0].iter().position(|&index| index == current) {
        groups[0].rotate_left(at);
    }
    let mut state = seed | 1;
    if groups.len() > 2 {
        shuffle_slice(&mut groups[1..], &mut state);
    }
    groups.into_iter().flatten().collect()
}

/// The shuffle order a restored queue was playing in, if it still fits: built
/// for the shuffle mode that is on now, and a permutation of the queue's
/// indices. Otherwise `None`, and a fresh order is built.
fn restored_order(
    saved: Option<PersistedOrder>,
    mode: ShuffleMode,
    len: usize,
) -> Option<Vec<usize>> {
    let saved = saved.filter(|saved| saved.mode == mode && mode != ShuffleMode::Off)?;
    let mut seen = vec![false; len];
    for &index in &saved.order {
        if index >= len || std::mem::replace(&mut seen[index], true) {
            return None;
        }
    }
    (saved.order.len() == len).then_some(saved.order)
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
        AutoDjSeedMode::Track | AutoDjSeedMode::Sonic => {
            Some((track.item_id.clone(), track.name.clone()))
        }
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

/// What of a fetched Auto-DJ mix may be appended: nothing already queued or
/// just heard, by item (`existing`) or by recording (`heard`), and no more
/// than `max` tracks.
fn fresh_mix(
    tracks: Vec<crate::api::types::TrackDto>,
    existing: &std::collections::HashSet<String>,
    heard: &crate::commands_sonic::Recordings,
    max: usize,
) -> Vec<crate::api::types::TrackDto> {
    let unknown = tracks
        .into_iter()
        .filter(|track| !existing.contains(&track.id))
        .collect();
    let mut fresh = crate::commands_sonic::without_copies_of(unknown, heard);
    fresh.truncate(max);
    fresh
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub status: PlaybackStatus,
    /// Only while loading: the source arrives paused — Pause was pressed
    /// during the load, or a paused track is being reopened. Play/Pause shows
    /// what the arrival will do, not the load itself.
    pub pending_pause: bool,
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
    pub stop_after: StopAfter,
    /// The queue entry the music stops after, so the list can mark the row:
    /// the armed row for a track, the last row of its album for an album.
    /// Its entry id, and its item id for a line from before entry ids.
    pub stop_after_entry: Option<String>,
    pub stop_after_item: Option<String>,
    /// Downloaded part of the current track as `[start, end]` in ms, for the
    /// seek bar. Estimated from bytes, so approximate for variable bitrates;
    /// `None` when the stream length is unknown (live transcode).
    pub buffered_ms: Option<(u64, u64)>,
    /// Playback speed (1 = normal) and whether the pitch is kept.
    pub speed: f32,
    pub preserve_pitch: bool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            pending_pause: false,
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
            stop_after: StopAfter::Off,
            stop_after_entry: None,
            stop_after_item: None,
            buffered_ms: None,
            speed: 1.0,
            preserve_pitch: true,
        }
    }
}

/// Whether the source being loaded arrives paused: Pause pressed while it
/// loads, or a paused track reopened. Only a load can; once a source is in,
/// the status says it, and a flag left over from an open that failed means
/// nothing.
fn arrives_paused(status: PlaybackStatus, pending_pause: bool) -> bool {
    status == PlaybackStatus::Loading && pending_pause
}

/// What the system media overlay (SMTC) shows. A load shows what its arrival
/// will do, as the window's Play/Pause button does.
fn media_playback(
    status: PlaybackStatus,
    pending_pause: bool,
    position: Duration,
) -> souvlaki::MediaPlayback {
    let progress = Some(souvlaki::MediaPosition(position));
    if status == PlaybackStatus::Idle {
        souvlaki::MediaPlayback::Stopped
    } else if status == PlaybackStatus::Paused || arrives_paused(status, pending_pause) {
        souvlaki::MediaPlayback::Paused { progress }
    } else {
        souvlaki::MediaPlayback::Playing { progress }
    }
}

/// Map a downloaded byte fraction onto the track's timeline.
/// A fresh play session id. See `spawn_open` for what the server does with it.
fn new_play_session() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Open with `auth`. A 401 — the session's token was revoked, a device
/// removed in the Jellyfin dashboard — gets one renewal (`renew`) and one
/// more open with the renewed session; whatever that brings is the answer.
fn open_renewing<T>(
    auth: StreamAuth,
    mut open: impl FnMut(&StreamAuth) -> AppResult<T>,
    renew: impl FnOnce(AppError) -> AppResult<StreamAuth>,
) -> AppResult<T> {
    match open(&auth) {
        Err(unauthorized @ AppError::Server { status: 401, .. }) => {
            tracing::info!("stream refused with 401, renewing the session");
            let renewed = renew(unauthorized)?;
            open(&renewed)
        }
        other => other,
    }
}

/// A fetch of the player's own on the async runtime (the Auto-DJ mix) on
/// `client`: a 401 gets one renewal (`renew`) and one more try, and `client`
/// is the renewed session from then on.
async fn fetch_renewing<T, Fetched, Renewed>(
    client: &mut Arc<JellyfinClient>,
    fetch: impl Fn(Arc<JellyfinClient>) -> Fetched,
    renew: impl FnOnce(Arc<JellyfinClient>, AppError) -> Renewed,
) -> AppResult<T>
where
    Fetched: std::future::Future<Output = AppResult<T>>,
    Renewed: std::future::Future<Output = AppResult<Arc<JellyfinClient>>>,
{
    match fetch(client.clone()).await {
        Err(unauthorized @ AppError::Server { status: 401, .. }) => {
            tracing::info!("player fetch refused with 401, renewing the session");
            *client = renew(client.clone(), unauthorized).await?;
            fetch(client.clone()).await
        }
        other => other,
    }
}

/// [`source::open_track_source`] through [`open_renewing`]: `attempt.stop`
/// stops every try, and each try gets a token of its own under it — a try
/// that fails stops the one it was given on its way out, and the retry must
/// not start out stopped.
fn open_track_renewing(
    auth: StreamAuth,
    track: &QueueTrack,
    attempt: source::Attempt<'_>,
    dsp: Arc<AudioDsp>,
    tap: Arc<tap::VisualizerTap>,
    waveform: Option<waveform::WaveformRequest>,
    renew: impl FnOnce(AppError) -> AppResult<StreamAuth>,
) -> AppResult<OpenedSource> {
    let source::Attempt {
        play_session,
        gain_db,
        start_ms,
        rate,
        stop,
    } = attempt;
    open_renewing(
        auth,
        |auth| {
            source::open_track_source(
                auth,
                track,
                source::Attempt {
                    play_session,
                    gain_db,
                    start_ms,
                    rate,
                    stop: stop.child_token(),
                },
                dsp.clone(),
                tap.clone(),
                waveform.clone(),
            )
        },
        renew,
    )
}

/// The session renewed after a stream open of `stale` got `unauthorized`,
/// as the auth to open again with. Runs on the open thread, never on the
/// player's: the renewal itself is a task on the async runtime
/// (`commands::renew_session`), so an open the player gives up on (`stop`)
/// stops waiting for it at once, and the renewal still runs to its end.
fn renew_stream_auth(
    app: &tauri::AppHandle,
    stale: &Arc<JellyfinClient>,
    unauthorized: AppError,
    stop: &tokio_util::sync::CancellationToken,
) -> AppResult<StreamAuth> {
    if stop.is_cancelled() {
        return Err(unauthorized);
    }
    let fallback = unauthorized.replay();
    let at = Instant::now();
    tauri::async_runtime::block_on(async {
        tokio::select! {
            renewed = crate::commands::renew_session(app, stale, at, unauthorized) => {
                renewed.map(|client| StreamAuth::from_client(&client))
            }
            () = stop.cancelled() => Err(fallback),
        }
    })
}

#[cfg(test)]
mod open_renewing_tests {
    use super::{
        fetch_renewing, open_renewing, open_track_renewing, source, tap, AppError, AudioDsp,
        DspParams, JellyfinClient, QueueTrack, StreamAuth,
    };
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn auth_for(token: &str, base_url: &str) -> StreamAuth {
        StreamAuth {
            auth_header: format!("MediaBrowser Token=\"{token}\""),
            trusted: Vec::new(),
            base_url: base_url.into(),
            server: crate::identity::Server {
                address: base_url.into(),
                id: None,
            },
        }
    }

    fn auth(token: &str) -> StreamAuth {
        auth_for(token, "http://music.home")
    }

    fn status(status: u16) -> AppError {
        AppError::Server {
            status,
            message: "stream open failed".into(),
        }
    }

    /// Opens with the answers in `script`, in order; returns the result, the
    /// auth headers the opens went out with, and how often it renewed.
    fn run(
        script: Vec<Result<(), AppError>>,
        renewal: Result<StreamAuth, AppError>,
    ) -> (Result<(), AppError>, Vec<String>, usize) {
        let script = RefCell::new(script.into_iter());
        let opened_with = RefCell::new(Vec::new());
        let renewals = RefCell::new(0);
        let result = open_renewing(
            auth("revoked"),
            |auth| {
                opened_with.borrow_mut().push(auth.auth_header.clone());
                script.borrow_mut().next().expect("no more opens expected")
            },
            |unauthorized| {
                assert!(matches!(unauthorized, AppError::Server { status: 401, .. }));
                *renewals.borrow_mut() += 1;
                renewal
            },
        );
        (result, opened_with.into_inner(), renewals.into_inner())
    }

    #[test]
    fn a_401_renews_once_and_opens_once_more_with_the_renewed_session() {
        let (result, opened_with, renewals) =
            run(vec![Err(status(401)), Ok(())], Ok(auth("fresh")));
        assert!(result.is_ok());
        assert_eq!(renewals, 1);
        assert_eq!(opened_with.len(), 2);
        assert!(opened_with[1].contains("fresh"), "{opened_with:?}");
    }

    #[test]
    fn a_second_401_is_the_answer() {
        let (result, opened_with, renewals) =
            run(vec![Err(status(401)), Err(status(401))], Ok(auth("fresh")));
        assert!(matches!(result, Err(AppError::Server { status: 401, .. })));
        assert_eq!((opened_with.len(), renewals), (2, 1));
    }

    #[test]
    fn a_renewal_that_fails_is_not_followed_by_another_open() {
        let (result, opened_with, renewals) =
            run(vec![Err(status(401))], Err(AppError::NotConnected));
        assert!(matches!(result, Err(AppError::NotConnected)));
        assert_eq!((opened_with.len(), renewals), (1, 1));
    }

    /// A second of CD-quality silence as a WAV file.
    fn wav() -> Vec<u8> {
        let data_len: u32 = 44_100 * 2 * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // channels
        out.extend_from_slice(&44_100u32.to_le_bytes());
        out.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.resize(44 + data_len as usize, 0);
        out
    }

    /// A stand-in for the server's stream: 401 for the revoked session, the
    /// file for any other. Returns its URL and the auth header of every
    /// request.
    fn stream_server() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stand-in server");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let body = wav();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => head.extend_from_slice(&buf[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(|value| value.trim().to_string())
                };
                let authorization = header("authorization:").unwrap_or_default();
                log.lock().unwrap().push(authorization.clone());
                if authorization.contains("revoked") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                let start = header("range:")
                    .and_then(|range| {
                        let (start, _) = range.strip_prefix("bytes=")?.split_once('-')?;
                        start.parse::<usize>().ok()
                    })
                    .filter(|&start| start < body.len());
                let (status, part) = match start {
                    Some(start) => ("206 Partial Content", &body[start..]),
                    None => ("200 OK", &body[..]),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: audio/wav\r\nConnection: close\r\n\r\n",
                        part.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(part);
            }
        });
        (url, seen)
    }

    fn track(url: &str) -> QueueTrack {
        QueueTrack {
            item_id: "x".into(),
            name: "x".into(),
            artist: String::new(),
            album: String::new(),
            album_id: None,
            album_artist: None,
            track_number: None,
            disc_number: None,
            duration_ms: 1_000,
            image_item_id: None,
            image_tag: None,
            image_blur_hash: None,
            stream_url: format!("{url}/Audio/x/universal?userId=u"),
            entry_id: String::new(),
            normalization_gain: None,
            album_normalization_gain: None,
            artists: Vec::new(),
            genres: Vec::new(),
            source_playlist_id: None,
            auto_dj_reason: None,
            is_favorite: false,
        }
    }

    /// Opens `url`'s track as `spawn_open` does, starting with the revoked
    /// session; `renewal` is what renewing it brings.
    fn open_revoked(
        url: &str,
        stop: &tokio_util::sync::CancellationToken,
        renewal: Result<StreamAuth, AppError>,
    ) -> Result<source::OpenedSource, AppError> {
        open_track_renewing(
            auth_for("revoked", url),
            &track(url),
            source::Attempt {
                play_session: "attempt",
                gain_db: None,
                start_ms: None,
                rate: rodio::SampleRate::new(48_000).unwrap(),
                stop: stop.clone(),
            },
            Arc::new(AudioDsp::new(DspParams::default())),
            Arc::new(tap::VisualizerTap::new()),
            None,
            |_| renewal,
        )
    }

    #[test]
    fn a_track_whose_session_was_revoked_plays_on_the_renewed_one() {
        let (url, seen) = stream_server();
        let stop = tokio_util::sync::CancellationToken::new();
        let opened = open_revoked(&url, &stop, Ok(auth_for("fresh", &url)));
        assert!(opened.is_ok(), "{:?}", opened.err());
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[0].contains("revoked") && seen[1].contains("fresh"));
        // The failed try stopped only what it was given; the open as a whole
        // is still wanted.
        assert!(!stop.is_cancelled());
    }

    #[test]
    fn a_track_whose_session_cannot_be_renewed_fails_after_one_request() {
        let (url, seen) = stream_server();
        let stop = tokio_util::sync::CancellationToken::new();
        let opened = open_revoked(&url, &stop, Err(AppError::NotConnected));
        assert!(matches!(opened, Err(AppError::NotConnected)));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    fn session(token: &str) -> Arc<JellyfinClient> {
        Arc::new(
            JellyfinClient::new("http://music.home", "test-device", Vec::new())
                .unwrap()
                .with_session(token.into(), "alice-id".into()),
        )
    }

    /// Fetches with the answers in `script`; returns the result, the tokens
    /// the fetches went out with, how often it renewed, and the client the
    /// caller carries on with.
    async fn fetch_with(
        script: Vec<Result<(), AppError>>,
        renewal: Result<Arc<JellyfinClient>, AppError>,
    ) -> (
        Result<(), AppError>,
        Vec<String>,
        usize,
        Arc<JellyfinClient>,
    ) {
        let script = Mutex::new(script.into_iter());
        let fetched_with = Mutex::new(Vec::new());
        let renewals = Mutex::new(0);
        let mut client = session("revoked");
        let result = fetch_renewing(
            &mut client,
            |client| {
                fetched_with
                    .lock()
                    .unwrap()
                    .push(client.token().unwrap_or_default().to_string());
                let answer = script
                    .lock()
                    .unwrap()
                    .next()
                    .expect("no more fetches expected");
                async move { answer }
            },
            |_, unauthorized| {
                assert!(matches!(unauthorized, AppError::Server { status: 401, .. }));
                *renewals.lock().unwrap() += 1;
                async move { renewal }
            },
        )
        .await;
        let fetched_with = fetched_with.into_inner().unwrap();
        let renewals = renewals.into_inner().unwrap();
        (result, fetched_with, renewals, client)
    }

    #[tokio::test]
    async fn a_player_fetch_renews_once_and_goes_on_with_the_renewed_session() {
        let (result, fetched_with, renewals, client) =
            fetch_with(vec![Err(status(401)), Ok(())], Ok(session("fresh"))).await;
        assert!(result.is_ok());
        assert_eq!(fetched_with, ["revoked", "fresh"]);
        assert_eq!(renewals, 1);
        assert_eq!(client.token(), Some("fresh"));

        let (result, fetched_with, renewals, _) = fetch_with(
            vec![Err(status(401)), Err(status(401))],
            Ok(session("fresh")),
        )
        .await;
        assert!(matches!(result, Err(AppError::Server { status: 401, .. })));
        assert_eq!((fetched_with.len(), renewals), (2, 1));

        let (result, fetched_with, renewals, client) =
            fetch_with(vec![Err(status(401))], Err(AppError::NotConnected)).await;
        assert!(matches!(result, Err(AppError::NotConnected)));
        assert_eq!((fetched_with.len(), renewals), (1, 1));
        assert_eq!(client.token(), Some("revoked"));

        let (result, fetched_with, renewals, _) =
            fetch_with(vec![Err(status(503))], Ok(session("fresh"))).await;
        assert!(matches!(result, Err(AppError::Server { status: 503, .. })));
        assert_eq!((fetched_with.len(), renewals), (1, 0));
    }

    #[test]
    fn other_failures_renew_nothing() {
        for failure in [
            status(403),
            status(404),
            status(500),
            AppError::Audio("decode failed".into()),
            AppError::NotConnected,
        ] {
            let shown = failure.to_string();
            let (result, opened_with, renewals) = run(vec![Err(failure)], Ok(auth("fresh")));
            assert_eq!(result.unwrap_err().to_string(), shown);
            assert_eq!((opened_with.len(), renewals), (1, 0), "{shown}");
        }
    }
}

fn buffered_range_ms(fraction: Option<(f64, f64)>, duration_ms: u64) -> Option<(u64, u64)> {
    let (start, end) = fraction?;
    if duration_ms == 0 {
        return None;
    }
    let at = |f: f64| (f.clamp(0.0, 1.0) * duration_ms as f64).round() as u64;
    Some((at(start), at(end)))
}

/// How far short of its length a track must stop to count as broken off.
const BROKE_OFF_SHORT_MS: u64 = 5_000;

/// Whether a track that ran out at `stopped_at_ms` broke off rather than
/// played through: a decoder ends on any read error, so a stream that broke
/// off looks like a natural end otherwise. `download` is the source's
/// downloaded range ([`DownloadProgress::fraction`]).
///
/// It takes both a stop clearly short of the track and a download that has
/// not reached the end. The download alone proves nothing: a reopen at a
/// position fetches from there to the end first and fills in the part before
/// it only then, so a track reopened near its end plays out while that fill
/// still runs. A short stop alone proves nothing either: a damaged frame also
/// ends a decoder, with the whole file on disk. A live transcode has no
/// length to tell by and never counts as broken off.
fn stream_broke_off(stopped_at_ms: u64, duration_ms: u64, download: Option<(f64, f64)>) -> bool {
    duration_ms > 0
        && stopped_at_ms.saturating_add(BROKE_OFF_SHORT_MS) < duration_ms
        && download.is_some_and(|(_, end)| end < 1.0)
}

/// `player:stream-broke-off` — the track stopped early because its stream
/// broke off (`Worker::handle_track_end`). The UI words the message.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamBrokeOffEvent<'a> {
    name: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    /// Names this track list and order; a [`QueuePosition`] applies to the
    /// snapshot with the same revision only.
    pub revision: u64,
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

/// `player:queue-index`: the queue moved on, the track list did not. Sent in
/// place of the whole [`QueueSnapshot`] on a track change — a played smart
/// view holds thousands of entries, and every window would get all of them
/// again with every track.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePosition {
    pub revision: u64,
    pub index: usize,
    pub can_undo: bool,
    pub played_count: usize,
    pub duplicate_count: usize,
}

pub enum PlayerCommand {
    /// Replace queue and start playing at `start_index` (sent through
    /// [`PlayerHandle::play_queue`], which drops a request overtaken by a
    /// newer one).
    PlayQueue {
        tracks: Vec<QueueTrack>,
        start_index: usize,
        /// Play it in exactly this order, also while shuffle is on.
        in_order: bool,
    },
    /// Load a queue without starting playback (session restore).
    RestoreQueue(RestoredQueue),
    /// Insert right after the current track.
    PlayNext(Vec<QueueTrack>),
    /// Append to the end of the queue.
    PlayLast(Vec<QueueTrack>),
    RemoveAt {
        index: usize,
        /// Identity check against stale UI indices (e.g. double-click on the
        /// remove button must not delete the track that slid into the slot,
        /// even when that is another copy of the same track).
        entry: EntryRef,
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
    /// `reply` gets the settings the worker ended up with: an output device
    /// that does not open is refused, and the previous one stays.
    SetPlaybackSettings {
        settings: PlaybackSettings,
        reply: Option<mpsc::SyncSender<PlaybackSettings>>,
    },
    /// Go back from a fallback to the configured output device (Settings'
    /// "switch back"); `ack` once it was tried.
    RestoreOutput {
        ack: mpsc::SyncSender<()>,
    },
    /// A settings import wrote the volume (`Some`), the EQ/normalization
    /// (already live) and maybe the per-device profiles: settle them on the
    /// device playing now.
    SoundImported {
        volume: Option<f32>,
        profiles: bool,
    },
    SetSpeed(tempo::SpeedSetting),
    SetExtrasSettings(ExtrasSettings),
    /// None clears the timer; `end_of_track` pauses at the next boundary
    /// after the minutes elapsed (or immediately at the boundary when 0).
    SetSleepTimer {
        minutes: Option<u32>,
        end_of_track: bool,
        fade_seconds: u32,
    },
    /// Arm (or clear, with `StopAfter::Off`) a stop at the end of a track or
    /// of its album. `entry` names the queue entry; `None` means whatever is
    /// playing now.
    SetStopAfter {
        mode: StopAfter,
        entry: Option<EntryRef>,
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
    /// The shuffle order the list was playing in. Without it every start
    /// would shuffle anew: what was heard would come round again, and "play
    /// next" would land anywhere. Absent while shuffle is off and in
    /// snapshots from older builds. Read leniently: an order a later build
    /// wrote for a shuffle mode this one does not know is dropped (shuffled
    /// anew), and the tracks still come back.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "unless_unreadable"
    )]
    order: Option<PersistedOrder>,
}

/// An optional part of a stored blob, or `None` where it does not read: what
/// a later build adds to it must not cost the whole blob.
fn unless_unreadable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// A shuffle order and the mode it was built for (`ShuffleMode::Tracks`
/// orders differ from album blocks): restored only under that same mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedOrder {
    mode: ShuffleMode,
    order: Vec<usize>,
}

/// A queue read back from the store, for [`PlayerCommand::RestoreQueue`].
pub struct RestoredQueue {
    tracks: Vec<QueueTrack>,
    index: usize,
    /// Where the track at `index` stood when playback last stopped for a
    /// while (`Worker::persist_position`); 0 when nothing says.
    position_ms: u64,
    order: Option<PersistedOrder>,
}

/// The queue index, saved apart from the track list (`keys::QUEUE_INDEX`): it
/// moves on every track change, and writing it into the snapshot would
/// re-serialize the whole queue each time. Applies only to the snapshot it
/// names. It also carries where that track stood when playback last stopped
/// for a while, so a restart — an update's too — goes on from there.
#[derive(Serialize, Deserialize)]
struct PersistedQueueIndex {
    snapshot_id: String,
    index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position_ms: Option<u64>,
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

/// Index and position to restore a snapshot at: the separately saved ones
/// when they belong to this very track list, otherwise the snapshot's own
/// index from the start of the track.
fn restored_index(snapshot: &PersistedQueue, saved_index: Option<&str>) -> (usize, u64) {
    saved_index
        .and_then(|json| serde_json::from_str::<PersistedQueueIndex>(json).ok())
        .filter(|saved| snapshot.snapshot_id.as_deref() == Some(saved.snapshot_id.as_str()))
        .map_or((snapshot.index, 0), |saved| {
            (saved.index, saved.position_ms.unwrap_or(0))
        })
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

/// A track that was just heard, kept out of the next Auto-DJ mixes: by item,
/// and by recording, so another file of it stays out too — also once a
/// "remove played" has taken it out of the queue.
struct RecentTrack {
    item_id: String,
    artist: String,
    name: String,
    duration_ms: u64,
}

impl RecentTrack {
    fn of(track: &QueueTrack) -> Self {
        Self {
            item_id: track.item_id.clone(),
            artist: track.artist.clone(),
            name: track.name.clone(),
            duration_ms: track.duration_ms,
        }
    }
}

/// Pause before an Auto-DJ seed whose mix came back with nothing (a failure,
/// or nothing that is not queued or heard already) may ask again, and how
/// often it may ask: the answer rarely changes from one tick to the next, and
/// the last 30 s of a track hold 75 of them.
const AUTO_DJ_RETRY_AFTER: Duration = Duration::from_secs(10);
const AUTO_DJ_ATTEMPTS: u32 = 3;

struct AutoDjRetry {
    seed: String,
    attempts: u32,
    not_before: Instant,
}

impl AutoDjRetry {
    /// Whether `seed` has to wait (or has asked often enough) at `now`.
    fn holds(&self, seed: &str, now: Instant) -> bool {
        self.seed == seed && (self.attempts >= AUTO_DJ_ATTEMPTS || now < self.not_before)
    }
}

/// How long the final report on quit or sign-out may wait for each server:
/// the callers wait no longer (2.5 s on quit, 2 s on sign-out), and a server
/// that does not answer must not hold the player for the client's whole
/// timeout.
const FINAL_REPORT_TIMEOUT: Duration = Duration::from_secs(2);

/// The final report of the track that was playing, taken while it still
/// played (`Worker::take_final_report`).
struct FinalReport {
    track: QueueTrack,
    position_ms: u64,
    listen: scrobble::Listen,
    session_id: Option<String>,
}

#[derive(Clone)]
pub struct PlayerHandle {
    tx: Sender<PlayerCommand>,
    state: Arc<Mutex<PlayerState>>,
    queue: Arc<Mutex<QueueSnapshot>>,
    dsp: Arc<AudioDsp>,
    tap: Arc<tap::VisualizerTap>,
    waveforms: Arc<waveform::WaveformCache>,
    analyses: Arc<analysis::AnalysisCache>,
    /// Requests to replace the queue, numbered as they are made, and the
    /// additions waiting behind them (see [`PlayerHandle::play_ticket`]).
    play_requests: Arc<PlayRequests>,
    /// Held by a request that adds to the queue while it fetches (see
    /// [`PlayerHandle::enqueue_turn`]).
    enqueue_turn: Arc<tokio::sync::Mutex<()>>,
}

/// One request to replace the queue, numbered when it was made. Dropped
/// without having replaced the queue (a failed fetch, a refusal), it gives
/// way: an older request still fetching may then play after all.
pub struct PlayTicket {
    number: u64,
    requests: Arc<PlayRequests>,
}

impl Drop for PlayTicket {
    fn drop(&mut self) {
        self.requests.settle(self.number);
    }
}

/// Which requests to replace the queue are still fetching, and which one
/// replaced it last.
struct PlayRequests {
    book: Mutex<PlayBook>,
    /// Bumped whenever a request settles, to wake whoever waits for one.
    settled: tokio::sync::watch::Sender<u64>,
}

#[derive(Default)]
struct PlayBook {
    /// Tickets handed out so far; the newest has this number.
    issued: u64,
    /// Requests still fetching: neither sent nor given up.
    pending: std::collections::BTreeSet<u64>,
    /// The newest request that replaced the queue.
    sent: u64,
}

impl Default for PlayRequests {
    fn default() -> Self {
        Self {
            book: Mutex::default(),
            settled: tokio::sync::watch::Sender::new(0),
        }
    }
}

impl PlayRequests {
    /// `number` no longer fetches: whoever waits on it looks again.
    fn settle(&self, number: u64) {
        let was_pending = self.book.lock().unwrap().pending.remove(&number);
        if was_pending {
            self.wake();
        }
    }

    fn wake(&self) {
        self.settled
            .send_modify(|settled| *settled = settled.wrapping_add(1));
    }
}

/// A request's turn to add to the queue (see [`PlayerHandle::enqueue_turn`]).
pub struct EnqueueTurn {
    _turn: tokio::sync::OwnedMutexGuard<()>,
    /// The newest play request made before the addition was asked for.
    after: u64,
}

/// `player:analysis` — tempo and key of one track, once analysed.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAnalysisInfo {
    pub item_id: String,
    pub bpm: Option<f32>,
    /// Camelot code ("8A"), the notation DJ software uses.
    pub camelot: Option<String>,
    /// Pitch class of the tonic (0 = C) and mode, for a localized name.
    pub key: Option<analysis::MusicalKey>,
}

impl TrackAnalysisInfo {
    fn new(item_id: &str, analysis: &analysis::TrackAnalysis) -> Self {
        Self {
            item_id: item_id.to_string(),
            bpm: analysis.bpm,
            camelot: analysis.camelot(),
            key: analysis.key,
        }
    }
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

    /// A ticket for a request that replaces the queue once it has fetched its
    /// tracks. Taken before the fetch: what plays is what was asked for last,
    /// not whichever fetch happens to come back last.
    pub fn play_ticket(&self) -> PlayTicket {
        let mut book = self.play_requests.book.lock().unwrap();
        book.issued += 1;
        let number = book.issued;
        book.pending.insert(number);
        PlayTicket {
            number,
            requests: self.play_requests.clone(),
        }
    }

    /// Replace the queue with `tracks` and play from `start_index` — unless a
    /// newer request has replaced it already: then this one has been
    /// overtaken and is dropped. See [`Self::replace_queue`].
    pub async fn play_queue(
        &self,
        ticket: PlayTicket,
        tracks: Vec<QueueTrack>,
        start_index: usize,
    ) -> AppResult<()> {
        self.replace_queue(ticket, tracks, start_index, false).await
    }

    /// Like [`Self::play_queue`], from the first track, in exactly this order
    /// even while shuffle is on: a sonic radio or path, where the order is
    /// what was asked for.
    pub async fn play_in_order(
        &self,
        ticket: PlayTicket,
        tracks: Vec<QueueTrack>,
    ) -> AppResult<()> {
        self.replace_queue(ticket, tracks, 0, true).await
    }

    /// While a newer request is still fetching, this one waits for it: if
    /// that one replaces the queue, this one is overtaken; if it fails or is
    /// refused, this one plays after all — what plays is the last request
    /// that could.
    async fn replace_queue(
        &self,
        ticket: PlayTicket,
        tracks: Vec<QueueTrack>,
        start_index: usize,
        in_order: bool,
    ) -> AppResult<()> {
        let mut settled = self.play_requests.settled.subscribe();
        loop {
            {
                let mut book = self.play_requests.book.lock().unwrap();
                if book.sent > ticket.number {
                    drop(book);
                    tracing::debug!("play request overtaken by a newer one; dropped");
                    return Ok(());
                }
                if book.pending.range(ticket.number + 1..).next().is_none() {
                    // Sent under the lock: nothing newer can look and send
                    // in between, and land before this one.
                    book.sent = ticket.number;
                    book.pending.remove(&ticket.number);
                    let sent = self.send(PlayerCommand::PlayQueue {
                        tracks,
                        start_index,
                        in_order,
                    });
                    drop(book);
                    self.play_requests.wake();
                    return sent;
                }
            }
            if settled.changed().await.is_err() {
                return Ok(());
            }
        }
    }

    /// Wait for the turn to add to the queue, and hold it while fetching what
    /// to add: two additions land in the order they were asked for, also when
    /// the first one's fetch is the slower one. Taken first thing, so the
    /// plays asked for before the addition are known (see [`Self::enqueue`]).
    pub async fn enqueue_turn(&self) -> EnqueueTurn {
        // Before waiting for the turn: a play asked for while an earlier
        // addition still fetches came after this one.
        let after = self.play_requests.book.lock().unwrap().issued;
        EnqueueTurn {
            _turn: self.enqueue_turn.clone().lock_owned().await,
            after,
        }
    }

    /// Add `tracks` right after the current track (`next`) or at the end,
    /// once every play request made before the addition was asked for has
    /// replaced the queue or given up: a slower play asked for first would
    /// otherwise replace the queue right after, and take the addition along.
    pub async fn enqueue(
        &self,
        turn: EnqueueTurn,
        tracks: Vec<QueueTrack>,
        next: bool,
    ) -> AppResult<()> {
        let mut settled = self.play_requests.settled.subscribe();
        loop {
            {
                let book = self.play_requests.book.lock().unwrap();
                if book.pending.range(..=turn.after).next().is_none() {
                    // Under the lock, like a play: the two land in the order
                    // they were decided.
                    let sent = self.send(if next {
                        PlayerCommand::PlayNext(tracks)
                    } else {
                        PlayerCommand::PlayLast(tracks)
                    });
                    drop(book);
                    return sent;
                }
            }
            if settled.changed().await.is_err() {
                return Ok(());
            }
        }
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

    /// Apply playback settings and wait (bounded) for the ones the worker
    /// ended up with, so Settings shows what really applies. Blocking — call
    /// it off the async runtime.
    pub fn set_playback_settings(
        &self,
        settings: PlaybackSettings,
        timeout: Duration,
    ) -> AppResult<PlaybackSettings> {
        let (reply, effective) = mpsc::sync_channel(1);
        self.send(PlayerCommand::SetPlaybackSettings {
            settings: settings.clone(),
            reply: Some(reply),
        })?;
        // A worker still waiting for any output at all applies them later;
        // until then they are what was asked for.
        Ok(effective.recv_timeout(timeout).unwrap_or(settings))
    }

    /// Switch back from a fallback to the configured output device and wait
    /// (bounded) until it was tried. Blocking — call it off the async
    /// runtime.
    pub fn restore_output(&self, timeout: Duration) -> AppResult<()> {
        let (ack, done) = mpsc::sync_channel(1);
        self.send(PlayerCommand::RestoreOutput { ack })?;
        if done.recv_timeout(timeout).is_err() {
            tracing::warn!("player did not get back to the configured output in time");
        }
        Ok(())
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

    pub fn track_analysis(&self, item_id: &str) -> Option<TrackAnalysisInfo> {
        self.analyses
            .get(item_id)
            .map(|analysis| TrackAnalysisInfo::new(item_id, &analysis))
    }
}

/// A command that asks for sound: what a missing output device has to answer
/// with an error, or the user is left clicking Play at nothing.
fn asks_for_playback(cmd: &PlayerCommand) -> bool {
    matches!(
        cmd,
        PlayerCommand::PlayQueue { .. }
            | PlayerCommand::Play
            | PlayerCommand::Toggle
            | PlayerCommand::JumpTo(_)
            | PlayerCommand::Next
            | PlayerCommand::Prev
    )
}

/// The queue and what plays from it: everything a new queue replaces.
fn acts_on_queue(cmd: &PlayerCommand) -> bool {
    matches!(
        cmd,
        PlayerCommand::PlayQueue { .. }
            | PlayerCommand::RestoreQueue { .. }
            | PlayerCommand::ResetQueue
            | PlayerCommand::PlayNext(_)
            | PlayerCommand::PlayLast(_)
            | PlayerCommand::RemoveAt { .. }
            | PlayerCommand::JumpTo(_)
            | PlayerCommand::MoveTrack { .. }
            | PlayerCommand::ClearQueue
            | PlayerCommand::RemovePlayed
            | PlayerCommand::RemoveDuplicates
            | PlayerCommand::UndoQueue
            | PlayerCommand::Play
            | PlayerCommand::Pause
            | PlayerCommand::Toggle
            | PlayerCommand::Next
            | PlayerCommand::Prev
            | PlayerCommand::Seek { .. }
            | PlayerCommand::Stop
    )
}

/// Keep a command that arrived while there is no output, for when one opens.
///
/// Bounded, but not at the cost of what matters. A volume or speed drag sends
/// one command per input event, so each keeps only its latest value. A new
/// queue (a play, the session restore, a sign-out's reset) makes the queue
/// edits and transport commands before it moot, so those go — and the new
/// queue itself is never what falls off the end: a lost reset would replay
/// the previous account's queue for the next one.
fn defer_command(deferred: &mut Vec<PlayerCommand>, cmd: PlayerCommand) {
    use std::mem::discriminant;
    let replaces_queue = matches!(
        cmd,
        PlayerCommand::PlayQueue { .. }
            | PlayerCommand::RestoreQueue { .. }
            | PlayerCommand::ResetQueue
    );
    if matches!(
        cmd,
        PlayerCommand::SetVolume { .. } | PlayerCommand::SetSpeed(_)
    ) {
        deferred.retain(|earlier| discriminant(earlier) != discriminant(&cmd));
    } else if replaces_queue {
        deferred.retain(|earlier| !acts_on_queue(earlier));
    }
    if replaces_queue || deferred.len() < DEFERRED_COMMAND_LIMIT {
        deferred.push(cmd);
    }
}

/// When the worker loop ticks: every [`TICK`] on the clock, whatever arrives
/// in between, or earlier when a planned transition asks for it. Restarting
/// the wait on every command would let a stream of them — a volume drag sends
/// one per input event, a held key one per repeat — starve everything that
/// runs on the tick: the crossfade start, the gapless append, the sleep timer,
/// progress reports.
struct TickClock {
    next: Instant,
}

impl TickClock {
    fn new(now: Instant) -> Self {
        Self { next: now + TICK }
    }

    /// The moment to wake up at: the next tick, or `early` from `now` when
    /// that comes first ([`Worker::next_wake`]).
    fn wake_at(&self, now: Instant, early: Duration) -> Instant {
        self.next.min(now + early)
    }

    /// Woken at `now`, by a command or by the timeout: whether to tick, the
    /// wake-up moment having come. The tick after it is a full interval away.
    fn tick_due(&mut self, now: Instant, wake_at: Instant) -> bool {
        let due = now >= wake_at;
        if due {
            self.next = now + TICK;
        }
        due
    }
}

/// A new player on the output at `volume`, filled by `fill` — and paused, if
/// asked — before it joins the mixer. Its way into the mixer
/// (`uniform::wrap`) sets up the format conversion from what the player
/// reports at that moment, and an empty player reports a placeholder (one
/// channel at 48 kHz) that the first 512 samples of the real source would be
/// converted as: a garbled blip at every start, and a beat transition landing
/// late. Paused before it joins, it cannot sound for a moment before the
/// pause takes either.
fn start_player(
    mix: &Mix,
    volume: f32,
    paused: bool,
    fill: impl FnOnce(&rodio::Player),
) -> rodio::Player {
    let (player, output) = rodio::Player::new();
    player.set_volume(volume);
    fill(&player);
    if paused {
        player.pause();
    }
    mix.mixer.add(uniform::wrap(output, mix.channels, mix.rate));
    player
}

/// The sleep fade's gain with `remaining` left until silence, for a fade
/// `fade` long; None while it has not started yet.
fn sleep_level(remaining: Duration, fade: Duration) -> Option<f32> {
    (remaining <= fade).then(|| {
        let full = fade.as_secs_f32();
        if full > 0.0 {
            (remaining.as_secs_f32() / full).clamp(0.0, 1.0)
        } else {
            0.0
        }
    })
}

/// What the sleep fade does to the playing source (`Worker::update_sleep_fade`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum SleepFadeStep {
    /// Leave the gain alone.
    Keep,
    /// Ramp to the fade's level by the time a resume's ramp would be done.
    Rise { to: f32, over: Duration },
    /// Ramp from wherever the gain is down to silence.
    Fall { over: Duration },
}

/// The sleep fade's next step for the playing source. `armed`: the source
/// carries the fade already. `resume_left`: a resume ramps the gain back up
/// to the fade's level, done in this long (zero: done by now). `ramp`: where
/// the fade stands, [`Worker::sleep_ramp`] — None outside it.
///
/// A fade handle runs one ramp at a time, so the fade waits for a resume's
/// ramp and takes over from its end. When it was re-timed meanwhile (a seek,
/// a timer or speed change took the mark), the gain rises to the new level
/// instead, by the same moment — rather than swelling to full volume.
fn sleep_fade_step(
    armed: bool,
    resume_left: Option<Duration>,
    ramp: Option<(f32, Duration)>,
) -> SleepFadeStep {
    let Some((level, rest)) = ramp else {
        return SleepFadeStep::Keep;
    };
    match resume_left {
        Some(left) if !left.is_zero() => {
            if armed {
                SleepFadeStep::Keep
            } else {
                SleepFadeStep::Rise {
                    to: level,
                    over: left,
                }
            }
        }
        // The resume's ramp is done: the fade takes over from its end.
        Some(_) => SleepFadeStep::Fall { over: rest },
        None if armed => SleepFadeStep::Keep,
        None => SleepFadeStep::Fall { over: rest },
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
        revision: 0,
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
    let analyses = Arc::new(analysis::AnalysisCache::new(Some(store.clone())));
    let handle = PlayerHandle {
        tx: tx.clone(),
        state: shared_state.clone(),
        queue: shared_queue.clone(),
        dsp: dsp.clone(),
        tap: visualizer_tap.clone(),
        waveforms: waveforms.clone(),
        analyses: analyses.clone(),
        play_requests: Arc::default(),
        enqueue_turn: Arc::new(tokio::sync::Mutex::new(())),
    };

    std::thread::Builder::new()
        .name("sonarune-player".into())
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
            // whenever the audio device is late (see `defer_command`).
            let mut deferred: Vec<PlayerCommand> = Vec::new();
            // When the UI was last told that there is no output.
            let mut reported_output_error: Option<Instant> = None;
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
                    analyses.clone(),
                    tx.clone(),
                    hwnd,
                    cover_dir.clone(),
                ) {
                    Ok(w) => break w,
                    Err(e) => {
                        tracing::error!("audio output init failed: {e}");
                        let message = PlayerErrorEvent::output_unavailable(&e);
                        // Report once, not every 3 s — a missing device would
                        // otherwise flood the UI with the same banner.
                        if reported_output_error.is_none() {
                            reported_output_error = Some(Instant::now());
                            let _ = app.emit("player:error", message.clone());
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
                                    // The report above goes out while the UI
                                    // is still starting, and an event nobody
                                    // listens to is gone: whoever asks for
                                    // sound is told why there is none — at
                                    // most every few seconds, a held key
                                    // repeats.
                                    if asks_for_playback(&cmd)
                                        && reported_output_error
                                            .is_none_or(|at| at.elapsed() >= OUTPUT_RETRY_DELAY)
                                    {
                                        reported_output_error = Some(Instant::now());
                                        let _ = app.emit("player:error", message.clone());
                                    }
                                    defer_command(&mut deferred, cmd);
                                }
                                Err(RecvTimeoutError::Timeout) => break,
                                Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    }
                }
            };
            // What the worker loaded (volume, shuffle, repeat, speed) has to
            // reach the shared state now: the next emit may only come with
            // the first command, and a UI reading the state back meanwhile
            // would see the defaults and compute its toggles from them.
            worker.emit();
            // Everything that arrived while there was no output device.
            for cmd in deferred.drain(..) {
                worker.handle(cmd);
            }
            let mut clock = TickClock::new(Instant::now());
            loop {
                let wake_at = clock.wake_at(Instant::now(), worker.next_wake());
                match rx.recv_timeout(wake_at.saturating_duration_since(Instant::now())) {
                    Ok(PlayerCommand::Shutdown { ack }) => {
                        worker.shutdown();
                        let _ = ack.send(());
                        break;
                    }
                    Ok(cmd) => worker.handle(cmd),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if clock.tick_due(Instant::now(), wake_at) {
                    worker.tick();
                }
                // After every command, not only on the tick: a moment of
                // music between two ticks is no silence.
                worker.watch_idle_output();
                worker.drop_stale_parked();
                // After the tick too: a track end it handled moves the
                // generation on, and the opens that made stale stop now.
                worker.stop_stale_opens();
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
    output: Output,
    /// Playback speed: the stage in front of the device, and the setting.
    tempo: tempo::TempoHandle,
    speed: tempo::SpeedSetting,
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
    /// Changed through `order_mut`.
    order: Vec<usize>,
    /// Last track that seeded an Auto-DJ append (fires once per track).
    last_auto_dj_seed: Option<String>,
    /// The seed whose mix last came back with nothing, and when it may ask
    /// again.
    auto_dj_retry: Option<AutoDjRetry>,
    /// Recently completed tracks excluded from new Auto-DJ mixes.
    recent_tracks: std::collections::VecDeque<RecentTrack>,
    /// Exact state before the most recent removal/clear operation. It is not
    /// persisted and is invalidated by unrelated queue/order changes.
    queue_undo: Option<QueueUndo>,
    /// Wall-clock moment the sleep timer fires (None = not armed).
    sleep_at: Option<Instant>,
    sleep_end_of_track: bool,
    /// "Stop after this track / album": the entry the music ends after (see
    /// `stop_after_due`), and for an album stop the album.
    stop_after: StopAfter,
    stop_at: Option<EntryRef>,
    /// The entries the stop was armed over, in play order: the row itself,
    /// or the album's run from the armed row on (`album_run`). `stop_at` is
    /// the last of them still queued, so removing the album's last track
    /// ends it one track earlier instead of not at all.
    stop_run: Vec<EntryRef>,
    stop_after_album: Option<String>,
    sleep_fade_duration: Duration,
    /// Item id whose source currently carries the sleep ramp.
    sleep_fade_item: Option<String>,
    /// Until when the ramp of a resume runs. A sleep fade-out takes over
    /// from its end: a fade handle runs one ramp at a time.
    resume_ramp_until: Option<Instant>,
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
    /// fade handle), dropped after its ramp — with its download, which has to
    /// keep running as long as the tail plays.
    fading_out: Option<(
        rodio::Player,
        FadeHandle,
        Instant,
        Option<Arc<DownloadProgress>>,
    )>,
    /// Players taken off the output (a skip, a stop, a reopen), ramping to
    /// silence until a tick drops them at the given moment.
    draining: Vec<(rodio::Player, Instant)>,
    /// Debounces OutputFailed storms from a dying device. Set by every
    /// attempt, also one that failed and left the old stream playing.
    last_output_rebuild: Instant,
    /// When the stream that plays now was opened (see `output_opened`).
    stream_opened_at: Instant,
    /// An OutputFailed arrived inside the debounce window; retried on a tick.
    output_failed_pending: bool,
    /// False after the output died and could not be rebuilt; the next play
    /// attempt re-opens it.
    output_alive: bool,
    /// Set by the stream's error callback the moment the device dies, ahead of
    /// the queued OutputFailed. rodio waits for the audio callback to carry out
    /// a native seek, and a dead stream never calls back.
    output_dead: Arc<std::sync::atomic::AtomicBool>,
    /// The device the output plays through (`audio_devices` has it for
    /// everyone else), with the name older builds knew it by.
    opened_output: Option<OutputEntry>,
    /// When "system default" last looked at what the default is.
    last_default_poll: Instant,
    /// A new default device that did not open (yet): tried again a few
    /// times, then not until the default changes once more.
    default_output_failed: Option<FailedDefault>,
    /// How long the open output has played nothing; closes it after
    /// `IDLE_OUTPUT_CLOSE` (`watch_idle_output`).
    idle: IdleClock,
    /// The source on its way fades in from silence, as a resume does: the
    /// paused track reloaded after the output closed (`resume_closed`).
    /// Should the reload fail, it goes back to paused (`reopen_failed`).
    fade_in_arrival: bool,
    /// The decoder and the whole download of the paused track whose player
    /// went with the output closed for silence (`close_idle_output`): Play
    /// resumes from them, without the server (`resume_closed`). Dropped as
    /// soon as anything else becomes of that track (`drop_stale_parked`).
    parked: Option<ParkedTrack>,
    queue: Vec<QueueTrack>,
    /// Bumped by every change to `queue` or to its play order (see
    /// `queue_mut`, `order_mut`), so `sync_queue` re-writes and re-sends the
    /// track list only when it actually changed.
    queue_revision: u64,
    /// What `sync_queue` last persisted.
    persisted_queue: Option<PersistedQueueMark>,
    /// The revision `sync_queue` last sent in full, and its duplicate count.
    synced_revision: Option<u64>,
    duplicate_count: usize,
    index: usize,
    status: PlaybackStatus,
    volume: f32,
    /// `volume` changed and is not saved yet (see `save_volume`).
    volume_unsaved: bool,
    /// Invalidates in-flight background opens; bumped whenever "what should
    /// play" changes.
    generation: u64,
    /// Invalidates only in-flight prefetches: a queue edit re-decides the next
    /// track, while the current track's own open is still wanted.
    prefetch_generation: u64,
    /// Next track, opened and decoded, waiting to be appended.
    prefetched: Option<Prefetched>,
    /// Opens in flight, so the ones `generation`/`prefetch_generation` have
    /// made stale can be stopped (`stop_stale_opens`).
    opens: source::OpenTracker,
    /// item_id of a prefetch open currently running on a background thread.
    prefetch_inflight: Option<String>,
    /// item_id of a next track whose prefetch failed. Not opened ahead of
    /// the boundary again; the boundary opens it as a plain Play, which is
    /// the retry. Forgotten when the queue or the current track changes.
    prefetch_failed: Option<String>,
    /// The generation of a prefetch of the track now loading, still on its
    /// way when the track before it ended (`handle_track_end`). It may land
    /// before the fresh open of the same track and play instead.
    adoptable_prefetch: Option<u64>,
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
    /// Until when the silence in front of a beat transition plays.
    lead_until: Option<Instant>,
    /// Jellyfin play session of the playback currently being reported — the
    /// one in the stream URL of the source in `sink`, minted when that source
    /// was opened (`spawn_open`). Taken when the track stops, so the same id
    /// is never reported started twice.
    play_session_id: Option<String>,
    /// When the last progress report (or the start report) went out.
    last_progress_report: Instant,
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
        analyses: Arc<analysis::AnalysisCache>,
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
            analyses,
            analysis_notify: {
                let app = app.clone();
                Arc::new(move |item_id: &str, analysis: &analysis::TrackAnalysis| {
                    let _ = app.emit("player:analysis", TrackAnalysisInfo::new(item_id, analysis));
                })
            },
        };
        let mut playback: PlaybackSettings = store
            .get(keys::PLAYBACK)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let mut extras: ExtrasSettings = store
            .get(keys::EXTRAS)
            .and_then(|json| ExtrasSettings::from_stored_json(&json))
            .unwrap_or_default();
        // The token lives in the credential manager, not the store.
        if extras.listenbrainz_token.is_none() {
            extras.listenbrainz_token = crate::commands::listenbrainz_token();
        }
        let play_mode: PlayMode = store
            .get(keys::PLAY_MODE)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let speed = tempo::SpeedSetting::load(&store);
        let tempo = tempo::TempoHandle::new(speed.speed, speed.preserve_pitch);
        tap.set_speed(tempo.speed());
        let mut output_adopted = false;
        let name = playback.output_device_name.clone();
        let (output, output_dead, output_device) =
            match open_output_named(&playback.output_device, name.as_deref(), &tempo, tx.clone()) {
                Ok(opened) => {
                    crate::audio_devices::clear_fallback();
                    if let (Some(_), Some(entry)) = (&playback.output_device, &opened.2) {
                        output_adopted = adopt_output_device(&mut playback, &entry.device);
                    }
                    opened
                }
                Err(e) => {
                    tracing::warn!("configured output device failed ({e}); using default");
                    let opened = open_output(&None, &tempo, tx.clone())?;
                    if let Some(requested_device) = playback.output_device.clone() {
                        crate::audio_devices::mark_fallback(requested_device.clone());
                        let _ = app.emit(
                            "audio:output-fallback",
                            crate::audio_devices::AudioOutputFallbackEvent {
                                requested_device: playback
                                    .output_device_name
                                    .clone()
                                    .unwrap_or(requested_device),
                            },
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
        let mut worker = Self {
            app,
            store,
            session,
            shared_state,
            shared_queue,
            output,
            tempo,
            speed,
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
            auto_dj_retry: None,
            recent_tracks: std::collections::VecDeque::new(),
            queue_undo: None,
            sleep_at: None,
            sleep_end_of_track: false,
            stop_after: StopAfter::Off,
            stop_at: None,
            stop_run: Vec::new(),
            stop_after_album: None,
            sleep_fade_duration: Duration::from_secs(30),
            sleep_fade_item: None,
            resume_ramp_until: None,
            sink: None,
            current_fade: None,
            pending_fade: None,
            current_tap: None,
            pending_tap: None,
            current_download: None,
            pending_download: None,
            pending_play_session: None,
            fading_out: None,
            draining: Vec::new(),
            last_output_rebuild: Instant::now(),
            stream_opened_at: Instant::now(),
            output_failed_pending: false,
            output_alive: true,
            output_dead,
            opened_output: None,
            last_default_poll: Instant::now(),
            default_output_failed: None,
            idle: IdleClock::default(),
            fade_in_arrival: false,
            parked: None,
            queue: Vec::new(),
            queue_revision: 0,
            persisted_queue: None,
            synced_revision: None,
            duplicate_count: 0,
            index: 0,
            status: PlaybackStatus::Idle,
            volume,
            volume_unsaved: false,
            generation: 0,
            prefetch_generation: 0,
            prefetched: None,
            opens: source::OpenTracker::default(),
            prefetch_inflight: None,
            prefetch_failed: None,
            adoptable_prefetch: None,
            appended: None,
            pending_pause: false,
            queue_ended: false,
            position_offset_ms: 0,
            lead_until: None,
            play_session_id: None,
            last_progress_report: Instant::now(),
            listen: scrobble::ListenClock::default(),
            cover_dir,
        };
        if output_adopted {
            worker.persist_playback();
        }
        worker.output_opened(output_device);
        Ok(worker)
    }

    /// The output now plays through `opened`: with per-device profiles on,
    /// its EQ/normalization and volume take over (see `audio_profiles.rs`).
    fn output_opened(&mut self, opened: Option<OutputEntry>) {
        // Called right after every new stream (start, rebuild, the reopen
        // on Play), and only then.
        self.stream_opened_at = Instant::now();
        // A stream opened while nothing plays (a device picked in Settings
        // during a long pause) gets its own while before it closes.
        self.idle.reset();
        // A volume not saved yet was set on the device that played until now.
        self.save_volume();
        crate::audio_devices::set_active_device(opened.as_ref().map(|o| o.device.clone()));
        self.opened_output = opened;
        self.apply_output_profile();
    }

    /// Switch to the profile of the device playing now, if it has one that
    /// differs from what plays.
    fn apply_output_profile(&mut self) {
        let Some(opened) = self.opened_output.clone() else {
            return;
        };
        let device = opened.device;
        let current = self.dsp.params();
        // A profile can follow the device from an id it had before, which
        // only a device no longer listed gives away.
        let listed = listed_output_ids();
        match crate::audio_profiles::on_device_opened(
            &self.store,
            &device,
            opened.legacy_name.as_deref(),
            listed.as_deref(),
            &current,
            self.volume,
        ) {
            Ok(Some(profile)) => {
                self.dsp.update(profile.dsp.clone());
                if let Ok(json) = serde_json::to_string(&profile.dsp) {
                    let _ = self.store.set(keys::AUDIO_DSP, &json);
                }
                if let Some(volume) = profile.volume {
                    self.apply_volume(volume);
                }
                let _ = self.app.emit(
                    "audio:profile-applied",
                    crate::audio_profiles::ProfileAppliedEvent {
                        device: device.name.clone(),
                    },
                );
                self.emit();
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("audio profile for '{}' not applied: {e}", device.name),
        }
    }

    /// Set the volume live. Saving it waits for [`Self::save_volume`].
    fn apply_volume(&mut self, volume: f32) {
        if !volume.is_finite() {
            return;
        }
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(sink) = &self.sink {
            sink.set_volume(self.volume);
        }
        if let Some((old, ..)) = &self.fading_out {
            old.set_volume(self.volume);
        }
        self.volume_unsaved = true;
    }

    /// Save a changed volume, also as the volume of the device playing. Runs
    /// on the tick, which waits for a pause in the commands: a volume drag
    /// sends one command per input event, and each save is a database commit
    /// or two.
    fn save_volume(&mut self) {
        if !self.volume_unsaved {
            return;
        }
        self.volume_unsaved = false;
        let _ = self.store.set(keys::VOLUME, &self.volume.to_string());
        if let Some(device) = crate::audio_devices::active_device() {
            let current = self.dsp.params();
            if let Err(e) =
                crate::audio_profiles::remember_volume(&self.store, &device, self.volume, &current)
            {
                tracing::warn!("cannot save the device volume: {e}");
            }
        }
    }

    fn handle(&mut self, cmd: PlayerCommand) {
        // A finished prefetch is stored before the boundary is resolved:
        // landing just as the current track ran out, it is exactly what the
        // boundary starts next. The other way round, the track would be opened
        // a second time and the ready source thrown away.
        let cmd = match cmd {
            PlayerCommand::SourceReady {
                generation,
                prefetch_generation,
                purpose: purpose @ OpenPurpose::Prefetch,
                track,
                play_session,
                result,
            } => {
                self.source_ready(
                    generation,
                    prefetch_generation,
                    purpose,
                    *track,
                    play_session,
                    result,
                );
                self.maybe_transition();
                return;
            }
            cmd => cmd,
        };
        // The audio may have crossed a track boundary since the last tick;
        // resolve it first so every command operates on a consistent
        // index/position pair (otherwise Prev double-skips, Pause freezes the
        // stale track for the whole pause, etc.).
        self.maybe_transition();
        match cmd {
            PlayerCommand::PlayQueue {
                tracks,
                start_index,
                in_order,
            } => {
                self.queue_undo = None;
                self.report_stopped_playing();
                *self.queue_mut() = tracks;
                self.index = start_index.min(self.queue.len().saturating_sub(1));
                // A fresh playthrough re-arms Auto-DJ.
                self.rearm_auto_dj();
                if self.shuffle {
                    if in_order {
                        // Shuffle stays on for what comes later; this queue's
                        // order is what was asked for.
                        let len = self.queue.len();
                        *self.order_mut() = (0..len).collect();
                    } else {
                        self.rebuild_order();
                    }
                }
                self.sync_queue();
                self.start_current();
            }
            PlayerCommand::RestoreQueue(restored) => self.restore_queue(restored),
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
                    let current = self.index;
                    let order = self.order_mut();
                    for v in order.iter_mut() {
                        if *v >= at {
                            *v += n;
                        }
                    }
                    let pos = order
                        .iter()
                        .position(|&i| i == current)
                        .map(|p| p + 1)
                        .unwrap_or(order.len());
                    order.splice(pos..pos, at..at + n);
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
                    let end = self.queue.len();
                    self.order_mut().extend(start..end);
                }
                self.sync_queue();
                self.emit();
            }
            PlayerCommand::RemoveAt { index, entry } => self.remove_at(index, &entry),
            PlayerCommand::JumpTo(i) => {
                if i < self.queue.len() {
                    self.queue_undo = None;
                    self.report_stopped_playing();
                    self.index = i;
                    self.rearm_auto_dj();
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
                        self.order_mut().clear();
                    }
                    self.persist_play_mode();
                    self.invalidate_next();
                    self.sync_queue();
                    self.emit();
                } else {
                    // Asked from a stale state: the answer is the real one.
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
                } else {
                    // Asked from a stale state: the answer is the real one.
                    self.emit();
                }
            }
            PlayerCommand::Play => {
                self.set_pending_pause(false);
                self.resume_or_start();
            }
            PlayerCommand::Pause => {
                if self.status == PlaybackStatus::Loading {
                    self.set_pending_pause(true);
                } else {
                    self.pause();
                }
            }
            PlayerCommand::Toggle => match self.status {
                PlaybackStatus::Playing => self.pause(),
                PlaybackStatus::Paused | PlaybackStatus::Idle => self.resume_or_start(),
                PlaybackStatus::Loading => self.set_pending_pause(!self.pending_pause),
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
                self.apply_volume(volume);
                self.emit();
            }
            PlayerCommand::SetPlaybackSettings {
                mut settings,
                reply,
            } => {
                let old_device = self.playback.output_device.clone();
                let old_name = self.playback.output_device_name.clone();
                // A Settings page that loaded before the configured device
                // turned up under a new id (or before its 0.6.0 name became
                // an id) still sends what it loaded. That means the device
                // playing now and is no switch: reopening it would only cut
                // the music for a moment.
                let stale = settings.output_device != old_device && self.means_playing(&settings);
                if stale {
                    settings.output_device = old_device.clone();
                    settings.output_device_name = old_name.clone();
                }
                let device_changed = settings.output_device != old_device;
                self.playback = settings;
                if stale {
                    // The command stored the request as it came.
                    self.persist_playback();
                }
                if device_changed && !self.try_rebuild_output(self.playback.output_device.clone()) {
                    // Keep playing on the still-healthy old output and revert
                    // the persisted choice so config and reality agree.
                    self.playback.output_device = old_device;
                    self.playback.output_device_name = old_name;
                    self.persist_playback();
                }
                if let Some(reply) = reply {
                    let _ = reply.send(self.playback.clone());
                }
            }
            PlayerCommand::RestoreOutput { ack } => {
                // Only a fallback has a configured device to go back to. The
                // user asked for it, so one that still fails says so.
                if crate::audio_devices::fallback_device().is_some() {
                    if let Some(device) = self.playback.output_device.clone() {
                        self.try_rebuild_output(Some(device));
                    }
                }
                let _ = ack.send(());
            }
            PlayerCommand::SoundImported { volume, profiles } => {
                if let Some(volume) = volume {
                    self.apply_volume(volume);
                    // The import saved it already. Imported profiles may
                    // override it; it must not be written into them first.
                    self.volume_unsaved = !profiles;
                }
                if profiles {
                    // The imported profiles decide what the device playing
                    // now sounds like: its own profile, or the imported sound
                    // becomes it.
                    self.apply_output_profile();
                } else if let Some(device) = crate::audio_devices::active_device() {
                    // The imported sound now belongs to the device playing,
                    // or its older profile would replace it at the next open.
                    let current = self.dsp.params();
                    if let Err(e) =
                        crate::audio_profiles::remember_dsp(&self.store, &device, &current)
                    {
                        tracing::warn!("cannot save the imported sound profile: {e}");
                    }
                    self.save_volume();
                }
                self.emit();
            }
            PlayerCommand::SetSpeed(setting) => {
                let old_speed = self.tempo.speed();
                // What played so far counts at the speed it played at.
                self.listen
                    .sample_at(Instant::now(), self.position_ms(), old_speed);
                self.speed = setting.clamped();
                self.tempo.set(self.speed.speed, self.speed.preserve_pitch);
                self.tap.set_speed(self.tempo.speed());
                // Deadlines on the clock that stand for track time (a
                // crossfade tail, the silence before a beat transition) move
                // with the speed.
                let now = Instant::now();
                let factor = old_speed / self.tempo.speed();
                let rescale = |at: Instant| now + at.saturating_duration_since(now).mul_f32(factor);
                if let Some((_, _, drop_at, _)) = &mut self.fading_out {
                    *drop_at = rescale(*drop_at);
                }
                self.lead_until = self.lead_until.map(rescale);
                self.speed.save(&self.store);
                // A sleep fade in progress was timed for the old speed.
                self.cancel_sleep_fade();
                self.update_sleep_fade();
                self.emit();
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
                // Armed after the next track was appended: an end-of-track
                // sleep now ends playback at that boundary.
                self.arm_appended();
                self.emit();
            }
            PlayerCommand::SetStopAfter { mode, entry } => {
                self.arm_stop_after(mode, entry);
                // Armed (or cleared) after the next track was appended: it
                // must not start at that boundary (or may again).
                self.arm_appended();
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
                // task that would find no client left. The music stops first:
                // the report waits for the server, and sign-out does not.
                // The installer hand-over comes this way too, and the process
                // ends right after it: a volume still waiting for its tick is
                // saved first, as shutdown does.
                self.save_volume();
                self.persist_position();
                let report = self.take_final_report(true);
                self.stop_internal();
                if let Some(report) = report {
                    self.send_final_report(report);
                }
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
            // A prefetch of the track that is loading anew may still play
            // (handle_track_end); anything else is a stale open from before a
            // queue change.
            if matches!(purpose, OpenPurpose::Prefetch)
                && self.adoptable_prefetch == Some(generation)
            {
                self.adopt_prefetch(track, play_session, result);
            }
            return;
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
                        } = self.refit(&track, *opened);
                        // Honor a pause pressed while this source was loading.
                        let paused = self.take_start_paused();
                        self.arm_fresh(&fade, 0, paused);
                        let sink = start_player(self.output.mix(), self.volume, paused, |player| {
                            player.append(source)
                        });
                        self.status = if paused {
                            PlaybackStatus::Paused
                        } else {
                            PlaybackStatus::Playing
                        };
                        self.sink = Some(sink);
                        self.current_fade = Some(fade);
                        self.current_tap = Some(tap_enabled);
                        self.current_download = Some(download);
                        self.position_offset_ms = 0;
                        // This open came first; a prefetch of the same track
                        // still on its way is dropped when it lands.
                        self.adoptable_prefetch = None;
                        self.last_progress_report = Instant::now();
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
                        } = self.refit(&track, *opened);
                        // Paused before the reopen, as adjusted by Play/Pause
                        // pressed while it loaded (see restart_at).
                        let paused = self.take_start_paused();
                        self.arm_fresh(&fade, start_ms, paused);
                        let sink = start_player(self.output.mix(), self.volume, paused, |player| {
                            player.append(source)
                        });
                        self.status = if paused {
                            PlaybackStatus::Paused
                        } else {
                            PlaybackStatus::Playing
                        };
                        self.sink = Some(sink);
                        self.current_fade = Some(fade);
                        self.current_tap = Some(tap_enabled);
                        self.current_download = Some(download);
                        self.position_offset_ms = start_ms;
                        self.listen.rebase(Instant::now(), start_ms);
                        if self.play_session_id.as_deref() == Some(play_session.as_str()) {
                            // Same play session continues — no new start report.
                            self.report_progress(paused);
                        } else {
                            // Opened at a position before it ever played (a
                            // seek while it was starting, or while nothing
                            // played): a new attempt, reported like any start.
                            self.last_progress_report = Instant::now();
                            self.report_start(&track, play_session);
                            if paused {
                                self.report_progress(true);
                            }
                            self.update_media_metadata(&track);
                        }
                        self.update_media_playback();
                        self.emit();
                    }
                    Err(e) => {
                        let reported =
                            self.play_session_id.as_deref() == Some(play_session.as_str());
                        self.reopen_failed(&track, e, reported);
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
                        // fresh Play open when the track actually starts —
                        // until then, the tick does not open it again.
                        tracing::warn!("prefetch failed: {e}");
                        self.prefetch_failed = Some(track.item_id);
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
        let order = match self.shuffle_mode {
            ShuffleMode::Off => Vec::new(),
            ShuffleMode::Tracks => build_shuffle_order(self.queue.len(), self.index, seed),
            ShuffleMode::Albums => build_album_shuffle_order(&self.queue, self.index, seed),
        };
        *self.order_mut() = order;
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
        self.prefetch_failed = None;
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
            if !self.resume_broken_stream() {
                self.handle_track_end();
            }
            return true;
        }
        false
    }

    /// The current source ran dry because its stream broke off — a live
    /// transcode cut by a dropped connection, a download that gave up after
    /// an outage (`source.rs`) — rather than at the end of its track: reopen
    /// it where it stopped, as a seek would, instead of skipping the rest.
    /// Not when it broke off before playing a second (a server that keeps
    /// failing ends the track as before), or in the last seconds of it.
    fn resume_broken_stream(&mut self) -> bool {
        const RESUME_MIN_PLAYED: Duration = Duration::from_secs(1);
        const RESUME_TAIL_MS: u64 = 2_000;
        if !self
            .current_download
            .as_ref()
            .is_some_and(|download| download.broke_off())
        {
            return false;
        }
        let silence = self
            .current_download
            .as_ref()
            .map_or(Duration::ZERO, |download| download.silence());
        let played = self
            .sink
            .as_ref()
            .map(|sink| sink.get_pos().saturating_sub(silence))
            .unwrap_or_default();
        let position_ms = self.position_ms();
        let near_end = self.queue.get(self.index).is_none_or(|track| {
            track.duration_ms == 0 || position_ms + RESUME_TAIL_MS >= track.duration_ms
        });
        if played < RESUME_MIN_PLAYED || near_end {
            return false;
        }
        tracing::warn!("stream broke off at {position_ms} ms; reopening it there");
        self.restart_at(position_ms);
        true
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
        self.follow_default_output();
        self.save_volume();
        // Crossfade tails are dropped once their ramp is over, and so are
        // players taken off the output.
        if let Some((_, _, drop_at, _)) = &self.fading_out {
            if Instant::now() >= *drop_at {
                self.fading_out = None;
            }
        }
        let now = Instant::now();
        self.draining.retain(|(_, drop_at)| now < *drop_at);
        // A timer that runs out during a pause has nothing left to do: the
        // music already stopped. Disarm it, so the next Play plays.
        if matches!(self.status, PlaybackStatus::Paused | PlaybackStatus::Idle)
            && self.sleep_timer_due()
        {
            self.disarm_sleep_timer();
            self.emit();
        }
        // A seek the decoder could not carry out leaves its source silent: the
        // track is opened anew there, as for a live transcode.
        if matches!(
            self.status,
            PlaybackStatus::Playing | PlaybackStatus::Paused
        ) {
            if let Some(target) = self
                .current_download
                .as_ref()
                .and_then(|download| download.take_seek_failure())
            {
                tracing::info!("the decoder could not seek; reopening the track there");
                self.restart_at(target.as_millis() as u64);
                return;
            }
        }
        if self.maybe_transition() || self.status != PlaybackStatus::Playing {
            return;
        }
        if self.sink.is_none() {
            return;
        }
        self.listen
            .sample_at(Instant::now(), self.position_ms(), self.tempo.speed());

        self.update_sleep_fade();

        // Sleep timer: pause when the deadline passes (unless the user asked
        // to finish the running track first — handled at the boundary).
        if self.sleep_timer_due() {
            self.sleep_at = None;
            self.sleep_fade_item = None;
            self.pause();
            return;
        }

        if self.last_progress_report.elapsed() >= REPORT_EVERY {
            self.last_progress_report = Instant::now();
            self.report_progress(false);
        }

        self.schedule_prefetch();
        self.maybe_auto_dj();
        self.update_media_playback();
        self.emit();
    }

    /// Start (or re-apply after pause/source change) the sample-time sleep
    /// ramp on the playing source. It ramps down from wherever the gain is —
    /// a crossfade fading in, a resume, a source that started at the fade's
    /// level — rather than jumping to the fade's level first. The sink's user
    /// volume is untouched, so cancelling can reliably restore the FadeSource
    /// gain to one.
    fn update_sleep_fade(&mut self) {
        if self.status != PlaybackStatus::Playing {
            return;
        }
        // A resume ramps back up to where the fade stands first
        // (resume_or_start); the first call after that ramp hands over.
        let resume_left = self
            .resume_ramp_until
            .map(|until| until.saturating_duration_since(Instant::now()));
        if resume_left.is_some_and(|left| left.is_zero()) {
            self.resume_ramp_until = None;
        }
        let current = self
            .queue
            .get(self.index)
            .map(|track| track.item_id.as_str());
        // Armed on the source playing now. A mark left by another source
        // does not count: that one is gone.
        let armed = current.is_some() && self.sleep_fade_item.as_deref() == current;
        let step = sleep_fade_step(armed, resume_left, self.sleep_ramp(self.position_ms()));
        let Some(fade) = &self.current_fade else {
            return;
        };
        match step {
            SleepFadeStep::Keep => return,
            // `over` is on the clock, a ramp runs in track time.
            SleepFadeStep::Rise { to, over } => fade.ramp_to(to, over.mul_f32(self.tempo.speed())),
            SleepFadeStep::Fall { over } => fade.ramp_to(0.0, over),
        }
        self.sleep_fade_item = current.map(str::to_string);
        // The next track, already appended, goes on at the fade's level.
        self.arm_appended();
    }

    /// Where the sleep fade-out stands with the current track at
    /// `position_ms`: the gain the music should have by now, and the rest of
    /// the ramp down to silence (track time). None outside the fade.
    fn sleep_ramp(&self, position_ms: u64) -> Option<(f32, Duration)> {
        let now = Instant::now();
        let remaining = if self.sleep_end_of_track {
            // A delayed end-of-track timer only starts fading once its wall
            // clock delay has elapsed.
            if self.sleep_at.is_some_and(|at| now < at) {
                return None;
            }
            let track = self.queue.get(self.index)?;
            if track.duration_ms == 0 {
                return None;
            }
            Duration::from_millis(track.duration_ms.saturating_sub(position_ms))
        } else {
            self.sleep_at?.saturating_duration_since(now)
        };
        if remaining.is_zero() {
            return None;
        }
        let gain = sleep_level(remaining, self.sleep_fade_duration)?;
        // The ramp runs in track time; a minutes timer counts on the clock.
        let ramp = if self.sleep_end_of_track {
            remaining
        } else {
            remaining.mul_f32(self.tempo.speed())
        };
        Some((gain, ramp))
    }

    /// Whether a source about to start comes in paused: a pause pressed while
    /// it loaded, or a minutes sleep timer that ran out meanwhile. The tick
    /// pauses at the deadline only while something plays, so a source landing
    /// after it would play at full volume until the next tick did. It starts
    /// paused instead, and the timer is done, as at its deadline.
    fn take_start_paused(&mut self) -> bool {
        let paused = std::mem::take(&mut self.pending_pause);
        if self.sleep_timer_due() {
            self.sleep_at = None;
            return true;
        }
        paused
    }

    /// Set the level a source starts at, before its first sample plays (a
    /// FadeSource takes the latest ramp then). Opened paused, it stays silent
    /// until the resume fades it in, like any other; reloaded to resume
    /// (`fade_in_arrival`), it fades in as a resume does; during a sleep fade
    /// it starts where the fade stands and carries on down. Otherwise it
    /// starts at full gain, as every source does.
    fn arm_fresh(&mut self, fade: &FadeHandle, position_ms: u64, paused: bool) {
        self.sleep_fade_item = None;
        let resumes = std::mem::take(&mut self.fade_in_arrival);
        if paused {
            fade.begin(0.0, 0.0, Duration::ZERO);
        } else if resumes {
            self.fade_in(Some(fade), position_ms);
        } else if let Some((gain, ramp)) = self.sleep_ramp(position_ms) {
            fade.begin(gain, 0.0, ramp);
            self.sleep_fade_item = self
                .queue
                .get(self.index)
                .map(|track| track.item_id.clone());
        }
    }

    /// Set the level the gapless-appended next track starts at. It is set
    /// ahead of time: the source takes the latest ramp at its first sample,
    /// and nothing of it is heard before. Silent when the boundary ends
    /// playback — an armed stop fires there, or an end-of-track sleep is due;
    /// at the level a minutes sleep fade will have reached by then; otherwise
    /// full.
    fn arm_appended(&self) {
        let Some(fade) = &self.pending_fade else {
            return;
        };
        let ends_playback = (self.sleep_end_of_track
            && self.sleep_at.is_none_or(|at| Instant::now() >= at))
            || self
                .queue
                .get(self.index)
                .is_some_and(|track| self.stop_after_fires(track));
        if ends_playback {
            fade.begin(0.0, 0.0, Duration::ZERO);
            return;
        }
        // A minutes timer: by the boundary, the clock has moved on by what is
        // left of the current track.
        let at_boundary = match (self.sleep_at, self.queue.get(self.index)) {
            (Some(at), Some(track)) if !self.sleep_end_of_track => {
                let left = track.duration_ms.saturating_sub(self.position_ms());
                Some(
                    at.saturating_duration_since(Instant::now())
                        .saturating_sub(self.tempo.wall(Duration::from_millis(left))),
                )
            }
            _ => None,
        };
        match at_boundary.and_then(|remaining| {
            sleep_level(remaining, self.sleep_fade_duration).map(|gain| (gain, remaining))
        }) {
            Some((gain, remaining)) => {
                fade.begin(gain, 0.0, remaining.mul_f32(self.tempo.speed()));
            }
            None => fade.begin(1.0, 1.0, Duration::ZERO),
        }
    }

    /// A minutes timer whose deadline has passed (an end-of-track timer
    /// fires at the boundary instead).
    fn sleep_timer_due(&self) -> bool {
        !self.sleep_end_of_track && self.sleep_at.is_some_and(|at| Instant::now() >= at)
    }

    fn disarm_sleep_timer(&mut self) {
        self.sleep_at = None;
        self.cancel_sleep_fade();
    }

    fn cancel_sleep_fade(&mut self) {
        if self.sleep_fade_item.take().is_some() {
            if let Some(fade) = &self.current_fade {
                fade.ramp_to(1.0, Duration::from_millis(PAUSE_FADE_MS));
            }
            // An appended next track was set to go on at the fade's level.
            self.arm_appended();
        }
    }

    /// Whether the armed stop fires when `finished` ends.
    fn stop_after_fires(&self, finished: &QueueTrack) -> bool {
        // Repeat-One replays the same entry: an album that keeps restarting
        // never reaches an end, and a track stop would fire on its own repeat.
        stop_after_due(
            self.stop_after,
            self.stop_at.as_ref(),
            self.stop_after_album.as_deref(),
            finished,
            self.repeat == RepeatMode::One,
        )
    }

    /// Arm "stop after" on the entry named (`None`: what plays now), or clear
    /// it. The stop is pinned to the entries it covers — the row itself, or
    /// its album's run in play order as the queue stands now — and ends after
    /// the last of them, so a track inserted later, a copy of the album
    /// elsewhere in the queue or Repeat All do not move where it ends.
    fn arm_stop_after(&mut self, mode: StopAfter, entry: Option<EntryRef>) {
        self.clear_stop_after();
        let armed = match entry {
            Some(entry) => self.queue.iter().position(|track| entry.matches(track)),
            None => (!self.queue.is_empty()).then_some(self.index),
        };
        let Some(armed) = armed else {
            return;
        };
        let run = match mode {
            StopAfter::Off => return,
            StopAfter::Track => vec![armed],
            StopAfter::Album => {
                // An album stop needs an album: a loose track has none, and
                // arming it would be a stop that never comes.
                let Some(album) = self.queue[armed]
                    .album_id
                    .clone()
                    .filter(|id| !id.is_empty())
                else {
                    return;
                };
                self.stop_after_album = Some(album);
                album_run(&self.queue, self.play_order(), armed)
            }
        };
        self.stop_after = mode;
        self.stop_run = run
            .into_iter()
            .map(|index| EntryRef::of(&self.queue[index]))
            .collect();
        self.stop_at = self.stop_run.last().cloned();
    }

    /// The normalization gain a source is opened with. Album mode prefers what
    /// the server says about the album, falls back to the album's tracks in
    /// the queue, and finally to the track's own gain — anything else would
    /// mean playing an album unnormalized because one server is older than
    /// another.
    ///
    /// Decided when the source is opened, so a change reaches the music with
    /// the next track. Rewrapping a playing source would mean reopening it,
    /// and a settings toggle is not worth a gap in the sound.
    fn normalization_gain_for(&self, track: &QueueTrack) -> Option<f32> {
        if !self.dsp.params().album_normalization {
            return track.normalization_gain;
        }
        track
            .album_normalization_gain
            .or_else(|| {
                track
                    .album_id
                    .as_deref()
                    .and_then(|album| album_gain_from_queue(&self.queue, album))
            })
            .or(track.normalization_gain)
    }

    fn clear_stop_after(&mut self) {
        self.stop_after = StopAfter::Off;
        self.stop_at = None;
        self.stop_run.clear();
        self.stop_after_album = None;
    }

    /// The gapless-appended next track took over at a boundary that is set
    /// to end playback: an end-of-track sleep that is due (once its delay,
    /// if any, has passed), or an armed stop — armed after the hand-off, so
    /// the append guard could not hold the track back. That track was kept
    /// silent for it (`arm_appended`); playback stops the way a plain track
    /// end stops (`handle_track_end`), positioned at the next track so resume
    /// continues naturally. Returns true when playback stopped.
    fn stop_at_appended_boundary(&mut self) -> bool {
        if self.sleep_end_of_track && self.sleep_at.is_none_or(|at| Instant::now() >= at) {
            self.sleep_at = None;
            self.sleep_end_of_track = false;
            self.sleep_fade_item = None;
        } else if self
            .queue
            .get(self.index)
            .is_some_and(|track| self.stop_after_fires(track))
        {
            self.clear_stop_after();
        } else {
            return false;
        }
        // Not onto itself under Repeat-One.
        if let Some(next) = self.next_index() {
            self.index = next;
        }
        self.sync_queue();
        self.stop_internal();
        true
    }

    /// Whether the coming boundary may be bridged at all: gapless-appended,
    /// crossfaded or beat-mixed. `schedule_prefetch` does that, and
    /// `next_wake` wakes up for it — both go by this.
    fn may_overlap_boundary(&self) -> bool {
        // A pending end-of-track sleep must reach a real track end: no
        // gapless append, no crossfade. Same for an armed stop that fires at
        // the coming boundary — the track has to end, not be overlapped. A
        // sleep fade in progress keeps the next track out of it, also once
        // its window is open but nothing is marked as fading yet.
        if self.sleep_end_of_track
            || self.sleep_fade_item.is_some()
            || self.sleep_ramp(self.position_ms()).is_some()
        {
            return false;
        }
        if self
            .queue
            .get(self.index)
            .is_some_and(|track| self.stop_after_fires(track))
        {
            return false;
        }
        // Repeat-One (and a single-track wrap) loops via handle_track_end, not a
        // gapless append of the same source — the identity guard can't
        // disambiguate a same-item_id "next".
        self.repeat != RepeatMode::One && self.auto_next_index() != Some(self.index)
    }

    /// Open the next track ahead of time and append it to the sink just
    /// before the boundary.
    fn schedule_prefetch(&mut self) {
        if !self.may_overlap_boundary() {
            return;
        }
        let next_idx = self.auto_next_index();
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

        let window_ms = if self.playback.beat_transitions {
            PREFETCH_OPEN_BEAT_MS
        } else {
            PREFETCH_OPEN_REMAINING_MS
        };
        // The window is time to get the next track ready, on the clock: at
        // 2x, 20 s of track pass in 10.
        let window_ms = (window_ms as f32 * self.tempo.speed().max(1.0)) as u64;
        if should_start_prefetch(
            self.prefetched.is_some() || self.prefetch_inflight.is_some(),
            self.appended.as_deref(),
            self.prefetch_failed.as_deref(),
            &next.item_id,
            duration,
            remaining,
            window_ms,
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
            // In time: a planned transition replaces the plain fade below
            // for as long as its moment has not passed.
            let position = self.position_ms();
            if let Some(plan) = self.transition_plan(&current, &next, effective_fade_ms) {
                if position + PLAN_LEAD_MS >= plan.start_at_ms {
                    if let Some(p) = self.prefetched.take() {
                        let lead = plan.start_at_ms.saturating_sub(position);
                        tracing::info!(
                            "beat transition into {}: at {position} + {lead} ms, fade out/in {}/{} ms",
                            p.item_id,
                            plan.fade_out_ms,
                            plan.fade_in_ms
                        );
                        let p = self.refit_prefetched(&next, p);
                        self.begin_crossfade(p, plan.fade_out_ms, plan.fade_in_ms, lead);
                    }
                }
                return;
            }
            // Crossfade: overlap the tracks on two players instead of the
            // gapless same-sink hand-off.
            if remaining > 0 && remaining <= effective_fade_ms {
                if let Some(p) = self.prefetched.take() {
                    if p.item_id == next.item_id {
                        let fade_ms = remaining.min(effective_fade_ms);
                        let p = self.refit_prefetched(&next, p);
                        self.begin_crossfade(p, fade_ms, fade_ms, 0);
                    } else {
                        self.prefetched = Some(p);
                    }
                }
            }
        } else if self.appended.is_none() && duration > 0 && remaining <= APPEND_REMAINING_MS {
            if let Some(p) = self.prefetched.take() {
                if p.item_id == next.item_id {
                    // Coupled to the playing track, at the output's rate.
                    // Replacing the output drops the prefetch, so the rate
                    // only guards.
                    let p = self.couple_prefetched(&next, p);
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

    /// The beat-aligned transition into the prefetched `next`, when the
    /// setting is on and both tracks have been analysed (see analysis.rs) —
    /// and only while it can still be started: a plan whose moment passed by
    /// more than PLAN_LATE_MS is dropped for a plain crossfade, and nothing
    /// may wait for it any more (`next_wake`).
    fn transition_plan(
        &self,
        current: &QueueTrack,
        next: &QueueTrack,
        fade_ms: u64,
    ) -> Option<analysis::TransitionPlan> {
        if !self.playback.beat_transitions
            || self.prefetched.as_ref().map(|p| p.item_id.as_str()) != Some(next.item_id.as_str())
        {
            return None;
        }
        let a = self
            .waveform
            .analyses
            .get_matching(&current.item_id, current.duration_ms)?;
        let b = self
            .waveform
            .analyses
            .get_matching(&next.item_id, next.duration_ms)?;
        let position = self.position_ms();
        analysis::plan_transition(&a, &b, fade_ms)
            .filter(|plan| position <= plan.start_at_ms + PLAN_LATE_MS)
    }

    /// How long the command loop may sleep: a tick, or less when a planned
    /// transition is due before the next one. Only for a plan the tick will
    /// start: one it never starts (a stop or a sleep ends this track, the
    /// moment has passed) would keep the loop waking every millisecond until
    /// the track ends.
    fn next_wake(&self) -> Duration {
        if self.status != PlaybackStatus::Playing
            || !self.playback.beat_transitions
            || !self.may_overlap_boundary()
        {
            return TICK;
        }
        let Some(current) = self.queue.get(self.index) else {
            return TICK;
        };
        let Some(next) = self.auto_next_index().and_then(|i| self.queue.get(i)) else {
            return TICK;
        };
        let fade_ms = (self.playback.crossfade_seconds.clamp(1.0, 12.0) * 1000.0) as u64;
        if !should_crossfade(self.playback.crossfade_mode, fade_ms, current, next) {
            return TICK;
        }
        let Some(plan) = self.transition_plan(current, next, fade_ms) else {
            return TICK;
        };
        let until = plan
            .start_at_ms
            .saturating_sub(PLAN_LEAD_MS)
            .saturating_sub(self.position_ms());
        // Track time; the clock runs it at the playback speed.
        let until = self.tempo.wall(Duration::from_millis(until));
        if until >= TICK {
            TICK
        } else {
            // Due already (a command kept the loop busy): tick right away.
            until.max(Duration::from_millis(1))
        }
    }

    /// Fade the current track out while the next fades in on a fresh player;
    /// the queue pointer moves immediately (the new track is what's "current").
    /// `lead_ms` of silence go in front of the next track, so a planned
    /// transition starts on the sample rather than on the thread wake-up.
    fn begin_crossfade(&mut self, p: Prefetched, fade_out_ms: u64, fade_in_ms: u64, lead_ms: u64) {
        let play_session = p.play_session;
        // Where the outgoing track is let go. Its report waits until the next
        // track is in the output: it writes the play history, and the lead of
        // a planned transition was measured before this call.
        let outgoing = self
            .queue
            .get(self.index)
            .cloned()
            .map(|track| (track, self.position_ms()));
        // The tail must stay steerable (pause/seek during the fade) and stop
        // feeding the visualizer — only one track drives the visuals.
        if let Some(tap) = self.current_tap.take() {
            tap.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        if let (Some(fade), Some(old)) = (self.current_fade.take(), self.sink.take()) {
            let fade_ms = fade_out_ms + lead_ms;
            fade.ramp_to(0.0, Duration::from_millis(fade_ms));
            self.fading_out = Some((
                old,
                fade,
                Instant::now() + self.tempo.wall(Duration::from_millis(fade_ms + 500)),
                self.current_download.take(),
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

        let lead = (lead_ms > 0).then(|| {
            // The player reports positions per source: the track's own
            // position starts at zero once the silence is through.
            self.lead_until =
                Some(Instant::now() + self.tempo.wall(Duration::from_millis(lead_ms)));
            use rodio::Source as _;
            rodio::source::Zero::new(p.source.channels(), p.source.sample_rate())
                .take_duration(Duration::from_millis(lead_ms))
        });
        p.fade.begin(0.0, 1.0, Duration::from_millis(fade_in_ms));
        let sink = start_player(self.output.mix(), self.volume, false, |player| {
            if let Some(silence) = lead {
                player.append(silence);
            }
            player.append(p.source);
        });
        self.sink = Some(sink);
        self.current_fade = Some(p.fade);
        self.sleep_fade_item = None;
        self.current_tap = Some(p.tap_enabled);
        self.current_download = Some(p.download);
        self.position_offset_ms = 0;
        if let Some((prev, position_ms)) = outgoing {
            self.report_stopped(&prev, position_ms);
        }
        self.listen.restart(Instant::now(), 0);
        self.appended = None;

        self.queue_undo = None;
        self.index = self.auto_next_index().unwrap_or(self.index);
        self.sync_queue();
        if let Some(track) = self.queue.get(self.index).cloned() {
            self.last_progress_report = Instant::now();
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
        self.report_decoder_crash();
        self.appended = None;
        // The appended source is what plays now, whatever follows: its fade
        // is the one to steer (a stop or a cut-over below ramps it out).
        self.current_fade = self.pending_fade.take();
        let finished = self.queue.get(self.index).cloned();
        if let Some(prev) = finished.as_ref() {
            self.report_stopped(prev, prev.duration_ms);
        }
        let invalidated_undo = self.queue_undo.take().is_some();
        if self.stop_at_appended_boundary() {
            return;
        }
        let target = self.auto_next_index();
        let next_matches = target
            .and_then(|i| self.queue.get(i))
            .is_some_and(|t| t.item_id == appended_item_id);
        if next_matches {
            self.index = target.unwrap();
            // The appended source plays its track from the very beginning.
            self.position_offset_ms = 0;
            self.listen.restart(Instant::now(), self.position_ms());
            // A sleep fade that was running went over to the appended source
            // (arm_appended): that is the one fading now.
            if self.sleep_fade_item.is_some() {
                self.sleep_fade_item = self
                    .queue
                    .get(self.index)
                    .map(|track| track.item_id.clone());
            }
            self.current_tap = self.pending_tap.take();
            self.current_download = self.pending_download.take();
            self.sync_queue();
            if let Some(track) = self.queue.get(self.index).cloned() {
                self.last_progress_report = Instant::now();
                let play_session = self
                    .pending_play_session
                    .take()
                    .unwrap_or_else(new_play_session);
                self.report_start(&track, play_session);
                self.update_media_metadata(&track);
            }
            self.update_media_playback();
            self.emit();
        } else {
            // The queue was edited after the hand-off (next track removed,
            // moved, or something inserted before it): the appended audio no
            // longer matches the play order. The finished track is still at
            // self.index, so cut over to whatever now follows it — from its
            // prefetch when that is ready already.
            if let Some(n) = self.auto_next_index() {
                self.index = n;
                self.sync_queue();
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
    }

    /// Current track finished with nothing appended (prefetch missed or was
    /// disabled): fall back to a regular start of the next track.
    fn handle_track_end(&mut self) {
        self.report_decoder_crash();
        // Where it really stopped. A decoder ends on any read error, so a
        // stream that broke off ends the track early and looks like a natural
        // end otherwise — reported at its full length, the server would count
        // it as played through.
        let stopped_at = self.position_ms();
        let download = self
            .current_download
            .as_ref()
            .and_then(|download| download.fraction());
        // A decoder that crashed (or came to a rate that cannot be
        // resampled) ended the track, whatever the download did: said so
        // above, and the queue goes on.
        let crashed = self
            .current_download
            .as_ref()
            .is_some_and(|download| download.decoder_failed());
        if let Some(prev) = self.queue.get(self.index).cloned() {
            self.report_stopped(&prev, stopped_at);
            if !crashed && stream_broke_off(stopped_at, prev.duration_ms, download) {
                // Not on to the next track as if nothing happened: stop on
                // this one, where it broke off, so Play continues it there.
                tracing::warn!("the stream broke off at {stopped_at} ms");
                let _ = self.app.emit(
                    "player:stream-broke-off",
                    StreamBrokeOffEvent { name: &prev.name },
                );
                self.stop_internal();
                self.position_offset_ms = stopped_at;
                self.emit();
                return;
            }
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
        if self
            .queue
            .get(self.index)
            .is_some_and(|t| self.stop_after_fires(t))
        {
            self.clear_stop_after();
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
                // Its prefetch may still be on the way, nearly done: the
                // fresh open is the backup, and whichever lands first plays.
                let in_flight = self
                    .queue
                    .get(n)
                    .is_some_and(|track| self.prefetch_inflight.as_deref() == Some(&track.item_id));
                let generation = self.generation;
                self.start_current();
                if in_flight {
                    self.adoptable_prefetch = Some(generation);
                }
            }
        } else {
            if invalidated_undo {
                self.sync_queue();
            }
            self.stop_internal();
            self.queue_ended = true;
        }
    }

    /// The decoder of the track that just ended crashed (on its own thread,
    /// see `source/feed.rs`): it ended early. Said like a failed start; the
    /// queue goes on.
    fn report_decoder_crash(&self) {
        let failure = self
            .current_download
            .as_ref()
            .and_then(|download| download.decoder_failure());
        if let (Some(failure), Some(track)) = (failure, self.queue.get(self.index)) {
            let e = AppError::Audio(failure);
            self.report_error(PlayerErrorEvent::cannot_play(&track.name, &e));
        }
    }

    /// The prefetch that was still on its way when the track before it ended
    /// landed before the fresh open of the same track (`handle_track_end`):
    /// it plays, and the other open is dropped when it lands, the generation
    /// having moved on. A failed one changes nothing — the fresh open is
    /// still coming.
    fn adopt_prefetch(
        &mut self,
        track: QueueTrack,
        play_session: String,
        result: AppResult<Box<OpenedSource>>,
    ) {
        let Ok(opened) = result else {
            return;
        };
        if self.status != PlaybackStatus::Loading
            || self.queue.get(self.index).map(|t| t.item_id.as_str())
                != Some(track.item_id.as_str())
        {
            return;
        }
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
        self.start_from_prefetched();
    }

    /// [`Self::refit`] for a prefetched source, which starts at the top.
    fn refit_prefetched(&self, track: &QueueTrack, p: Prefetched) -> Prefetched {
        let Prefetched {
            item_id,
            play_session,
            source,
            fade,
            tap_enabled,
            download,
        } = p;
        let opened = self.refit(
            track,
            OpenedSource {
                source,
                fade,
                tap_enabled,
                download,
                start_ms: 0,
            },
        );
        Prefetched {
            item_id,
            play_session,
            source: opened.source,
            fade: opened.fade,
            tap_enabled: opened.tap_enabled,
            download: opened.download,
        }
    }

    /// The prefetched next track, coupled to the playing one for the gapless
    /// hand-over (`source::couple`), then [`Self::refit_prefetched`] — which
    /// leaves a coupled one as it is: it is at the output's rate.
    fn couple_prefetched(&self, track: &QueueTrack, p: Prefetched) -> Prefetched {
        if let Some(current) = &self.current_download {
            source::couple(current, &p.download, self.output.rate());
        }
        self.refit_prefetched(track, p)
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
        // Opened for an output replaced since, it may be at another rate.
        let p = self.refit_prefetched(&track, p);
        self.generation = self.generation.wrapping_add(1);
        // The old player may still sound: an appended track the queue has
        // moved away from (handle_transition).
        self.release_sink();
        self.prefetch_inflight = None;
        self.prefetch_failed = None;
        self.adoptable_prefetch = None;
        self.appended = None;
        self.pending_fade = None;
        self.pending_tap = None;
        self.pending_download = None;
        self.pending_play_session = None;
        // Started out of a load (adopt_prefetch), a pause pressed meanwhile
        // holds; at a track end there is none, but a sleep timer may have
        // run out.
        let paused = self.take_start_paused();
        self.arm_fresh(&p.fade, 0, paused);
        let sink = start_player(self.output.mix(), self.volume, paused, |player| {
            player.append(p.source)
        });
        self.sink = Some(sink);
        self.current_fade = Some(p.fade);
        self.current_tap = Some(p.tap_enabled);
        self.current_download = Some(p.download);
        self.position_offset_ms = 0;
        self.listen.restart(Instant::now(), 0);
        self.status = if paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        self.last_progress_report = Instant::now();
        self.report_start(&track, p.play_session);
        if paused {
            self.report_progress(true);
        }
        self.update_media_metadata(&track);
        self.update_media_playback();
        self.emit();
        true
    }

    /// Whether the loading source arrives paused (Play/Pause pressed while it
    /// loads). Shown at once, so the button says what the arrival will do.
    fn set_pending_pause(&mut self, paused: bool) {
        if self.pending_pause == paused {
            return;
        }
        self.pending_pause = paused;
        // The media overlay too: it still shows what it did before the load.
        self.update_media_playback();
        self.emit();
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
            // Not started yet (a crossed boundary was resolved before this):
            // it starts silent should the boundary come during the sleep. A
            // ramp would start it at full level, and the pause would then cut
            // it off mid-ramp.
            if let Some(fade) = &self.pending_fade {
                fade.begin(0.0, 0.0, Duration::ZERO);
            }
            if let Some((_, fade, ..)) = &self.fading_out {
                fade.ramp_to(0.0, Duration::from_millis(PAUSE_FADE_MS));
            }
            std::thread::sleep(self.tempo.wall(Duration::from_millis(PAUSE_FADE_MS + 20)));
            // The track boundary may have been crossed while we slept;
            // resolve it so index/position/fade handles are current.
            self.maybe_transition();
            match self.status {
                PlaybackStatus::Playing => {}
                PlaybackStatus::Loading => {
                    // Transition cut over to a fresh open: pause it on arrival.
                    self.set_pending_pause(true);
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
        self.persist_position();
        self.update_media_playback();
        self.emit();
    }

    /// Save where the current track stands along with the index, so a
    /// restart resumes there. Written when playback stops for a while — a
    /// pause, quitting, the installer taking over — not while it plays: the
    /// index entry is small, but not worth a write every tick.
    fn persist_position(&mut self) {
        self.persist_queue();
        let Some(written) = self.persisted_queue.as_ref() else {
            return;
        };
        let entry = PersistedQueueIndex {
            snapshot_id: written.snapshot_id.clone(),
            index: self.index,
            position_ms: Some(self.position_ms()),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            let _ = self.store.set(keys::QUEUE_INDEX, &json);
        }
    }

    fn resume_or_start(&mut self) {
        match self.status {
            PlaybackStatus::Paused => {
                // Ran out during the pause (the tick disarms it, but a Play
                // can come first): it is done, and Play means play.
                if self.sleep_timer_due() {
                    self.disarm_sleep_timer();
                }
                // Its player went with the output, closed after a long pause.
                let Some(sink) = &self.sink else {
                    self.resume_closed();
                    return;
                };
                sink.play();
                // Up from silence, said so rather than ramped from the current
                // gain: pause() left it at zero, but a source that was opened
                // paused has never been pulled, so it has taken no ramp yet
                // and would still start at full gain.
                let fade = self.current_fade.clone();
                self.fade_in(fade.as_ref(), self.position_ms());
                // The appended next source was muted by pause() too.
                self.arm_appended();
                self.status = PlaybackStatus::Playing;
                self.report_progress(false);
                self.update_media_playback();
                self.emit();
            }
            PlaybackStatus::Idle => {
                // Ran out while nothing played: as above. Left armed, it would
                // pause the track the moment it lands (take_start_paused).
                if self.sleep_timer_due() {
                    self.disarm_sleep_timer();
                }
                // Nothing loaded in the sink yet — start the current queue
                // position (e.g. after a restored session), where a seek made
                // meanwhile put it (seek_ms).
                if !self.queue.is_empty() {
                    self.start_current_at(self.position_offset_ms);
                }
            }
            PlaybackStatus::Playing | PlaybackStatus::Loading => {}
        }
    }

    /// Fade the source behind `fade` in from silence over the resume's ramp:
    /// up to where a sleep fade stands by now, not to full volume — pause()
    /// replaced the sleep ramp with its own, and a source reloaded to resume
    /// has none yet. The sleep fade takes over from there once this ramp is
    /// done (update_sleep_fade, on a later tick).
    fn fade_in(&mut self, fade: Option<&FadeHandle>, position_ms: u64) {
        let sleep = self.sleep_ramp(position_ms);
        let level = sleep.map_or(1.0, |(gain, _)| gain);
        if let Some(fade) = fade {
            fade.begin(0.0, level, Duration::from_millis(PAUSE_FADE_MS));
        }
        // Brought back below full by the sleep fade, it carries the fade
        // already: cancelling it brings the gain back up.
        self.sleep_fade_item = sleep
            .and_then(|_| self.queue.get(self.index))
            .map(|track| track.item_id.clone());
        self.resume_ramp_until =
            Some(Instant::now() + self.tempo.wall(Duration::from_millis(PAUSE_FADE_MS + 50)));
    }

    /// Play a paused track whose player went with the output when that
    /// closed for silence (`close_idle_output`): open the output again and
    /// bring the track back where it was paused, in the same play session —
    /// from its parked decoder and download when it kept them (`unpark`,
    /// no server needed), else reloaded (`restart_at`). It fades in from
    /// silence like any resume (`arm_fresh`). An output that does not open,
    /// or a reload that fails (`reopen_failed`), leaves it paused there in
    /// its session, for the next Play to try again.
    fn resume_closed(&mut self) {
        // The installer is about to end the process (see start_current_at).
        if crate::updater::handing_over() {
            self.emit();
            return;
        }
        if self.queue.get(self.index).is_none() {
            self.stop_internal();
            return;
        }
        if !self.output.is_open() || !self.output_alive {
            if let Err(e) = self.reopen_output() {
                tracing::error!("cannot open audio output: {e}");
                self.report_error(PlayerErrorEvent::output_failed(None, &e));
                return;
            }
        }
        let position_ms = self.position_ms();
        // Loading to play: restart_at would bring a paused track back
        // paused.
        self.status = PlaybackStatus::Loading;
        self.fade_in_arrival = true;
        match self.unpark(position_ms) {
            Some(resumed) => {
                tracing::info!("resuming the paused track from its kept download");
                self.reopen_at(position_ms, Some(resumed), position_ms);
            }
            None => {
                tracing::info!("reloading the paused track to resume it");
                self.restart_at(position_ms);
            }
        }
    }

    /// The parked source of the current track, at `position_ms` on the
    /// output as it is now (see `source::Parked::resume`). Taken either way:
    /// what does not resume from it is opened anew.
    fn unpark(&mut self, position_ms: u64) -> Option<OpenedSource> {
        let parked = self.parked.take()?;
        let track = self.queue.get(self.index)?;
        if !parked.holds(track) {
            return None;
        }
        parked.source.resume(
            position_ms,
            self.output.rate(),
            self.dsp.clone(),
            self.normalization_gain_for(track),
            self.tap.clone(),
        )
    }

    /// Let go of a parked source once it no longer belongs to a paused track
    /// without a player (another track, a stop, a reload): its download and
    /// its decoder thread go with it. Checked after every command and tick,
    /// rather than on every way out of that state.
    fn drop_stale_parked(&mut self) {
        let Some(parked) = &self.parked else {
            return;
        };
        if !keeps_parked(
            self.status,
            self.sink.is_some(),
            self.queue.get(self.index).is_some_and(|t| parked.holds(t)),
        ) {
            tracing::debug!("the parked track is let go of");
            self.parked = None;
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

    fn remove_at(&mut self, i: usize, entry: &EntryRef) {
        if !self.queue.get(i).is_some_and(|track| entry.matches(track)) {
            return;
        }
        self.remember_queue_undo();
        self.invalidate_next();
        let removing_current = i == self.index;
        // Where playback continues if we remove the current track: the play-
        // order successor (under shuffle that's not the physical neighbor),
        // captured before the mutation reindexes everything — and only if it
        // was going on.
        let landing = if removing_current {
            landing_after_removal(
                self.queue.len(),
                i,
                self.play_order(),
                self.repeat == RepeatMode::All,
            )
        } else {
            None
        };
        let resume = landing.is_some_and(|(_, onward)| onward)
            && plays_on_after_removal(self.status, self.pending_pause);
        // Its player goes when playback continues or stops below.
        if removing_current {
            self.report_stopped_playing();
        }
        self.queue_mut().remove(i);
        if self.shuffle {
            let order = self.order_mut();
            order.retain(|&v| v != i);
            for v in order.iter_mut() {
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
            if let Some((landing, _)) = landing {
                self.index = landing;
            }
            if resume {
                self.sync_queue();
                self.start_current();
                return;
            }
            // Everything of the removed track goes with it: its position and
            // buffered range would show on the entry that took its slot.
            self.stop_internal();
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
            let order = self.order_mut();
            let entry = order.remove(from);
            order.insert(to, entry);
            self.sync_queue();
            self.emit();
            return;
        }
        let queue = self.queue_mut();
        let track = queue.remove(from);
        queue.insert(to, track);
        if self.shuffle {
            for v in self.order_mut().iter_mut() {
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
        // The listen, the resume point and a reload's fade go with the stop.
        self.stop_internal();
        // And the Jellyfin play session, which every stop path other than
        // this one has reported (and so taken) first. A sign-out did too
        // (`StopAck`), but a session the backend ended skips the sign-out:
        // the next account would report its own track stopped under the old
        // account's session, to its own server.
        self.play_session_id = None;
        self.pending_play_session = None;
        self.queue_mut().clear();
        self.index = 0;
        self.order_mut().clear();
        self.rearm_auto_dj();
        self.queue_undo = None;
        self.recent_tracks.clear();
        self.clear_media_metadata();
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
        self.order_mut().clear();
        self.rearm_auto_dj();
        self.clear_media_metadata();
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
            let order = remap_order_after_removals(&self.order, &indices);
            *self.order_mut() = order;
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
        // A paused track whose player went with the output closed for
        // silence still holds its place.
        let restart = live_current != restored_current
            || (self.sink.is_none()
                && self.status != PlaybackStatus::Paused
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
        *self.order_mut() = if self.shuffle { undo.order } else { Vec::new() };
        self.last_auto_dj_seed = undo.last_auto_dj_seed;
        self.invalidate_next();
        self.sync_queue();

        if restart && !self.queue.is_empty() {
            match undo.status {
                PlaybackStatus::Playing | PlaybackStatus::Loading => self.start_current(),
                PlaybackStatus::Paused => {
                    self.start_current();
                    self.set_pending_pause(true);
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
        self.start_current_at(0);
    }

    /// [`Self::start_current`], `position_ms` into the track: where a seek
    /// made while nothing played has put it.
    fn start_current_at(&mut self, position_ms: u64) {
        // The installer is about to end the process (`updater::hand_over`,
        // which sends the final reports): a track started now would be cut
        // off at once. The queue keeps what was asked for.
        if crate::updater::handing_over() {
            self.emit();
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.release_sink();
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.prefetch_failed = None;
        self.adoptable_prefetch = None;
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
        self.fade_in_arrival = false;
        self.position_offset_ms = position_ms;
        self.listen.restart(Instant::now(), position_ms);
        // A new attempt: the callers report the previous one stopped first.
        self.play_session_id = None;
        // The output may have closed for silence, or died while idle: bring
        // it back before playing.
        if !self.output.is_open() || !self.output_alive {
            if let Err(e) = self.reopen_output() {
                tracing::error!("cannot open audio output: {e}");
                self.report_error(PlayerErrorEvent::output_failed(None, &e));
                self.status = PlaybackStatus::Idle;
                self.emit();
                return;
            }
        }
        let Some(track) = self.queue.get(self.index).cloned() else {
            self.stop_internal();
            return;
        };
        self.status = PlaybackStatus::Loading;
        self.emit();
        // Started at a position, the open is positioned like a reopen; it is
        // still a new attempt (source_ready reports its start).
        let purpose = if position_ms > 0 {
            OpenPurpose::SeekRestart { position_ms }
        } else {
            OpenPurpose::Play
        };
        self.spawn_open(purpose, track);
    }

    fn stop_internal(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.release_sink();
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.prefetch_failed = None;
        self.adoptable_prefetch = None;
        self.appended = None;
        self.pending_pause = false;
        self.queue_ended = false;
        self.pending_fade = None;
        self.current_fade = None;
        self.pending_tap = None;
        self.current_tap = None;
        self.pending_download = None;
        self.current_download = None;
        self.fade_in_arrival = false;
        self.position_offset_ms = 0;
        // Every stop path reports first, so nothing measured is lost here.
        self.listen = scrobble::ListenClock::default();
        self.status = PlaybackStatus::Idle;
        self.update_media_playback();
        self.emit();
    }

    /// Take the current player off the output without a click. Dropping a
    /// player cuts its sound off mid-waveform, so it ramps to silence and a
    /// later tick drops it; an appended next source in it must not start
    /// meanwhile, and a crossfade tail goes the same way. Paused or played
    /// out, it is silent already and goes at once.
    fn release_sink(&mut self) {
        let ramp = Duration::from_millis(RELEASE_FADE_MS);
        let drop_at = Instant::now()
            + self
                .tempo
                .wall(Duration::from_millis(RELEASE_FADE_MS + 100));
        if let Some(sink) = self.sink.take() {
            if !sink.is_paused() && !sink.empty() {
                if let Some(fade) = &self.current_fade {
                    fade.ramp_to(0.0, ramp);
                }
                if let Some(fade) = &self.pending_fade {
                    fade.begin(0.0, 0.0, Duration::ZERO);
                }
                // Only one track drives the visuals, and that is the next.
                for tap in [&self.current_tap, &self.pending_tap].into_iter().flatten() {
                    tap.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                self.draining.push((sink, drop_at));
            }
        }
        if let Some((tail, fade, _, _)) = self.fading_out.take() {
            fade.ramp_to(0.0, ramp);
            self.draining.push((tail, drop_at));
        }
    }

    fn fail_start(&mut self, track: &QueueTrack, e: AppError) {
        // No title in the log: the diagnostics export promises no track
        // metadata, and its quote redaction trips over apostrophes.
        tracing::error!("failed to start a track: {e}");
        self.report_error(PlayerErrorEvent::cannot_play(&track.name, &e));
        // Nothing plays: what the failed attempt left behind goes, like on
        // any stop. Where it was to start stays, as the target an idle
        // player starts at (seek_ms): Play tries there again, the seek bar
        // keeps showing it, and Prev or a seek move it.
        let start_at = self.position_offset_ms;
        self.stop_internal();
        if start_at > 0 {
            self.position_offset_ms = start_at;
            self.emit();
        }
    }

    /// Open a track's stream + decoder on a short-lived background thread and
    /// deliver the result back through the command channel.
    fn spawn_open(&mut self, purpose: OpenPurpose, track: QueueTrack) {
        let Some(client) = self.session_client() else {
            // Nothing will answer, and a reopen has dropped its player
            // already (restart_at): left alone it would wait in Loading for
            // good. It fails the way a reopen that did not open does.
            match purpose {
                OpenPurpose::Prefetch => {}
                OpenPurpose::SeekRestart { .. } => {
                    let reported = self.play_session_id.is_some();
                    self.reopen_failed(&track, AppError::NotConnected, reported);
                }
                OpenPurpose::Play => self.fail_start(&track, AppError::NotConnected),
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
        // The output it is for. Should that be replaced by one at another
        // rate before the source lands, it is moved over then (`refit`).
        let rate = self.output.rate();
        let gain_db = self.normalization_gain_for(&track);
        let dsp = self.dsp.clone();
        let tap = self.tap.clone();
        let waveform = self.waveform.clone();
        let generation = self.generation;
        let prefetch_generation = self.prefetch_generation;
        let ticket = self.opens.start(
            generation,
            matches!(purpose, OpenPurpose::Prefetch).then_some(prefetch_generation),
        );
        let tx = self.tx.clone();
        let app = self.app.clone();
        let session = play_session.clone();
        let spawned = std::thread::Builder::new()
            .name("sonarune-open".into())
            .spawn(move || {
                let stop = ticket.stop_token();
                // A decoder panicking on a malformed file (symphonia on a WAV
                // header with sample rate 0) must still answer, or the player
                // waits in Loading for good.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    open_track_renewing(
                        auth,
                        &track,
                        source::Attempt {
                            play_session: &session,
                            gain_db,
                            start_ms,
                            rate,
                            stop: stop.clone(),
                        },
                        dsp,
                        tap,
                        Some(waveform),
                        |unauthorized| renew_stream_auth(&app, &client, unauthorized, &stop),
                    )
                }))
                .unwrap_or_else(|_| Err(AppError::Audio("the decoder crashed".into())))
                .map(Box::new);
                ticket.delivered();
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
            let e = AppError::Audio("cannot start opener thread".into());
            match (purpose, self.queue.get(self.index).cloned()) {
                (OpenPurpose::SeekRestart { .. }, Some(track)) => {
                    let reported = self.play_session_id.is_some();
                    self.reopen_failed(&track, e, reported);
                }
                (OpenPurpose::SeekRestart { .. }, None) if self.play_session_id.is_some() => {
                    self.report_stopped_playing();
                }
                (OpenPurpose::Play, Some(track)) => self.fail_start(&track, e),
                _ => {}
            }
        }
    }

    /// A reopen at a position (`OpenPurpose::SeekRestart`) did not open;
    /// `reported`: the play session it continues was reported started. See
    /// [`failed_reopen`] for what that leaves.
    fn reopen_failed(&mut self, track: &QueueTrack, e: AppError, reported: bool) {
        let resuming = std::mem::take(&mut self.fade_in_arrival);
        match failed_reopen(resuming, reported) {
            FailedReopen::StayPaused => {
                // No title in the log, as in fail_start.
                tracing::error!("failed to resume a track: {e}");
                self.report_error(PlayerErrorEvent::cannot_play(&track.name, &e));
                // As restart_at left it: no player, the target as the
                // position, the session and what was heard of it untouched.
                self.pending_pause = false;
                self.status = PlaybackStatus::Paused;
                self.update_media_playback();
                self.emit();
            }
            FailedReopen::CloseSession => {
                self.report_stopped_playing();
                self.fail_start(track, e);
            }
            FailedReopen::Fail => self.fail_start(track, e),
        }
    }

    /// Stop the downloads of opens whose result `source_ready` would now
    /// throw away. Their threads may be waiting for a stalled server, and
    /// would otherwise keep downloading what nobody plays.
    fn stop_stale_opens(&mut self) {
        self.opens.stop_stale(
            self.generation,
            self.prefetch_generation,
            self.adoptable_prefetch,
        );
    }

    fn session_client(&self) -> Option<Arc<JellyfinClient>> {
        // Plain OS thread, never inside the async runtime -> blocking is fine.
        self.session.blocking_read().clone()
    }

    /// The active stream died. Debounced retry: a dying device spams errors,
    /// but the one-shot error of a freshly rebuilt stream must not be lost —
    /// it is deferred to a tick instead of dropped.
    fn handle_output_failed(&mut self) {
        // The error of a stream that has been replaced since (a device
        // switch, an earlier rebuild) is no reason to leave the one playing,
        // and a stream closed for silence failed nobody: Play opens a new one.
        if !output_failure_counts(
            self.output.is_open(),
            self.output_dead.load(std::sync::atomic::Ordering::Relaxed),
        ) {
            self.output_failed_pending = false;
            return;
        }
        if self.last_output_rebuild.elapsed() <= Duration::from_secs(2) {
            self.output_failed_pending = true;
            return;
        }
        self.output_failed_pending = false;
        let requested = self.playback.output_device.clone();
        // How long the failed stream ran: a switch or a "switch back" that
        // did not open left it playing, and does not make it any younger.
        if reopen_configured_first(
            requested.is_some(),
            crate::audio_devices::fallback_device().is_some(),
            self.stream_opened_at.elapsed(),
        ) {
            match self.rebuild_output(requested) {
                Ok(()) => {
                    tracing::warn!("audio output failed; reopened the configured device");
                    return;
                }
                Err(e) => tracing::warn!("the configured output did not reopen: {e}"),
            }
        }
        tracing::warn!("audio output failed; falling back to the default device");
        // The device that died, named before the attempt can change it.
        let failed = self.opened_output.as_ref().map(|o| o.device.name.clone());
        match self.rebuild_output(None) {
            Ok(()) => {
                if let Some(requested_device) = self.playback.output_device.clone() {
                    // Keep the persisted preference: Settings can show the
                    // missing device, while the runtime stays on system
                    // default.
                    self.mark_fallback(requested_device);
                }
            }
            Err(e) => {
                tracing::error!("cannot open audio output: {e}");
                self.output_alive = false;
                self.report_error(PlayerErrorEvent::no_fallback_output(failed, &e));
                // Close the Jellyfin play session like any other stop.
                if self.status != PlaybackStatus::Idle {
                    self.report_stopped_playing();
                }
                self.stop_internal();
            }
        }
    }

    /// The configured device (`requested`, its stored id) does not play:
    /// the system default stands in for it until Settings switches back.
    fn mark_fallback(&self, requested: String) {
        crate::audio_devices::mark_fallback(requested.clone());
        let _ = self.app.emit(
            "audio:output-fallback",
            crate::audio_devices::AudioOutputFallbackEvent {
                requested_device: self
                    .playback
                    .output_device_name
                    .clone()
                    .unwrap_or(requested),
            },
        );
    }

    /// Tell the error banner (`player:error`); the UI words it.
    fn report_error(&self, event: PlayerErrorEvent) {
        let _ = self.app.emit("player:error", event);
    }

    /// Swap the audio output and pick playback back up. Returns false (and
    /// keeps the previous output untouched) when the new device cannot open.
    fn try_rebuild_output(&mut self, device: Option<String>) -> bool {
        match self.rebuild_output(device.clone()) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("cannot open audio output: {e}");
                // Only the configured device is asked for by id here (a pick
                // in Settings, "Switch back"); None is the system default.
                let name = device.map(|id| {
                    self.playback
                        .output_device_name
                        .clone()
                        .filter(|_| self.playback.output_device.as_ref() == Some(&id))
                        .unwrap_or(id)
                });
                self.report_error(PlayerErrorEvent::output_failed(name, &e));
                false
            }
        }
    }

    /// [`Self::try_rebuild_output`] without reporting a failure, for a caller
    /// that still has another way out.
    fn rebuild_output(&mut self, device: Option<String>) -> AppResult<()> {
        // A gapless boundary crossed since the last tick is settled first
        // (the tick follows the default device before it looks): the
        // position read below must belong to the track `index` names, not to
        // the one appended after it.
        self.maybe_transition();
        self.last_output_rebuild = Instant::now();
        // The configured device is also found by its name, should Windows
        // list it under a new id.
        let name = self
            .playback
            .output_device_name
            .clone()
            .filter(|_| device.is_some() && device == self.playback.output_device);
        let (output, dead, opened) =
            open_output_named(&device, name.as_deref(), &self.tempo, self.tx.clone())?;
        crate::audio_devices::clear_fallback();
        let position_ms = self.position_ms();
        // The playing track's decoder thread and download go over to the new
        // output, its source there starting at the position. Before the old
        // output goes: that takes the old source along, and a decoder thread
        // whose source is gone ends. The appended next track and a crossfade
        // tail go with the old output. A paused track whose player went with
        // an output closed for silence has no source to take along. Its ring
        // comes at the new output's rate.
        let carried = match self.status {
            PlaybackStatus::Playing | PlaybackStatus::Paused if self.sink.is_some() => {
                self.reposition(position_ms, output.rate())
            }
            _ => None,
        };
        self.pending_download = None;
        self.fading_out = None;
        self.output = output;
        self.output_dead = dead;
        self.output_alive = true;
        if let (Some(_), Some(entry)) = (&device, &opened) {
            if adopt_output_device(&mut self.playback, &entry.device) {
                self.persist_playback();
            }
        }
        self.output_opened(opened);
        match self.status {
            PlaybackStatus::Idle => {
                self.sink = None;
                self.current_fade = None;
                self.pending_fade = None;
                self.current_download = None;
                self.pending_download = None;
                self.fading_out = None;
            }
            // An open in flight is tied to no output: its player joins
            // whichever one is there when it lands (source_ready), moved to
            // that output's rate (`refit`). Restarting it would lose a seek
            // target, a pause pressed while it loads and the play session a
            // reopen continues.
            PlaybackStatus::Loading => {}
            // A paused track whose player went with the output when that
            // closed for silence has nothing on the old one: it comes back
            // when it plays (resume_closed), on whatever output is there
            // then — from its parked source, which no output holds.
            PlaybackStatus::Paused if self.sink.is_none() => {}
            PlaybackStatus::Playing | PlaybackStatus::Paused => {
                self.reopen_at(position_ms, carried, position_ms);
            }
        }
        Ok(())
    }

    /// "System default" follows the system: when the default device changes
    /// (headphones that connect, a pick in the volume flyout), the output
    /// moves to the new one. A fallback plays on the system default as well,
    /// so it follows too. Polled: cpal reports no such change, and asking
    /// for the default is cheap.
    fn follow_default_output(&mut self) {
        if self.last_default_poll.elapsed() < DEFAULT_OUTPUT_POLL {
            return;
        }
        self.last_default_poll = Instant::now();
        let fallback = crate::audio_devices::fallback_device();
        if !watches_default(
            self.playback.output_device.is_none() || fallback.is_some(),
            self.output.is_open(),
            self.output_alive,
            self.output_failed_pending,
            self.last_output_rebuild.elapsed(),
        ) {
            return;
        }
        let Some(default) = default_output_id() else {
            return;
        };
        let active = self.opened_output.as_ref().map(|o| o.device.id.as_str());
        if active == Some(default.as_str()) {
            // On the default again: a later change is worth a try anew.
            self.default_output_failed = None;
        }
        let standing_in_for = fallback
            .as_ref()
            .and(self.playback.output_device.as_deref());
        if !should_follow_default(
            &default,
            active,
            self.default_output_failed.as_ref(),
            standing_in_for,
        ) {
            return;
        }
        tracing::info!("the system default output changed; following it");
        // Quietly: nobody asked for this switch, and the music goes on
        // where it plays if the new default does not open.
        match self.rebuild_output(None) {
            Ok(()) => {
                self.default_output_failed = None;
                if let Some(requested) = fallback {
                    // Still standing in for the configured device.
                    crate::audio_devices::mark_fallback(requested);
                }
            }
            Err(e) => {
                let failed = FailedDefault::after(self.default_output_failed.take(), default);
                tracing::warn!(
                    "the new default output did not open (attempt {} of {DEFAULT_OUTPUT_ATTEMPTS}): {e}",
                    failed.attempts
                );
                self.default_output_failed = Some(failed);
            }
        }
    }

    /// After every command and tick: close the output once nothing has been
    /// audible, or about to be, for `IDLE_OUTPUT_CLOSE`.
    fn watch_idle_output(&mut self) {
        let silent = output_silent(
            self.status,
            self.fading_out.is_some() || !self.draining.is_empty(),
            self.output_failed_pending,
        );
        if self.idle.due(self.output.is_open(), silent, Instant::now()) {
            self.close_idle_output();
        }
    }

    /// Close the output nobody has heard for a while, so that its stream
    /// stops holding Windows awake. A paused track lets go of its player as
    /// well — that player plays into this stream — and keeps its place, its
    /// play session and what was heard of it: Play opens the output again
    /// and brings the track back there (`resume_closed`). One that is all on
    /// disk also keeps its decoder and download (`source::park`), so that it
    /// resumes without the server, as a paused player would; anything else
    /// is reloaded then. The device stays the active one, for Settings and
    /// the sound profiles: it is what plays when the music goes on
    /// (`reopen_output`).
    fn close_idle_output(&mut self) {
        let position_ms = self.position_ms();
        // Before the player goes: the decoder thread moves to the parked
        // ring while its source is still there. Parked already (closed
        // once more after a device was picked), it stays as it is.
        if self.parked.is_none() {
            self.parked = self.park_current(position_ms);
        }
        // Nothing may join the closed stream: what is on its way is a
        // prefetch at most, which a reload throws away anyway (restart_at).
        self.generation = self.generation.wrapping_add(1);
        self.release_sink();
        self.draining.clear();
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.adoptable_prefetch = None;
        self.appended = None;
        self.pending_fade = None;
        self.current_fade = None;
        self.pending_tap = None;
        self.current_tap = None;
        self.pending_download = None;
        self.current_download = None;
        self.pending_play_session = None;
        self.sleep_fade_item = None;
        self.position_offset_ms = position_ms;
        self.output.close();
        // Whatever the closed stream still reports is none of ours
        // (handle_output_failed).
        self.output_dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.output_failed_pending = false;
        tracing::info!(
            "audio output closed after {} min of silence",
            IDLE_OUTPUT_CLOSE.as_secs() / 60
        );
        if self.status == PlaybackStatus::Paused {
            if self.parked.is_some() {
                tracing::info!("the paused track keeps its download: Play resumes it from there");
            } else {
                tracing::info!("the paused track is reloaded on Play");
            }
        }
        // The buffered range went with the download, unless it is parked.
        self.emit();
    }

    /// The paused track's source, parked at `position_ms` for the output to
    /// close under it (see `source::park`).
    fn park_current(&self, position_ms: u64) -> Option<ParkedTrack> {
        if self.status != PlaybackStatus::Paused || self.sink.is_none() {
            return None;
        }
        let track = self.queue.get(self.index)?;
        let source = source::park(
            self.current_download.as_ref()?,
            position_ms,
            self.output.rate(),
        )?;
        Some(ParkedTrack {
            entry_id: track.entry_id.clone(),
            item_id: track.item_id.clone(),
            source,
        })
    }

    /// Open the output again for playback, after it closed for silence
    /// (`close_idle_output`) or died with nothing to fall back on
    /// (`handle_output_failed`). As at startup: the configured device, also
    /// under a new id, else the system default — which then stands in for
    /// it, with the toast. A fallback in place stays on the default: the
    /// device coming back does not take playback back by itself (Settings
    /// offers "Switch back"). "System default" opens what the default is
    /// now, and the profile of the device that opens applies.
    fn reopen_output(&mut self) -> AppResult<()> {
        let mut step = ReopenStep::first(
            self.playback.output_device.as_deref(),
            self.playback.output_device_name.as_deref(),
            crate::audio_devices::fallback_device().is_some(),
        );
        let (output, dead, opened) = loop {
            let (device, name) = step.device();
            match open_output_named(&device, name, &self.tempo, self.tx.clone()) {
                Ok(opened) => break opened,
                Err(e) => match step.after_failure() {
                    Some(next) => {
                        tracing::warn!("configured output device failed ({e}); using default");
                        step = next;
                    }
                    None => return Err(e),
                },
            }
        };
        if let Some(requested) = step.stands_in_for() {
            self.mark_fallback(requested.to_string());
        }
        self.output = output;
        self.output_dead = dead;
        self.output_alive = true;
        self.output_failed_pending = false;
        self.last_output_rebuild = Instant::now();
        if let (true, Some(entry)) = (step.adopts(), &opened) {
            if adopt_output_device(&mut self.playback, &entry.device) {
                self.persist_playback();
            }
        }
        self.output_opened(opened);
        Ok(())
    }

    /// Whether `settings` ask for the configured device that plays now,
    /// maybe under an id or name it had before (see [`names_output`]). Never
    /// during a fallback: the configured device is not what plays then.
    fn means_playing(&self, settings: &PlaybackSettings) -> bool {
        let (Some(requested), Some(_), Some(playing)) = (
            settings.output_device.as_deref(),
            &self.playback.output_device,
            &self.opened_output,
        ) else {
            return false;
        };
        crate::audio_devices::fallback_device().is_none()
            && list_outputs().is_ok_and(|outputs| {
                names_output(
                    requested,
                    settings.output_device_name.as_deref(),
                    &playing.device,
                    outputs.iter().map(|(_, entry)| entry),
                )
            })
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
        self.listen
            .sample_at(Instant::now(), position_ms, self.tempo.speed());
        let listen = self.listen.take();
        if self
            .recent_tracks
            .back()
            .map(|recent| recent.item_id.as_str())
            != Some(track.item_id.as_str())
        {
            self.recent_tracks.push_back(RecentTrack::of(track));
        }
        let keep = self.extras.auto_dj_recent_tracks.clamp(1, 500) as usize;
        while self.recent_tracks.len() > keep {
            self.recent_tracks.pop_front();
        }
        if scrobble::should_scrobble(listen.listened_ms, track.duration_ms) {
            if let Some(token) = self.extras.listenbrainz_token.clone() {
                scrobble::submit_listen(token, track.clone(), listen.started_at);
            }
            if self.record_history(track, &listen) {
                let _ = self.app.emit("history:changed", ());
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
        self.save_volume();
        // While the track is still there to say where it stands.
        self.persist_position();
        let report = self.take_final_report(false);
        // Deliberately NOT stop_internal(): that ends in `emit()`, which
        // updates the tray, and the tray menu has to be touched on the main
        // thread. The main thread is at this very moment blocked waiting for
        // our acknowledgement (`quit()` -> `PlayerHandle::shutdown`), so going
        // through it would stall the whole quit until the 2.5 s timeout.
        // Nobody is watching the UI during shutdown anyway; just release the
        // audio device — before the report, which waits for the network.
        self.generation = self.generation.wrapping_add(1);
        self.prefetched = None;
        self.prefetch_inflight = None;
        self.appended = None;
        self.current_fade = None;
        self.fading_out = None;
        self.draining.clear();
        self.sink = None;
        // The downloads stop, and their decoder threads with them.
        self.current_download = None;
        self.pending_download = None;
        self.parked = None;
        self.status = PlaybackStatus::Idle;
        if let Some(report) = report {
            self.send_final_report(report);
        }
    }

    /// Settle the current track for its final report while it still plays —
    /// for the moments where the runtime or the session is about to disappear
    /// (quit, sign-out): where it is, what of it was heard, the play session
    /// it went out under. The audio can stop right after; the report is sent
    /// by [`Self::send_final_report`].
    ///
    /// `resolve_boundary` settles a crossed track boundary first. Quit passes
    /// false: resolving it emits state, which updates the tray on the main
    /// thread — and that thread is blocked in `quit()` waiting for us.
    fn take_final_report(&mut self, resolve_boundary: bool) -> Option<FinalReport> {
        if resolve_boundary {
            self.maybe_transition();
        }
        let position_ms = self.position_ms();
        let track = self.queue.get(self.index).cloned()?;
        self.listen
            .sample_at(Instant::now(), position_ms, self.tempo.speed());
        Some(FinalReport {
            listen: self.listen.take(),
            session_id: self.play_session_id.take(),
            track,
            position_ms,
        })
    }

    /// Like [`Self::report_stopped`], but waits for the requests instead of
    /// spawning them. The play history goes first: it is a local write, and
    /// the process may be gone before a slow server answers. Then Jellyfin:
    /// the caller waits only briefly, and a slow ListenBrainz must not use up
    /// the time the session report needs.
    fn send_final_report(&self, report: FinalReport) {
        let FinalReport {
            track,
            position_ms,
            listen,
            session_id,
        } = report;
        let counts = scrobble::should_scrobble(listen.listened_ms, track.duration_ms);
        if counts {
            // No event: nobody is looking at the timeline while the session
            // goes away.
            self.record_history(&track, &listen);
        }
        if let (Some(session_id), Some(client)) = (session_id, self.session_client()) {
            let item_id = track.item_id.clone();
            tauri::async_runtime::block_on(async move {
                let report = client.report_stopped(&item_id, &session_id, position_ms);
                match tokio::time::timeout(FINAL_REPORT_TIMEOUT, report).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::debug!("final report stopped failed: {e}"),
                    Err(_) => tracing::debug!("final report stopped got no answer in time"),
                }
            });
        }
        if counts {
            if let Some(token) = self.extras.listenbrainz_token.clone() {
                scrobble::submit_listen_blocking(
                    token,
                    track,
                    listen.started_at,
                    FINAL_REPORT_TIMEOUT,
                );
            }
        }
    }

    /// One row in the play history for a listen that counts, dated when the
    /// listening ended — a track paused overnight and reported stopped the
    /// next morning was heard the night before. A failed write costs that row
    /// and nothing else; playback does not wait on it beyond one local insert.
    fn record_history(&self, track: &QueueTrack, listen: &scrobble::Listen) -> bool {
        if !self.extras.record_history {
            return false;
        }
        let Some(owner) = listener(self.session_client().as_deref(), &self.store) else {
            return false;
        };
        let played_at = listen
            .ended_at
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .map(|since| since.as_millis() as i64);
        match crate::history::record(&self.store, &owner, track, listen.listened_ms, played_at) {
            Ok(written) => written,
            Err(e) => {
                tracing::warn!("play history not written: {e}");
                false
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
        let mut cover = self.cover_file_url(track);
        if cover.is_none() {
            self.fetch_smtc_artwork(track);
            // souvlaki leaves the thumbnail alone without a cover, so the
            // overlay would show the previous track's: a neutral one stands
            // in until (or unless) the real one lands.
            cover = self.placeholder_cover_url();
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

    /// The queue is gone (cleared, or the account signed out): nothing of the
    /// last track may linger in the media overlay. souvlaki only overwrites
    /// what it is given, so it gets empty fields and the neutral cover.
    fn clear_media_metadata(&mut self) {
        let cover = self.placeholder_cover_url();
        let Some(controls) = self.media.as_mut() else {
            return;
        };
        let _ = controls.set_metadata(souvlaki::MediaMetadata {
            title: Some(""),
            artist: Some(""),
            album: Some(""),
            duration: None,
            cover_url: cover.as_deref(),
        });
    }

    /// The media overlay's stand-in cover — the app icon — as souvlaki wants
    /// it (see `cover_file_url`). Written into the cover directory on first
    /// use, and again whenever the cache has cleared or evicted it.
    fn placeholder_cover_url(&self) -> Option<String> {
        const ICON: &[u8] = include_bytes!("../../icons/128x128.png");
        // No overlay, no picture to put in it.
        self.media.as_ref()?;
        // Not a name the jfimg cache uses (`cache::cover_file_name`).
        let path = self.cover_dir.join("smtc-placeholder.png");
        if !path.exists() {
            let _ = std::fs::create_dir_all(&self.cover_dir);
            if let Err(e) = crate::cache::write_file_atomic(&path, ICON) {
                tracing::debug!("smtc placeholder cover not written: {e}");
                return None;
            }
        }
        Some(format!("file://{}", path.display()))
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
        let position = Duration::from_millis(self.position_ms());
        let playback = media_playback(self.status, self.pending_pause, position);
        let Some(controls) = self.media.as_mut() else {
            return;
        };
        let _ = controls.set_playback(playback);
    }

    /// Queue is on its last track and Auto-DJ is on: append a server mix
    /// seeded by it (once per track; the latch is released on failure so a
    /// transient error is retried on a later tick — after a pause, and only a
    /// few times, see [`AutoDjRetry`]).
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
        if self
            .auto_dj_retry
            .as_ref()
            .is_some_and(|retry| retry.holds(&current.item_id, Instant::now()))
        {
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
        existing.extend(self.recent_tracks.iter().map(|t| t.item_id.clone()));
        // The same, by recording: another file of a track that is queued or
        // was just heard is that song again — and to the sonic continuation
        // the closest match of all.
        let mut heard_recordings = crate::commands_sonic::Recordings::default();
        for track in &self.queue {
            heard_recordings.insert(&track.artist, &track.name, track.duration_ms);
        }
        for track in &self.recent_tracks {
            heard_recordings.insert(&track.artist, &track.name, track.duration_ms);
        }
        // What was just heard, oldest first, ending with the current track:
        // the sonic continuation follows where the set has gone.
        let mut heard: Vec<String> = self
            .recent_tracks
            .iter()
            .map(|t| t.item_id.clone())
            .collect();
        heard.retain(|id| id != &request_track);
        heard.push(request_track.clone());
        let sonic = (reason.kind == AutoDjSeedMode::Sonic)
            .then(|| (self.store.clone(), crate::identity::Server::of(&client)));
        let tx = self.tx.clone();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            // A revoked session (the device removed while the window sits in
            // the tray): renewed the way every caller renews it, so the mix
            // still arrives before the queue runs out.
            let renew = |stale: Arc<JellyfinClient>, unauthorized| {
                let app = app.clone();
                async move {
                    crate::commands::renew_session(&app, &stale, Instant::now(), unauthorized).await
                }
            };
            let mut client = client;
            let mut reason = reason;
            let mut tracks = Vec::new();
            if let Some((store, server)) = sonic {
                let exclude = existing.clone();
                let ids = tokio::task::spawn_blocking(move || {
                    crate::sonic::index(&store, &server).continuation(&heard, max_tracks, &exclude)
                })
                .await
                .unwrap_or_default();
                if ids.len() >= 3 {
                    let found = fetch_renewing(
                        &mut client,
                        |client| {
                            let ids = ids.clone();
                            async move { client.tracks_by_ids(&ids).await }
                        },
                        &renew,
                    )
                    .await;
                    match found {
                        Ok(mut found) => {
                            let position =
                                |id: &str| ids.iter().position(|x| x == id).unwrap_or(usize::MAX);
                            found.sort_by_key(|t| position(&t.id));
                            tracks = fresh_mix(found, &existing, &heard_recordings, max_tracks);
                        }
                        Err(e) => tracing::warn!("auto-dj sonic tracks failed: {e}"),
                    }
                }
                // A handful is not a set; let the server mix it instead. Told
                // after the fetch: ids the server no longer has, or this
                // account cannot see, are no set either.
                if tracks.len() < 3 {
                    tracks.clear();
                    reason = AutoDjReason {
                        kind: AutoDjSeedMode::Track,
                        label: reason.label,
                    };
                }
            }
            if tracks.is_empty() {
                let mix = fetch_renewing(
                    &mut client,
                    |client| {
                        let seed = seed.clone();
                        async move {
                            client
                                .instant_mix(&seed, (max_tracks as u32).saturating_mul(3))
                                .await
                        }
                    },
                    &renew,
                )
                .await;
                match mix {
                    Ok(mix) => tracks = fresh_mix(mix, &existing, &heard_recordings, max_tracks),
                    Err(e) => tracing::warn!("auto-dj mix failed: {e}"),
                }
            }
            let mut queue = crate::commands::to_queue_tracks(&client, tracks).unwrap_or_else(|e| {
                tracing::warn!("auto-dj queue build failed: {e}");
                Vec::new()
            });
            for track in &mut queue {
                track.auto_dj_reason = Some(reason.clone());
            }
            let _ = tx.send(PlayerCommand::AutoDjResult {
                request_track,
                reason,
                tracks: queue,
            });
        });
    }

    fn release_auto_dj_latch(&mut self, request_track: &str) {
        if self.last_auto_dj_seed.as_deref() == Some(request_track) {
            self.last_auto_dj_seed = None;
        }
    }

    /// The mix for `request_track` brought nothing (a failure, or nothing
    /// that is not queued or heard already): the track may ask again, but
    /// not on the next tick.
    fn auto_dj_came_back_empty(&mut self, request_track: &str) {
        self.release_auto_dj_latch(request_track);
        let attempts = match &self.auto_dj_retry {
            Some(retry) if retry.seed == request_track => retry.attempts + 1,
            _ => 1,
        };
        self.auto_dj_retry = Some(AutoDjRetry {
            seed: request_track.to_string(),
            attempts,
            not_before: Instant::now() + AUTO_DJ_RETRY_AFTER,
        });
    }

    /// A fresh playthrough: Auto-DJ may fire again, and a seed that came back
    /// empty before gets its attempts back.
    fn rearm_auto_dj(&mut self) {
        self.last_auto_dj_seed = None;
        self.auto_dj_retry = None;
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
        // Switched off while the mix was on its way: it was asked for by a
        // setting that is no longer on, and a queue that ran out must not
        // start again over it.
        if !self.extras.auto_dj_enabled {
            self.release_auto_dj_latch(&request_track);
            return;
        }
        if tracks.is_empty() {
            self.auto_dj_came_back_empty(&request_track);
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
            // The queue moved on while the mix was fetched; the seed may come
            // up last again later and must be able to ask once more.
            self.release_auto_dj_latch(&request_track);
            return;
        }
        let existing: std::collections::HashSet<&str> =
            self.queue.iter().map(|t| t.item_id.as_str()).collect();
        let fresh: Vec<QueueTrack> = tracks
            .into_iter()
            .filter(|t| !existing.contains(t.item_id.as_str()))
            .collect();
        if fresh.is_empty() {
            self.auto_dj_came_back_empty(&request_track);
            return;
        }
        self.auto_dj_retry = None;
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
            let end = self.queue.len();
            self.order_mut().extend(start..end);
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
        let Some(sink) = &self.sink else {
            return self.position_offset_ms;
        };
        // rodio counts the silence a source plays while its decoder has
        // nothing for it (a stalled download, a seek on its way) as played;
        // it is no media time. Past a gapless boundary not settled yet,
        // rodio counts for the appended source already.
        let playing = if self.appended.is_some() && sink.len() <= 1 {
            &self.pending_download
        } else {
            &self.current_download
        };
        let silence = playing
            .as_ref()
            .map_or(Duration::ZERO, |download| download.silence());
        self.position_offset_ms + sink.get_pos().saturating_sub(silence).as_millis() as u64
    }

    fn seek_ms(&mut self, position_ms: u64) {
        // A crossfade tail playing across a seek would be confusing: fade it
        // out quickly instead of hard-cutting (pop).
        if let Some((_, fade, drop_at, _)) = &mut self.fading_out {
            fade.ramp_to(0.0, Duration::from_millis(50));
            *drop_at = Instant::now() + self.tempo.wall(Duration::from_millis(150));
        }
        // What played up to here was listened to; the jump is not.
        self.listen
            .sample_at(Instant::now(), self.position_ms(), self.tempo.speed());
        let Some(sink) = &self.sink else {
            // Nothing to move yet. An open in flight starts over at the new
            // target (a pause pressed meanwhile still holds); with nothing
            // loaded, the target is where the track starts once it plays
            // (resume_or_start) — and what the seek bar shows until then.
            match self.status {
                PlaybackStatus::Loading => {
                    let paused = self.pending_pause;
                    self.restart_at(position_ms);
                    // restart_at reported the load as one that plays; a
                    // pause pressed meanwhile still holds, and says so.
                    self.set_pending_pause(paused);
                }
                PlaybackStatus::Idle if self.index < self.queue.len() => {
                    self.position_offset_ms = position_ms;
                    self.emit();
                }
                // Paused, its player gone with the output closed for
                // silence: the target is where Play brings it back
                // (resume_closed). The jump there is not listening.
                PlaybackStatus::Paused if self.index < self.queue.len() => {
                    self.position_offset_ms = position_ms;
                    self.listen.rebase(Instant::now(), position_ms);
                    self.report_progress(true);
                    self.update_media_playback();
                    self.emit();
                }
                _ => {}
            }
            return;
        };
        // The source's decoder thread carries out the seek: rodio hands it to
        // the source on the audio callback, which passes it on and plays
        // silence until the target's audio is decoded. Wherever the download
        // stands — the decoder steers it to the target and reads what is on
        // disk. A live transcode cannot seek and is reopened at the target
        // (`restart_at`), as is a source on a dead output, whose callback
        // never runs.
        let seekable = self
            .current_download
            .as_ref()
            .is_some_and(|download| download.seekable());
        let output_dead = self.output_dead.load(std::sync::atomic::Ordering::Relaxed);
        // During the silence in front of a beat transition, a native seek
        // would position the silence, not the track.
        let in_lead = self
            .lead_until
            .is_some_and(|until| Instant::now() < until + Duration::from_millis(50));
        if seekable && !output_dead && !in_lead {
            match sink.try_seek(Duration::from_millis(position_ms)) {
                Ok(()) => {
                    if let Some(download) = &self.current_download {
                        download.wake_decoder();
                    }
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
    /// new output device). A direct-play source keeps its decoder thread and
    /// download, positioned anew (`source::reposition`); a transcode starts
    /// there via `startTimeTicks`, and anything else that cannot be
    /// repositioned is opened anew and positioned on the open thread. The
    /// offset tracks media time either way.
    fn restart_at(&mut self, position_ms: u64) {
        // Before the decoder moves on: what rodio counted as played then.
        let played_to = self.position_ms();
        let carried = self.reposition(position_ms, self.output.rate());
        self.reopen_at(position_ms, carried, played_to);
    }

    /// The current source once more, at `position_ms` and at `rate` (an
    /// output's), from its own decoder thread and download — when it can be
    /// (see `source::reposition`).
    fn reposition(&self, position_ms: u64, rate: rodio::SampleRate) -> Option<OpenedSource> {
        let track = self.queue.get(self.index)?;
        let download = self.current_download.as_ref()?;
        source::reposition(
            download,
            position_ms,
            rate,
            self.dsp.clone(),
            self.normalization_gain_for(track),
            self.tap.clone(),
        )
    }

    /// `opened` at the output's rate. A source opened for an output that was
    /// replaced by one at another rate while it loaded is moved onto a new
    /// ring of the same decoder, resampled for this one, where it starts
    /// (`start_ms`). One that cannot be repositioned (a live transcode) stays
    /// as it is: the player's way into the mixer converts its rate then,
    /// linearly, as rodio did for every track before.
    fn refit(&self, track: &QueueTrack, opened: OpenedSource) -> OpenedSource {
        use rodio::Source as _;
        let rate = self.output.rate();
        if opened.source.sample_rate() == rate {
            return opened;
        }
        tracing::debug!(
            "a source opened at {} Hz lands on an output at {rate} Hz",
            opened.source.sample_rate()
        );
        source::reposition(
            &opened.download,
            opened.start_ms,
            rate,
            self.dsp.clone(),
            self.normalization_gain_for(track),
            self.tap.clone(),
        )
        .unwrap_or(opened)
    }

    /// [`Self::restart_at`], with the repositioned source when there is one;
    /// `played_to` is where the current source stood before that.
    fn reopen_at(&mut self, position_ms: u64, carried: Option<OpenedSource>, played_to: u64) {
        let Some(track) = self.queue.get(self.index).cloned() else {
            return;
        };
        let resume_paused = self.status == PlaybackStatus::Paused;
        // Listening continues from the target; the jump there is not counted.
        self.listen
            .sample_at(Instant::now(), played_to, self.tempo.speed());
        self.listen.rebase(Instant::now(), position_ms);
        self.generation = self.generation.wrapping_add(1);
        self.release_sink();
        self.prefetched = None;
        self.prefetch_inflight = None;
        // A prefetch from the start would ignore the position.
        self.adoptable_prefetch = None;
        self.appended = None;
        self.pending_fade = None;
        self.current_fade = None;
        self.pending_tap = None;
        self.current_tap = None;
        self.pending_download = None;
        self.current_download = None;
        // The fresh source takes the sleep ramp when it lands (arm_fresh).
        self.sleep_fade_item = None;
        // Arrive paused when it was paused; Play/Pause pressed while loading
        // adjusts this like for any other open.
        self.pending_pause = resume_paused;
        // Show the seek target while loading (get_pos is gone with the sink).
        self.position_offset_ms = position_ms;
        if let Some(opened) = carried {
            // Ready at once: the same attempt, the same play session.
            let play_session = self
                .play_session_id
                .clone()
                .unwrap_or_else(new_play_session);
            self.source_ready(
                self.generation,
                self.prefetch_generation,
                OpenPurpose::SeekRestart { position_ms },
                track,
                play_session,
                Ok(Box::new(opened)),
            );
            return;
        }
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

    /// The shuffle order for changing it, like [`Self::queue_mut`]: the order
    /// is persisted and shown with the track list, so a change to it alone
    /// (a drag under shuffle, shuffle switched on) is a change to the list.
    fn order_mut(&mut self) -> &mut Vec<usize> {
        self.queue_revision = self.queue_revision.wrapping_add(1);
        &mut self.order
    }

    /// Persist the queue, refresh the shared mirror, notify the UI.
    ///
    /// The whole snapshot goes out only when the track list or its order
    /// changed. A track change moves the index alone, and then the UI gets
    /// `player:queue-index` and keeps the list it has: nothing on the way
    /// from one track to the next clones or sends the whole queue.
    fn sync_queue(&mut self) {
        let order = if self.shuffle { &self.order[..] } else { &[] };
        let played = played_count(self.queue.len(), self.index, order);
        let can_undo = self.queue_undo.is_some();
        if self.synced_revision == Some(self.queue_revision) {
            {
                let mut shared = self.shared_queue.lock().unwrap();
                shared.index = self.index;
                shared.can_undo = can_undo;
                shared.played_count = played;
            }
            self.persist_queue();
            let position = QueuePosition {
                revision: self.queue_revision,
                index: self.index,
                can_undo,
                played_count: played,
                duplicate_count: self.duplicate_count,
            };
            let _ = self.app.emit("player:queue-index", position);
            return;
        }
        let order = order.to_vec();
        // The entries a stop was armed over can be removed, or the whole
        // queue replaced. It moves to the last of them still queued; with
        // none left it would never fire and the marker would sit in the list
        // forever, so it goes with them.
        if !self.stop_run.is_empty() {
            match stop_entry(&self.stop_run, &self.queue) {
                Some(at) => {
                    if self.stop_at.as_ref() != Some(at) {
                        self.stop_at = Some(at.clone());
                    }
                }
                None => self.clear_stop_after(),
            }
        }
        self.duplicate_count = duplicate_count(&self.queue);
        let snapshot = QueueSnapshot {
            revision: self.queue_revision,
            tracks: self.queue.clone(),
            index: self.index,
            order,
            can_undo,
            played_count: played,
            duplicate_count: self.duplicate_count,
        };
        self.persist_queue();
        let _ = self.app.emit("player:queue", &snapshot);
        *self.shared_queue.lock().unwrap() = snapshot;
        self.synced_revision = Some(self.queue_revision);
    }

    /// Load a queue read back from the store, without starting playback.
    fn restore_queue(&mut self, restored: RestoredQueue) {
        let RestoredQueue {
            mut tracks,
            index,
            position_ms,
            order,
        } = restored;
        // Lines persisted before entry ids existed get one now, so from here
        // on two copies of a track can be told apart (stop after, remove).
        for track in &mut tracks {
            if track.entry_id.is_empty() {
                track.entry_id = uuid::Uuid::new_v4().to_string();
            }
        }
        self.queue_undo = None;
        *self.queue_mut() = tracks;
        self.index = index.min(self.queue.len().saturating_sub(1));
        self.status = PlaybackStatus::Idle;
        // Play starts the track there, as after a seek made while nothing
        // played. An index the list no longer reaches starts its track anew.
        self.position_offset_ms = if self.index == index { position_ms } else { 0 };
        if self.shuffle {
            // The order it was playing in: what was heard stays heard, and
            // what was put next stays next.
            match restored_order(order, self.shuffle_mode, self.queue.len()) {
                Some(order) => *self.order_mut() = order,
                None => self.rebuild_order(),
            }
        }
        self.sync_queue();
        self.emit();
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
                    position_ms: None,
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
                    order: self.shuffle.then(|| PersistedOrder {
                        mode: self.shuffle_mode,
                        order: self.order.clone(),
                    }),
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
        // Bytes map to time when the stream is the whole file: direct play,
        // reopened at a position or not (Jellyfin ignores startTimeTicks there
        // and sends the file from its start). A live transcode — the only
        // stream that starts mid-track — has no length, so no fraction.
        // A track parked while the output is closed is all on disk still.
        let download = self
            .current_download
            .as_ref()
            .or_else(|| self.parked.as_ref().map(|p| p.source.download()));
        let buffered_ms = buffered_range_ms(download.and_then(|d| d.fraction()), duration_ms);
        let state = PlayerState {
            status: self.status,
            pending_pause: arrives_paused(self.status, self.pending_pause),
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
            stop_after: self.stop_after,
            stop_after_entry: self
                .stop_at
                .as_ref()
                .map(|at| at.entry_id.clone())
                .filter(|id| !id.is_empty()),
            stop_after_item: self.stop_at.as_ref().map(|at| at.item_id.clone()),
            buffered_ms,
            speed: self.speed.speed,
            preserve_pitch: self.speed.preserve_pitch,
        };
        *self.shared_state.lock().unwrap() = state.clone();
        let _ = self.app.emit("player:state", &state);
        crate::tray::update_player(&self.app, &state);
    }
}

/// An output device as the player knows it.
#[derive(Debug, Clone, PartialEq)]
struct OutputEntry {
    device: OutputDevice,
    /// cpal's bare endpoint name ("Speakers"): what builds before 0.6.1
    /// stored for the device, as the output setting and as a profile key.
    legacy_name: Option<String>,
}

/// What [`open_output`] returns: the output, the flag its error callback
/// sets, and the device it opened.
type OpenedOutput = (
    Output,
    Arc<std::sync::atomic::AtomicBool>,
    Option<OutputEntry>,
);

/// Open an output stream on the stored device (its id, or a name from before
/// 0.6.1; None = system default) with an error callback that reports device
/// death back to the player loop. Also returns the device it opened.
fn open_output(
    device: &Option<String>,
    tempo: &tempo::TempoHandle,
    tx: Sender<PlayerCommand>,
) -> AppResult<OpenedOutput> {
    open_output_named(device, None, tempo, tx)
}

/// [`open_output`] for the configured device, which `name` (as last seen)
/// finds again when Windows lists it under a new id.
fn open_output_named(
    device: &Option<String>,
    name: Option<&str>,
    tempo: &tempo::TempoHandle,
    tx: Sender<PlayerCommand>,
) -> AppResult<OpenedOutput> {
    use rodio::cpal::traits::HostTrait;

    // The device is resolved once and opened as such, so the id the
    // per-device profiles key on is the device that actually plays.
    let (builder, opened) = match device {
        Some(stored) => {
            let mut outputs = list_outputs()?;
            let index = resolve_output(stored, name, outputs.iter().map(|(_, entry)| entry))
                .ok_or_else(|| AppError::Audio(errors::OUTPUT_DEVICE_MISSING.into()))?;
            let (device, entry) = outputs.swap_remove(index);
            let builder = rodio::stream::DeviceSinkBuilder::from_device(device)
                .map_err(|e| AppError::Audio(format!("cannot use output device: {e}")))?;
            (builder, Some(entry))
        }
        None => {
            let device = rodio::cpal::default_host()
                .default_output_device()
                .ok_or_else(|| AppError::Audio(errors::NO_OUTPUT_DEVICE.into()))?;
            let entry = describe_output(&device);
            let builder = rodio::stream::DeviceSinkBuilder::from_device(device)
                .map_err(|e| AppError::Audio(format!("cannot open default output: {e}")))?;
            (builder, entry)
        }
    };
    let dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = dead.clone();
    let mut sink = builder
        .with_error_callback(move |err| {
            tracing::warn!("audio stream error: {err}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(PlayerCommand::OutputFailed);
        })
        .open_stream()
        .map_err(|e| AppError::Audio(format!("cannot open audio output: {e}")))?;
    // Closing it is routine (`Output::close`), not worth a line on stderr.
    sink.log_on_drop(false);
    // The players mix into a mixer of their own, at the device's format,
    // which reaches the device through the tempo stage and the limiter: the
    // last stage keeps whatever the mix adds up to under full scale.
    let (channels, rate) = (sink.config().channel_count(), sink.config().sample_rate());
    let (mixer, mixed) = rodio::mixer::mixer(channels, rate);
    sink.mixer().add(limiter::wrap(tempo::wrap(mixed, tempo)));
    tracing::debug!("audio output at {rate} Hz, {channels} channels");
    Ok((
        Output {
            mix: Mix {
                mixer,
                channels,
                rate,
            },
            device: Some(sink),
        },
        dead,
        opened,
    ))
}

/// An output: the device stream, and the mixer the players connect to.
/// Closed after a while of silence (`Worker::close_idle_output`), it holds no
/// stream, and its mixer reaches nothing. No player joins it then: every way
/// into Loading opens the output again first (`Worker::reopen_output`).
struct Output {
    mix: Mix,
    device: Option<rodio::stream::MixerDeviceSink>,
}

impl Output {
    fn mix(&self) -> &Mix {
        &self.mix
    }

    /// The rate everything is mixed at: what the tracks are resampled to.
    /// A closed output keeps the one it had; whatever plays next opens an
    /// output first, at its own.
    fn rate(&self) -> rodio::SampleRate {
        self.mix.rate
    }

    fn is_open(&self) -> bool {
        self.device.is_some()
    }

    /// Stop the stream and let go of the device. The players on the mixer
    /// go with it.
    fn close(&mut self) {
        self.device = None;
    }
}

/// The mixer the players join, and the format it mixes in.
struct Mix {
    mixer: rodio::mixer::Mixer,
    channels: rodio::ChannelCount,
    rate: rodio::SampleRate,
}

/// Output devices for the settings UI.
pub fn output_devices() -> Vec<OutputDevice> {
    list_outputs()
        .map(|outputs| outputs.into_iter().map(|(_, entry)| entry.device).collect())
        .unwrap_or_default()
}

/// Every output device cpal lists, except one without an id: that could
/// neither be stored nor found again.
fn list_outputs() -> AppResult<Vec<(rodio::cpal::Device, OutputEntry)>> {
    use rodio::cpal::traits::HostTrait;
    let devices = rodio::cpal::default_host()
        .output_devices()
        .map_err(|e| AppError::Audio(format!("cannot list output devices: {e}")))?;
    Ok(devices
        .filter_map(|device| describe_output(&device).map(|entry| (device, entry)))
        .collect())
}

/// cpal's deprecated `name()` is read on purpose: it is what builds before
/// 0.6.1 stored, and what their choices are found by.
#[allow(deprecated)]
fn describe_output(device: &rodio::cpal::Device) -> Option<OutputEntry> {
    use rodio::cpal::traits::DeviceTrait;
    let id = device.id().ok()?.to_string();
    let name = device
        .description()
        .ok()
        .map(|description| display_name(&description))
        .unwrap_or_else(|| id.clone());
    Some(OutputEntry {
        device: OutputDevice { id, name },
        legacy_name: device.name().ok(),
    })
}

/// The name the system's sound settings show. cpal's name is only the
/// endpoint's own line ("Speakers"); on Windows the full name with the
/// adapter ("Speakers (Realtek(R) Audio)") is the first extended line.
fn display_name(description: &rodio::cpal::DeviceDescription) -> String {
    description
        .extended()
        .first()
        .filter(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| description.name().to_string())
}

/// Which listed output a stored choice means: the one with that id; for a
/// choice from before 0.6.1 (cpal's bare endpoint name), the first output of
/// that name — the one those builds opened; else the only output called
/// `name`, the device's full name as last seen. Windows lists a device under
/// a new id now and then (see `OutputDevice::id`), mostly with the name it
/// had; a name two outputs share finds neither.
fn resolve_output<'a>(
    stored: &str,
    name: Option<&str>,
    outputs: impl Iterator<Item = &'a OutputEntry> + Clone,
) -> Option<usize> {
    outputs
        .clone()
        .position(|entry| entry.device.id == stored)
        .or_else(|| {
            outputs
                .clone()
                .position(|entry| entry.legacy_name.as_deref() == Some(stored))
        })
        .or_else(|| {
            let name = name?;
            let mut named = outputs
                .enumerate()
                .filter(|(_, entry)| entry.device.name == name)
                .map(|(index, _)| index);
            match (named.next(), named.next()) {
                (Some(index), None) => Some(index),
                _ => None,
            }
        })
}

/// The configured device opened: store its id (a choice from before 0.6.1
/// held its bare endpoint name) and the name Settings shows while it is
/// away. Returns whether anything changed.
fn adopt_output_device(playback: &mut PlaybackSettings, device: &OutputDevice) -> bool {
    let changed = playback.output_device.as_deref() != Some(device.id.as_str())
        || playback.output_device_name.as_deref() != Some(device.name.as_str());
    if changed {
        playback.output_device = Some(device.id.clone());
        playback.output_device_name = Some(device.name.clone());
    }
    changed
}

/// Whether a stored choice (`requested`, with the name as last seen) means
/// `device` among the listed outputs.
fn names_output<'a>(
    requested: &str,
    name: Option<&str>,
    device: &OutputDevice,
    mut outputs: impl Iterator<Item = &'a OutputEntry> + Clone,
) -> bool {
    resolve_output(requested, name, outputs.clone())
        .and_then(|index| outputs.nth(index))
        .is_some_and(|entry| entry.device.id == device.id)
}

/// Ids of the output devices listed now; None when they cannot be listed.
fn listed_output_ids() -> Option<Vec<String>> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    let devices = rodio::cpal::default_host().output_devices().ok()?;
    Some(
        devices
            .filter_map(|device| device.id().ok().map(|id| id.to_string()))
            .collect(),
    )
}

/// Id of the system's default output device right now.
fn default_output_id() -> Option<String> {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    let device = rodio::cpal::default_host().default_output_device()?;
    device.id().ok().map(|id| id.to_string())
}

/// Whether a failed output gets its configured device back before the
/// fallback to the default. A stream that ran for a while and then died was
/// most often reconfigured under us (a format or spatial-sound change, a
/// driver reset after resume) while its device stayed. One that dies right
/// after opening is on its way out, and a fallback already in place stays
/// on the default.
fn reopen_configured_first(configured: bool, fallback_active: bool, ran_for: Duration) -> bool {
    configured && !fallback_active && ran_for >= OUTPUT_SETTLED
}

/// A default device that did not open when "system default" followed it.
#[derive(Debug, Clone, PartialEq)]
struct FailedDefault {
    id: String,
    /// Failed attempts on it so far.
    attempts: u32,
    at: Instant,
}

impl FailedDefault {
    /// One more failed attempt on `id`, counted on from `before` if that
    /// was the same device.
    fn after(before: Option<FailedDefault>, id: String) -> Self {
        let attempts = before
            .filter(|failed| failed.id == id)
            .map_or(1, |failed| failed.attempts + 1);
        Self {
            id,
            attempts,
            at: Instant::now(),
        }
    }

    /// Whether `default` gets another attempt: a few, a moment apart.
    fn retry(&self, default: &str) -> bool {
        self.id != default
            || (self.attempts < DEFAULT_OUTPUT_ATTEMPTS
                && self.at.elapsed() >= DEFAULT_OUTPUT_RETRY)
    }
}

/// Whether "system default" moves to `default`: it is not what plays, not a
/// default that failed to open a moment ago or too often, and not the
/// configured device a fallback stands in for. That one coming back, even as
/// the new default, does not take playback back by itself; Settings offers
/// to switch back.
fn should_follow_default(
    default: &str,
    active: Option<&str>,
    failed: Option<&FailedDefault>,
    standing_in_for: Option<&str>,
) -> bool {
    active != Some(default)
        && failed.is_none_or(|failed| failed.retry(default))
        && standing_in_for != Some(default)
}

/// Whether the tick looks for a new default device at all: for "system
/// default" or a fallback (`follows`), on an output that is open and healthy,
/// and not right after it was rebuilt. An output closed for silence opens
/// whatever the default is when the music goes on; following the default
/// meanwhile would open a stream nobody hears.
fn watches_default(
    follows: bool,
    open: bool,
    alive: bool,
    failure_pending: bool,
    since_rebuild: Duration,
) -> bool {
    follows && open && alive && !failure_pending && since_rebuild >= DEFAULT_OUTPUT_POLL
}

/// Whether nothing is audible or about to be: paused or stopped, with no
/// player still ramping out (`fading`: a crossfade tail, a release) and no
/// output failure waiting for its retry. A load is about to play.
fn output_silent(status: PlaybackStatus, fading: bool, failure_pending: bool) -> bool {
    matches!(status, PlaybackStatus::Paused | PlaybackStatus::Idle) && !fading && !failure_pending
}

/// The source a paused track kept when the output closed under it
/// (`Worker::close_idle_output`), and the queue entry it belongs to.
struct ParkedTrack {
    entry_id: String,
    item_id: String,
    source: source::Parked,
}

impl ParkedTrack {
    /// It is `track`'s: the same entry, not just another copy of the song.
    fn holds(&self, track: &QueueTrack) -> bool {
        track.entry_id == self.entry_id && track.item_id == self.item_id
    }
}

/// Whether a parked source is still wanted (`Worker::drop_stale_parked`):
/// its track is the current one (`holds`), paused, and has no player — the
/// output closed under it, and Play has not brought it back yet. A device
/// opened meanwhile (one picked in Settings) changes none of that.
fn keeps_parked(status: PlaybackStatus, has_player: bool, holds: bool) -> bool {
    status == PlaybackStatus::Paused && !has_player && holds
}

/// How long the open output has played nothing (`Worker::watch_idle_output`).
#[derive(Debug, Default)]
struct IdleClock {
    since: Option<Instant>,
}

impl IdleClock {
    /// Look at the output at `now`: whether it is open, and silent
    /// ([`output_silent`]). True once it has been both for
    /// [`IDLE_OUTPUT_CLOSE`]: it closes now.
    fn due(&mut self, open: bool, silent: bool, now: Instant) -> bool {
        if !open || !silent {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if now.saturating_duration_since(since) < IDLE_OUTPUT_CLOSE {
            return false;
        }
        self.since = None;
        true
    }

    /// A stream opened: its silence counts from now.
    fn reset(&mut self) {
        self.since = None;
    }
}

/// One attempt of an output opened again for playback
/// (`Worker::reopen_output`), as at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReopenStep {
    /// The configured device: by its stored id, or by its name as last seen
    /// should Windows list it under a new id. Opened, it keeps the id it
    /// opened under (`adopt_output_device`).
    Configured { id: String, name: Option<String> },
    /// The system default, as itself: "system default" is the setting, or
    /// the default stands in for the configured device already.
    Default,
    /// The system default standing in for the configured device `requested`
    /// (its stored id), which did not open: marked as a fallback, with the
    /// toast, until Settings switches back.
    FallBack { requested: String },
}

impl ReopenStep {
    /// What to try first. A fallback stays on the default: the device coming
    /// back does not take playback back by itself.
    fn first(configured: Option<&str>, name: Option<&str>, fallback_active: bool) -> Self {
        match configured {
            Some(id) if !fallback_active => Self::Configured {
                id: id.to_string(),
                name: name.map(str::to_string),
            },
            _ => Self::Default,
        }
    }

    /// What to try once this did not open; None: the reopen failed.
    fn after_failure(&self) -> Option<Self> {
        match self {
            Self::Configured { id, .. } => Some(Self::FallBack {
                requested: id.clone(),
            }),
            Self::Default | Self::FallBack { .. } => None,
        }
    }

    /// The device to open, and the name to find it by, for
    /// [`open_output_named`]; None is the system default.
    fn device(&self) -> (Option<String>, Option<&str>) {
        match self {
            Self::Configured { id, name } => (Some(id.clone()), name.as_deref()),
            Self::Default | Self::FallBack { .. } => (None, None),
        }
    }

    /// Whether the device that opened is the configured one, whose id is
    /// then stored as it opened.
    fn adopts(&self) -> bool {
        matches!(self, Self::Configured { .. })
    }

    /// The configured device the opened default stands in for, to mark as a
    /// fallback (with the toast).
    fn stands_in_for(&self) -> Option<&str> {
        match self {
            Self::FallBack { requested } => Some(requested),
            Self::Configured { .. } | Self::Default => None,
        }
    }
}

/// What a reopen at a position that did not open leaves
/// (`Worker::reopen_failed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedReopen {
    /// It resumed a paused track whose player went with the idle close
    /// (`resume_closed`): paused again where it was, in the same play
    /// session, with what was heard of it, for Play to try again. Closing
    /// the session here would make one listen two — two history rows, two
    /// scrobbles — once the next Play started a new one.
    StayPaused,
    /// A reopen of a track reported started (a seek in a transcode, a device
    /// switch, a broken stream picked up): its session is closed like at any
    /// stop, and the track stops where it was to start.
    CloseSession,
    /// An open at a position before the track was reported started (Play
    /// from a seek target while stopped): it simply failed.
    Fail,
}

/// [`FailedReopen`] for a reopen that did not open. `resuming`: it was the
/// reload of a paused track after the idle close (`Worker::fade_in_arrival`);
/// `reported`: its play session was reported started.
fn failed_reopen(resuming: bool, reported: bool) -> FailedReopen {
    if resuming {
        FailedReopen::StayPaused
    } else if reported {
        FailedReopen::CloseSession
    } else {
        FailedReopen::Fail
    }
}

/// Whether an `OutputFailed` is one to act on: from the stream that plays
/// now (its error callback set `dead`), not from one replaced since, and not
/// while the output is closed for silence — a stream closed on purpose
/// failed nobody.
fn output_failure_counts(open: bool, dead: bool) -> bool {
    open && dead
}

#[cfg(test)]
mod output_tests {
    use super::*;

    fn entry(id: &str, name: &str, legacy: &str) -> OutputEntry {
        OutputEntry {
            device: OutputDevice {
                id: id.to_string(),
                name: name.to_string(),
            },
            legacy_name: Some(legacy.to_string()),
        }
    }

    /// Onboard audio and a USB headset: Windows calls both "Speakers".
    fn twins() -> Vec<OutputEntry> {
        vec![
            entry(
                "wasapi:{onboard}",
                "Speakers (Realtek(R) Audio)",
                "Speakers",
            ),
            entry("wasapi:{usb}", "Speakers (USB Audio Device)", "Speakers"),
        ]
    }

    #[test]
    fn a_device_is_found_by_its_id_even_when_its_name_is_shared() {
        let outputs = twins();
        let usb_name = Some("Speakers (USB Audio Device)");
        assert_eq!(
            resolve_output("wasapi:{usb}", None, outputs.iter()),
            Some(1)
        );
        assert_eq!(
            resolve_output("wasapi:{onboard}", usb_name, outputs.iter()),
            Some(0)
        );
        assert_eq!(resolve_output("wasapi:{gone}", None, outputs.iter()), None);
    }

    #[test]
    fn a_device_listed_under_a_new_id_is_found_by_its_name() {
        // Windows made a new endpoint for the same device: a Focusrite
        // interface switched to 48 kHz (one endpoint per rate), a re-paired
        // Bluetooth headset, a USB headset without a serial number in
        // another port. New id, same name.
        let name = "Speakers (Focusrite USB Audio)";
        let outputs = [
            entry(
                "wasapi:{onboard}",
                "Speakers (Realtek(R) Audio)",
                "Speakers",
            ),
            entry("wasapi:{48k}", name, "Speakers"),
        ];
        assert_eq!(
            resolve_output("wasapi:{44k}", Some(name), outputs.iter()),
            Some(1)
        );
        // Without the name as last seen (a choice that never opened), or
        // under a name nothing has: not there.
        assert_eq!(resolve_output("wasapi:{44k}", None, outputs.iter()), None);
        assert_eq!(
            resolve_output(
                "wasapi:{44k}",
                Some("Speakers (Scarlett Solo USB)"),
                outputs.iter()
            ),
            None
        );

        let mut playback = PlaybackSettings {
            output_device: Some("wasapi:{44k}".to_string()),
            output_device_name: Some(name.to_string()),
            ..PlaybackSettings::default()
        };
        assert!(adopt_output_device(&mut playback, &outputs[1].device));
        assert_eq!(playback.output_device.as_deref(), Some("wasapi:{48k}"));
    }

    #[test]
    fn a_name_two_devices_share_finds_neither() {
        // Two identical headsets plugged in, the chosen one's id gone:
        // which one it was is anyone's guess, so the fallback decides.
        let name = "Headphones (USB Audio Device)";
        let outputs = [
            entry("wasapi:{left}", name, "Headphones"),
            entry("wasapi:{right}", name, "Headphones"),
        ];
        assert_eq!(
            resolve_output("wasapi:{gone}", Some(name), outputs.iter()),
            None
        );
        // By id, each is still its own.
        assert_eq!(
            resolve_output("wasapi:{right}", Some(name), outputs.iter()),
            Some(1)
        );
    }

    #[test]
    fn a_stale_choice_of_the_device_playing_is_no_switch() {
        let name = "Speakers (Focusrite USB Audio)";
        let outputs = [
            entry(
                "wasapi:{onboard}",
                "Speakers (Realtek(R) Audio)",
                "Speakers",
            ),
            entry("wasapi:{48k}", name, "Speakers"),
        ];
        let playing = &outputs[1].device;
        // A page loaded while the device still had its old id.
        assert!(names_output(
            "wasapi:{44k}",
            Some(name),
            playing,
            outputs.iter()
        ));
        assert!(names_output(
            "wasapi:{48k}",
            Some(name),
            playing,
            outputs.iter()
        ));
        // A real pick of another device is one.
        let onboard = Some("Speakers (Realtek(R) Audio)");
        assert!(!names_output(
            "wasapi:{onboard}",
            onboard,
            playing,
            outputs.iter()
        ));
        // So is a device that is not there at all.
        assert!(!names_output(
            "wasapi:{gone}",
            None,
            playing,
            outputs.iter()
        ));
    }

    #[test]
    fn a_choice_saved_by_name_means_the_device_older_builds_opened() {
        // 0.6.0 stored "Speakers" and opened the first device of that name.
        let outputs = twins();
        let index = resolve_output("Speakers", None, outputs.iter()).unwrap();
        assert_eq!(index, 0);

        let mut playback: PlaybackSettings = serde_json::from_str(
            r#"{"crossfadeMode":"smart","crossfadeSeconds":4,"outputDevice":"Speakers"}"#,
        )
        .unwrap();
        assert_eq!(playback.output_device_name, None);
        assert!(adopt_output_device(&mut playback, &outputs[index].device));
        assert_eq!(playback.output_device.as_deref(), Some("wasapi:{onboard}"));
        assert_eq!(
            playback.output_device_name.as_deref(),
            Some("Speakers (Realtek(R) Audio)")
        );
        // The rest of the settings is untouched, and adopting again is a no-op.
        assert_eq!(playback.crossfade_mode, CrossfadeMode::Smart);
        assert!(!adopt_output_device(&mut playback, &outputs[index].device));
    }

    #[test]
    fn the_settings_page_shows_the_full_device_name() {
        use rodio::cpal::DeviceDescriptionBuilder;
        let windows = DeviceDescriptionBuilder::new("Speakers")
            .add_extended_line("Speakers (Realtek(R) Audio)")
            .build();
        assert_eq!(display_name(&windows), "Speakers (Realtek(R) Audio)");
        let plain = DeviceDescriptionBuilder::new("Built-in Output").build();
        assert_eq!(display_name(&plain), "Built-in Output");
    }

    /// `attempts` failed attempts on `id`, the last one `ago`.
    fn failed(id: &str, attempts: u32, ago: Duration) -> FailedDefault {
        FailedDefault {
            id: id.to_string(),
            attempts,
            at: Instant::now().checked_sub(ago).unwrap(),
        }
    }

    #[test]
    fn system_default_follows_a_new_default() {
        let (bt, speakers) = ("wasapi:{bt}", "wasapi:{speakers}");
        // The default moved: follow it.
        assert!(should_follow_default(bt, Some(speakers), None, None));
        // Already there.
        assert!(!should_follow_default(bt, Some(bt), None, None));
        // Nothing opened yet: any default is worth a try.
        assert!(should_follow_default(bt, None, None, None));
    }

    #[test]
    fn a_default_that_did_not_open_gets_a_few_more_tries() {
        // Bluetooth headphones become the default a moment before they can
        // play: the first attempt can fail.
        let (bt, speakers) = ("wasapi:{bt}", "wasapi:{speakers}");
        let just_now = failed(bt, 1, Duration::ZERO);
        assert!(!should_follow_default(
            bt,
            Some(speakers),
            Some(&just_now),
            None
        ));
        let a_moment_ago = failed(bt, 1, DEFAULT_OUTPUT_RETRY);
        assert!(should_follow_default(
            bt,
            Some(speakers),
            Some(&a_moment_ago),
            None
        ));
        // Then it is left alone until the default changes again.
        let too_often = failed(bt, DEFAULT_OUTPUT_ATTEMPTS, DEFAULT_OUTPUT_RETRY * 2);
        assert!(!should_follow_default(
            bt,
            Some(speakers),
            Some(&too_often),
            None
        ));
        // What failed on another device holds a new default back in no way.
        let other = failed("wasapi:{usb}", DEFAULT_OUTPUT_ATTEMPTS, Duration::ZERO);
        assert!(should_follow_default(
            bt,
            Some(speakers),
            Some(&other),
            None
        ));

        // Attempts count on for one device and start over for the next.
        let second = FailedDefault::after(Some(failed(bt, 1, Duration::ZERO)), bt.to_string());
        assert_eq!(second.attempts, 2);
        let fresh = FailedDefault::after(Some(second), "wasapi:{usb}".to_string());
        assert_eq!(fresh.attempts, 1);
        assert_eq!(FailedDefault::after(None, bt.to_string()).attempts, 1);
    }

    #[test]
    fn a_settled_stream_gets_its_device_back_before_the_default() {
        let long = OUTPUT_SETTLED + Duration::from_secs(1);
        // Reconfigured under us after playing for a while: reopen it.
        assert!(reopen_configured_first(true, false, long));
        // Died right after opening: on its way out.
        assert!(!reopen_configured_first(
            true,
            false,
            Duration::from_secs(3)
        ));
        // "System default", or already standing in for the device.
        assert!(!reopen_configured_first(false, false, long));
        assert!(!reopen_configured_first(true, true, long));
    }

    fn handle() -> (PlayerHandle, mpsc::Receiver<PlayerCommand>) {
        let (tx, rx) = mpsc::channel();
        let handle = PlayerHandle {
            tx,
            state: Arc::new(Mutex::new(PlayerState::default())),
            queue: Arc::new(Mutex::new(QueueSnapshot {
                tracks: Vec::new(),
                index: 0,
                order: Vec::new(),
                can_undo: false,
                played_count: 0,
                duplicate_count: 0,
                revision: 0,
            })),
            dsp: Arc::new(AudioDsp::new(DspParams::default())),
            tap: Arc::new(tap::VisualizerTap::new()),
            waveforms: Arc::new(waveform::WaveformCache::default()),
            analyses: Arc::new(analysis::AnalysisCache::new(None)),
            play_requests: Default::default(),
            enqueue_turn: Default::default(),
        };
        (handle, rx)
    }

    fn asking_for(device: &str) -> PlaybackSettings {
        PlaybackSettings {
            output_device: Some(device.to_string()),
            output_device_name: Some("Busy DAC".to_string()),
            ..PlaybackSettings::default()
        }
    }

    #[test]
    fn settings_answer_with_the_output_that_stayed() {
        // The worker could not open the device and kept the system default:
        // that, not the request, is what Settings has to show.
        let (handle, rx) = handle();
        let worker = std::thread::spawn(move || {
            if let Ok(PlayerCommand::SetPlaybackSettings {
                mut settings,
                reply,
            }) = rx.recv()
            {
                settings.output_device = None;
                settings.output_device_name = None;
                reply.expect("a reply channel").send(settings).unwrap();
            }
        });
        let applied = handle
            .set_playback_settings(asking_for("wasapi:{busy}"), Duration::from_secs(5))
            .unwrap();
        assert_eq!(applied.output_device, None);
        worker.join().unwrap();
    }

    #[test]
    fn settings_without_an_answer_are_what_was_asked() {
        // No output at all yet: the worker applies them once it has one.
        let (handle, _rx) = handle();
        let asked = asking_for("wasapi:{dac}");
        let applied = handle
            .set_playback_settings(asked.clone(), Duration::from_millis(20))
            .unwrap();
        assert_eq!(applied, asked);
    }

    #[test]
    fn the_idle_clock_fires_once_after_the_whole_while_of_open_silence() {
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let close = IDLE_OUTPUT_CLOSE.as_secs();
        let mut clock = IdleClock::default();
        // Open and silent: not yet.
        assert!(!clock.due(true, true, t0));
        assert!(!clock.due(true, true, at(close - 1)));
        // Silent for the whole while: due now, once.
        assert!(clock.due(true, true, at(close)));
        // Closed, it is never due again, however long the silence goes on.
        assert!(!clock.due(false, true, at(close + 1)));
        assert!(!clock.due(false, true, at(10 * close)));
    }

    #[test]
    fn silence_is_paused_or_stopped_with_nothing_ramping_or_failing() {
        use PlaybackStatus::*;
        assert!(output_silent(Paused, false, false));
        assert!(output_silent(Idle, false, false));
        // Playing, or loading to play, is not; neither is a crossfade tail
        // or a release still ramping out, nor a failure waiting for its
        // retry.
        assert!(!output_silent(Playing, false, false));
        assert!(!output_silent(Loading, false, false));
        assert!(!output_silent(Idle, true, false));
        assert!(!output_silent(Paused, true, false));
        assert!(!output_silent(Paused, false, true));
    }

    #[test]
    fn a_parked_source_is_kept_only_while_its_track_waits_paused_without_a_player() {
        use PlaybackStatus::*;
        // Closed under the paused track, before Play: kept.
        assert!(keeps_parked(Paused, false, true));
        // Play brought it back (it has a player again, or it loads), it was
        // stopped, or another track or entry is current now: let go of.
        assert!(!keeps_parked(Paused, true, true));
        assert!(!keeps_parked(Loading, false, true));
        assert!(!keeps_parked(Playing, true, true));
        assert!(!keeps_parked(Idle, false, true));
        assert!(!keeps_parked(Paused, false, false));
    }

    #[test]
    fn the_idle_clock_starts_over_after_a_moment_of_sound() {
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let close = IDLE_OUTPUT_CLOSE.as_secs();
        let mut clock = IdleClock::default();
        assert!(!clock.due(true, true, t0));
        assert!(!clock.due(true, false, at(close - 10)));
        assert!(!clock.due(true, true, at(close - 9)));
        assert!(!clock.due(true, true, at(close + 60)));
        assert!(clock.due(true, true, at(2 * close - 9)));
    }

    #[test]
    fn a_reset_idle_clock_counts_from_its_next_look() {
        // What `output_opened` does for a stream opened during a long
        // pause (a device picked in Settings): it gets a whole while.
        let t0 = Instant::now();
        let close = IDLE_OUTPUT_CLOSE.as_secs();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let mut clock = IdleClock::default();
        assert!(!clock.due(true, true, t0));
        clock.reset();
        assert!(!clock.due(true, true, at(close)));
        assert!(!clock.due(true, true, at(2 * close - 1)));
        assert!(clock.due(true, true, at(2 * close)));
    }

    #[test]
    fn a_reopen_tries_the_configured_device_by_id_and_name_then_the_default() {
        let name = Some("Speakers (Focusrite USB Audio)");
        let first = ReopenStep::first(Some("wasapi:{44k}"), name, false);
        // By its id, and by the name it was last seen under, should Windows
        // list it under a new one: what opens keeps the id it opened under.
        assert_eq!(first.device(), (Some("wasapi:{44k}".to_string()), name));
        assert!(first.adopts());
        assert_eq!(first.stands_in_for(), None);
        // It did not open: the default stands in, marked as a fallback for
        // the configured device (the toast names it), with no id adopted.
        let fallback = first.after_failure().expect("the default is tried next");
        assert_eq!(fallback.device(), (None, None));
        assert_eq!(fallback.stands_in_for(), Some("wasapi:{44k}"));
        assert!(!fallback.adopts());
        // And when that does not open either, the reopen fails.
        assert_eq!(fallback.after_failure(), None);
    }

    #[test]
    fn a_reopen_for_system_default_or_a_fallback_opens_the_default_as_itself() {
        // "System default": whatever the default is by then, nothing to
        // adopt or to mark, and nothing after it.
        let default = ReopenStep::first(None, None, false);
        assert_eq!(default, ReopenStep::Default);
        assert_eq!(default.device(), (None, None));
        assert!(!default.adopts());
        assert_eq!(default.stands_in_for(), None);
        assert_eq!(default.after_failure(), None);
        // The default stood in for the device before the close: it goes on
        // standing in, also when the device is back by then (switching back
        // is the user's call), and is not marked or announced again.
        let standing_in = ReopenStep::first(Some("wasapi:{dac}"), Some("USB DAC"), true);
        assert_eq!(standing_in, ReopenStep::Default);
        assert_eq!(standing_in.stands_in_for(), None);
    }

    #[test]
    fn a_failed_resume_after_the_close_stays_paused_in_its_session() {
        // Play after the idle close could not reload the track: paused
        // again where it was, the session open, the listen one listen.
        assert_eq!(failed_reopen(true, true), FailedReopen::StayPaused);
        assert_eq!(failed_reopen(true, false), FailedReopen::StayPaused);
        // Any other reopen of a track reported started closes its session;
        // one before any start report just fails.
        assert_eq!(failed_reopen(false, true), FailedReopen::CloseSession);
        assert_eq!(failed_reopen(false, false), FailedReopen::Fail);
    }

    #[test]
    fn a_failure_counts_only_when_the_open_stream_died() {
        // The stream that plays died: handled.
        assert!(output_failure_counts(true, true));
        // The error of a stream replaced since: not.
        assert!(!output_failure_counts(true, false));
        // Closed for silence: whatever the old stream still reports, there
        // is nothing to fall back from.
        assert!(!output_failure_counts(false, true));
        assert!(!output_failure_counts(false, false));
    }

    #[test]
    fn the_default_is_watched_only_on_an_open_healthy_output() {
        let settled = DEFAULT_OUTPUT_POLL;
        // "System default" (or a fallback) on an open, healthy output.
        assert!(watches_default(true, true, true, false, settled));
        // Closed for silence: no stream for a new default; Play opens
        // whatever the default is then.
        assert!(!watches_default(true, false, true, false, settled));
        // As before: a configured device, a dead output, a failure waiting
        // for its retry, a rebuild a moment ago.
        assert!(!watches_default(false, true, true, false, settled));
        assert!(!watches_default(true, true, false, false, settled));
        assert!(!watches_default(true, true, true, true, settled));
        assert!(!watches_default(
            true,
            true,
            true,
            false,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn a_fallback_follows_the_default_but_not_back_to_its_device() {
        let (dac, bt, speakers) = ("wasapi:{dac}", "wasapi:{bt}", "wasapi:{speakers}");
        // Standing in for the DAC on the speakers: headphones that become
        // the default take over.
        assert!(should_follow_default(bt, Some(speakers), None, Some(dac)));
        // The DAC itself coming back as the default does not: switching
        // back stays the user's call.
        assert!(!should_follow_default(dac, Some(speakers), None, Some(dac)));
    }
}

fn init_media_controls(
    hwnd: Option<isize>,
    tx: Sender<PlayerCommand>,
) -> Option<souvlaki::MediaControls> {
    let hwnd = hwnd?;
    let config = souvlaki::PlatformConfig {
        dbus_name: "sonarune",
        display_name: "Sonarune",
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
pub fn load_persisted_queue(store: &Store) -> Option<RestoredQueue> {
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
    let (index, position_ms) = restored_index(&snapshot, store.get(keys::QUEUE_INDEX).as_deref());
    Some(RestoredQueue {
        tracks: snapshot.tracks,
        index,
        position_ms,
        order: snapshot.order,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        album_gain_from_queue, album_run, buffered_range_ms, build_album_shuffle_order,
        build_shuffle_order, choose_auto_dj_seed, defer_command, duplicate_count,
        duplicate_indices, fresh_mix, landing_after_removal, load_persisted_queue, order_next,
        order_prev, played_count, played_indices, plays_on_after_removal, queue_write,
        remap_after_removals, remap_move, remap_order_after_removals, restored_index,
        restored_order, should_crossfade, should_start_prefetch, sleep_level, start_player,
        stop_after_due, stop_entry, AutoDjReason, AutoDjRetry, AutoDjSeedMode, CrossfadeMode,
        EntryRef, ExtrasSettings, PersistedOrder, PersistedQueue, PersistedQueueMark, PlayMode,
        PlaybackStatus, PlayerCommand, QueueTrack, QueueWrite, ShuffleMode, StopAfter, TickClock,
        AUTO_DJ_ATTEMPTS, AUTO_DJ_RETRY_AFTER, TICK,
    };
    use super::{listener, sleep_fade_step, stream_broke_off, Mix, SleepFadeStep};
    use crate::api::JellyfinClient;
    use crate::store::{keys, Store};
    use proptest::prelude::*;
    use rodio::buffer::SamplesBuffer;
    use std::time::Duration;

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

    #[test]
    fn the_state_tells_the_ui_when_a_load_arrives_paused() {
        // TransportControls and the mini player read `pendingPause` to offer
        // Play instead of Pause while such a load runs (types.ts PlayerState).
        let state = super::PlayerState {
            status: super::PlaybackStatus::Loading,
            pending_pause: true,
            ..super::PlayerState::default()
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["status"], "loading");
        assert_eq!(json["pendingPause"], true);
        let idle = serde_json::to_value(super::PlayerState::default()).unwrap();
        assert_eq!(idle["pendingPause"], false);
    }

    #[test]
    fn the_media_overlay_shows_what_a_load_will_do() {
        use super::{arrives_paused, media_playback, PlaybackStatus};
        use souvlaki::{MediaPlayback, MediaPosition};
        use std::time::Duration;

        let at = Duration::from_secs(42);
        let progress = Some(MediaPosition(at));
        // Pause pressed during the load, or a paused track reopened: like the
        // window's button, the overlay offers Play, and its Play/Pause buttons
        // (souvlaki sends Play or Pause on Windows, never Toggle) do what they show.
        assert_eq!(
            media_playback(PlaybackStatus::Loading, true, at),
            MediaPlayback::Paused { progress }
        );
        assert_eq!(
            media_playback(PlaybackStatus::Loading, false, at),
            MediaPlayback::Playing { progress }
        );
        assert_eq!(
            media_playback(PlaybackStatus::Playing, false, at),
            MediaPlayback::Playing { progress }
        );
        assert_eq!(
            media_playback(PlaybackStatus::Paused, false, at),
            MediaPlayback::Paused { progress }
        );
        // Only a load arrives paused; a flag left over from an open that
        // failed means nothing, on the overlay or in the state the UI gets.
        assert_eq!(
            media_playback(PlaybackStatus::Idle, true, at),
            MediaPlayback::Stopped
        );
        assert_eq!(
            media_playback(PlaybackStatus::Playing, true, at),
            MediaPlayback::Playing { progress }
        );
        assert!(arrives_paused(PlaybackStatus::Loading, true));
        assert!(!arrives_paused(PlaybackStatus::Loading, false));
        assert!(!arrives_paused(PlaybackStatus::Idle, true));
        assert!(!arrives_paused(PlaybackStatus::Paused, true));
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

        /// The count the queue panel shows is the list "remove played" takes.
        #[test]
        fn prop_played_count_is_the_played_list(
            len in 0usize..48,
            current_raw in 0usize..48,
            seed in any::<u64>(),
            shuffled in any::<bool>(),
        ) {
            let current = if len == 0 { 0 } else { current_raw % len };
            let order = if shuffled && len > 0 {
                build_shuffle_order(len, (seed as usize) % len, seed)
            } else {
                Vec::new()
            };
            prop_assert_eq!(
                played_count(len, current, &order),
                played_indices(len, current, &order).len()
            );
        }

        /// Removing the current entry lands on an entry of the shortened
        /// queue, and with a successor on exactly that successor.
        #[test]
        fn prop_removal_lands_on_the_successor(
            len in 2usize..40,
            removed_raw in 0usize..40,
            seed in any::<u64>(),
            repeat_all in any::<bool>(),
        ) {
            let removed = removed_raw % len;
            let order = build_shuffle_order(len, (seed as usize) % len, seed);
            let (landing, onward) = landing_after_removal(len, removed, &order, repeat_all)
                .expect("a queue of two or more keeps an entry");
            prop_assert!(landing < len - 1);
            let after: Vec<usize> = order
                .iter()
                .filter(|&&i| i != removed)
                .map(|&i| if i > removed { i - 1 } else { i })
                .collect();
            let next = order_next(len, removed, &order, repeat_all);
            if onward {
                let next = next.unwrap();
                prop_assert_eq!(landing, if next > removed { next - 1 } else { next });
            } else {
                prop_assert!(next.is_none());
                prop_assert_eq!(after.last().copied(), Some(landing));
            }
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

    fn gain_track(item_id: &str, album_id: &str, gain: Option<f32>) -> QueueTrack {
        QueueTrack {
            normalization_gain: gain,
            ..album_track(item_id, album_id)
        }
    }

    #[test]
    fn an_album_gain_is_the_median_of_its_tracks() {
        let queue = vec![
            gain_track("a", "one", Some(-6.0)),
            gain_track("b", "one", Some(-2.0)),
            gain_track("c", "one", Some(-4.0)),
            // Another album's loud single must not drag the album down.
            gain_track("d", "two", Some(-20.0)),
        ];
        assert_eq!(album_gain_from_queue(&queue, "one"), Some(-4.0));
        assert_eq!(album_gain_from_queue(&queue, "two"), Some(-20.0));
        assert_eq!(album_gain_from_queue(&queue, "three"), None);
    }

    #[test]
    fn an_even_number_of_tracks_averages_the_middle_two() {
        let queue = vec![
            gain_track("a", "one", Some(-6.0)),
            gain_track("b", "one", Some(-2.0)),
        ];
        assert_eq!(album_gain_from_queue(&queue, "one"), Some(-4.0));
    }

    #[test]
    fn tracks_without_a_usable_gain_are_left_out() {
        let queue = vec![
            gain_track("a", "one", None),
            gain_track("b", "one", Some(f32::NAN)),
            gain_track("c", "one", Some(-3.0)),
        ];
        // Only the one real value is left, so it is the median.
        assert_eq!(album_gain_from_queue(&queue, "one"), Some(-3.0));
        // Nothing usable at all: no album gain, and the caller falls back to
        // the track's own.
        let empty = vec![gain_track("a", "one", None)];
        assert_eq!(album_gain_from_queue(&empty, "one"), None);
    }

    fn album_track(item_id: &str, album_id: &str) -> QueueTrack {
        QueueTrack {
            album_id: Some(album_id.into()),
            ..queue_track(item_id)
        }
    }

    /// A queue line with its own entry id, as `to_queue_tracks` builds it.
    fn entry(item_id: &str, entry_id: &str) -> QueueTrack {
        QueueTrack {
            entry_id: entry_id.into(),
            ..queue_track(item_id)
        }
    }

    fn album_entry(entry_id: &str, album_id: &str) -> QueueTrack {
        QueueTrack {
            album_id: Some(album_id.into()),
            ..entry(entry_id, entry_id)
        }
    }

    #[test]
    fn stop_after_track_fires_on_the_entry_it_was_armed_on() {
        let a = album_track("a", "album-1");
        let b = album_track("b", "album-1");
        let at = EntryRef::of(&a);
        let armed = |finished: &QueueTrack| {
            stop_after_due(StopAfter::Track, Some(&at), None, finished, false)
        };
        assert!(armed(&a));
        // Another track of the same album ends: not the one that was armed.
        assert!(!armed(&b));
        assert!(!stop_after_due(StopAfter::Off, Some(&at), None, &a, false));
    }

    #[test]
    fn stop_after_a_second_copy_does_not_fire_on_the_first() {
        // The same track queued twice: armed on the later copy.
        let first = entry("x", "e1");
        let second = entry("x", "e2");
        let at = EntryRef::of(&second);
        assert!(!stop_after_due(
            StopAfter::Track,
            Some(&at),
            None,
            &first,
            false
        ));
        assert!(stop_after_due(
            StopAfter::Track,
            Some(&at),
            None,
            &second,
            false
        ));
    }

    #[test]
    fn an_entry_from_before_entry_ids_is_matched_by_its_item() {
        let legacy = queue_track("x");
        let named = entry("x", "e1");
        let by_item = EntryRef {
            entry_id: String::new(),
            item_id: "x".into(),
        };
        assert!(by_item.matches(&legacy));
        assert!(by_item.matches(&named));
        assert!(EntryRef::of(&named).matches(&legacy));
        assert!(!EntryRef::of(&named).matches(&entry("x", "e2")));
        assert!(!by_item.matches(&entry("y", "e1")));
    }

    #[test]
    fn an_album_stop_ends_with_the_last_track_of_the_run_it_was_armed_in() {
        // B1, then a single X put "next", then the rest of B.
        let queue = vec![
            album_entry("b1", "B"),
            album_entry("x", "X"),
            album_entry("b2", "B"),
            album_entry("b3", "B"),
            album_entry("b4", "B"),
            album_entry("c1", "C"),
        ];
        // Armed on the last row of B: that row, not the end of B1's run.
        assert_eq!(album_run_end(&queue, &[], 4), 4);
        assert_eq!(album_run_end(&queue, &[], 2), 4);
        // Armed on B1 itself: X follows, so that run is B1 alone.
        assert_eq!(album_run_end(&queue, &[], 0), 0);
        let at = EntryRef::of(&queue[4]);
        let due = |finished: &QueueTrack| {
            stop_after_due(StopAfter::Album, Some(&at), Some("B"), finished, false)
        };
        assert!(!due(&queue[0]), "B1 ending with X next is not the stop");
        assert!(!due(&queue[3]));
        assert!(due(&queue[4]));
    }

    #[test]
    fn an_album_stop_has_an_end_under_repeat_all() {
        // One album, Repeat All: after the last track the queue wraps to the
        // first of the same album — the run still ends at the last one.
        let queue = vec![
            album_entry("a1", "A"),
            album_entry("a2", "A"),
            album_entry("a3", "A"),
        ];
        assert_eq!(album_run_end(&queue, &[], 0), 2);
        assert_eq!(album_run_end(&queue, &[1, 2, 0], 1), 0);
    }

    #[test]
    fn an_album_stop_on_a_second_copy_of_the_album_ends_that_copy() {
        let copy = |item_id: &str, entry_id: &str, album_id: &str| QueueTrack {
            album_id: Some(album_id.into()),
            ..entry(item_id, entry_id)
        };
        let queue = vec![
            copy("a1", "e1", "A"),
            copy("a2", "e2", "A"),
            copy("c1", "e3", "C"),
            copy("a1", "e4", "A"),
            copy("a2", "e5", "A"),
        ];
        assert_eq!(album_run_end(&queue, &[], 3), 4);
        assert_eq!(album_run_end(&queue, &[], 0), 1);
        // In shuffle order the run follows the order, not the stored list.
        assert_eq!(album_run_end(&queue, &[4, 2, 0, 1, 3], 0), 1);
        // Where the album starts over, the run ends: the next copy is not
        // this one.
        assert_eq!(album_run_end(&queue, &[0, 3, 4, 1, 2], 0), 0);
        let back_to_back = vec![
            copy("a1", "e1", "A"),
            copy("a2", "e2", "A"),
            copy("a1", "e3", "A"),
            copy("a2", "e4", "A"),
        ];
        assert_eq!(album_run_end(&back_to_back, &[], 0), 1);
        assert_eq!(album_run_end(&back_to_back, &[], 2), 3);
    }

    /// Where an album stop armed on `armed` ends (the last entry it covers).
    fn album_run_end(tracks: &[QueueTrack], order: &[usize], armed: usize) -> usize {
        *album_run(tracks, order, armed).last().unwrap()
    }

    fn copy_of(item_id: &str, entry_id: &str) -> QueueTrack {
        QueueTrack {
            album_id: Some("A".into()),
            ..entry(item_id, entry_id)
        }
    }

    #[test]
    fn an_album_stop_armed_in_the_middle_of_a_copy_ends_with_that_copy() {
        // A1-A3 queued twice back to back, armed on A2 or A3 of the first.
        let back_to_back: Vec<QueueTrack> = ["a1", "a2", "a3", "a1", "a2", "a3"]
            .iter()
            .enumerate()
            .map(|(i, item)| copy_of(item, &format!("e{i}")))
            .collect();
        assert_eq!(album_run(&back_to_back, &[], 1), [1, 2]);
        assert_eq!(album_run_end(&back_to_back, &[], 2), 2);
        // Armed in the second copy, it ends with that one.
        assert_eq!(album_run(&back_to_back, &[], 4), [4, 5]);
        assert_eq!(album_run_end(&back_to_back, &[], 3), 5);
        // The album, then A1 queued again on its own.
        let requeued: Vec<QueueTrack> = ["a1", "a2", "a3", "a1"]
            .iter()
            .enumerate()
            .map(|(i, item)| copy_of(item, &format!("e{i}")))
            .collect();
        assert_eq!(album_run_end(&requeued, &[], 1), 2);
        assert_eq!(album_run_end(&requeued, &[], 3), 3);
        // Album shuffle: the two copies as blocks next to each other, the
        // first one started at its second track.
        assert_eq!(album_run(&back_to_back, &[4, 5, 3, 0, 1, 2], 4), [4, 5, 3]);
        assert_eq!(album_run_end(&back_to_back, &[4, 5, 3, 0, 1, 2], 1), 2);
    }

    #[test]
    fn an_album_stop_ends_with_the_last_of_its_tracks_still_queued() {
        let queue: Vec<QueueTrack> = ["b1", "b2", "b3", "b4"]
            .iter()
            .map(|item| copy_of(item, item))
            .collect();
        // Armed on B2: it covers B2 to B4.
        let run: Vec<EntryRef> = album_run(&queue, &[], 1)
            .into_iter()
            .map(|index| EntryRef::of(&queue[index]))
            .collect();
        assert_eq!(stop_entry(&run, &queue), Some(&run[2]));
        // B4 removed: the album ends after B3 now.
        let without_b4 = &queue[..3];
        assert_eq!(stop_entry(&run, without_b4).unwrap().entry_id, "b3");
        // Removing the armed row itself changes nothing about the end.
        let without_b2 = [queue[0].clone(), queue[2].clone(), queue[3].clone()];
        assert_eq!(stop_entry(&run, &without_b2).unwrap().entry_id, "b4");
        // Undone, B4 is back, and so is the stop after it.
        assert_eq!(stop_entry(&run, &queue).unwrap().entry_id, "b4");
        // Nothing of it left (the queue replaced): no stop at all.
        assert_eq!(stop_entry(&run, &queue[..1]), None);
        assert_eq!(stop_entry(&[], &queue), None);
        // A queue line from before entry ids goes by its item.
        let legacy = [queue_track("b3")];
        assert_eq!(stop_entry(&run, &legacy).unwrap().entry_id, "b3");
    }

    #[test]
    fn a_load_waiting_to_pause_stays_paused_when_its_track_is_removed() {
        assert!(plays_on_after_removal(PlaybackStatus::Playing, false));
        assert!(plays_on_after_removal(PlaybackStatus::Loading, false));
        // Pause pressed while it loaded, or a seek made while paused.
        assert!(!plays_on_after_removal(PlaybackStatus::Loading, true));
        assert!(!plays_on_after_removal(PlaybackStatus::Paused, false));
        assert!(!plays_on_after_removal(PlaybackStatus::Idle, false));
    }

    #[test]
    fn under_repeat_one_an_album_ends_with_the_track_that_plays() {
        let a1 = album_entry("a1", "A");
        let a3 = album_entry("a3", "A");
        let other = album_entry("c1", "C");
        let at = EntryRef::of(&a3);
        let due = |finished: &QueueTrack| {
            stop_after_due(StopAfter::Album, Some(&at), Some("A"), finished, true)
        };
        assert!(due(&a1), "the album never reaches its end otherwise");
        assert!(!due(&other));
    }

    #[test]
    fn a_track_without_an_album_never_ends_an_album() {
        let loose = queue_track("a");
        let at = EntryRef::of(&loose);
        assert_eq!(album_run_end(&[loose.clone(), queue_track("b")], &[], 0), 0);
        // Armed with no album at all: a loose track ends nothing.
        assert!(!stop_after_due(
            StopAfter::Album,
            Some(&at),
            None,
            &loose,
            false
        ));
        assert!(!stop_after_due(
            StopAfter::Album,
            Some(&at),
            None,
            &loose,
            true
        ));
    }

    #[test]
    fn removing_the_current_track_lands_on_its_successor_in_play_order() {
        // Shuffled: 57 plays, followed by 3 and 120 in play order (here:
        // index 2, followed by 0, then 4).
        let order = [1, 2, 0, 4, 3];
        // Removing index 2: its successor 0 stays 0 after the shift.
        assert_eq!(landing_after_removal(5, 2, &order, false), Some((0, true)));
        // Removing the last in play order, repeat off: back to its predecessor
        // (4, which is 3 after the shift), and no playback goes on there.
        assert_eq!(landing_after_removal(5, 3, &order, false), Some((3, false)));
        // With Repeat All the order wraps to its head.
        assert_eq!(landing_after_removal(5, 3, &order, true), Some((1, true)));
        // Linear: the next one slides into the removed slot.
        assert_eq!(landing_after_removal(3, 1, &[], false), Some((1, true)));
        assert_eq!(landing_after_removal(3, 2, &[], false), Some((1, false)));
        // The last track left: nowhere to go.
        assert_eq!(landing_after_removal(1, 0, &[], true), None);
        assert_eq!(landing_after_removal(1, 0, &[0], false), None);
    }

    fn queue_track(item_id: &str) -> QueueTrack {
        QueueTrack {
            item_id: item_id.into(),
            name: item_id.into(),
            artist: String::new(),
            album: String::new(),
            album_id: None,
            album_artist: None,
            track_number: None,
            disc_number: None,
            duration_ms: 1,
            image_item_id: None,
            image_tag: None,
            image_blur_hash: None,
            stream_url: String::new(),
            entry_id: String::new(),
            normalization_gain: None,
            album_normalization_gain: None,
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
    fn smart_crossfade_leaves_an_album_without_disc_numbers_alone() {
        let track = |id: &str, disc: Option<i32>, number: i32| {
            let mut t = album_track(id, "album");
            t.disc_number = disc;
            t.track_number = Some(number);
            t.duration_ms = 180_000;
            t
        };
        // A rip without disc tags: the tracks follow each other on disc 1.
        assert!(!should_crossfade(
            CrossfadeMode::Smart,
            6_000,
            &track("a1", None, 1),
            &track("a2", None, 2)
        ));
        // A missing disc is disc 1, also next to a tagged one.
        assert!(!should_crossfade(
            CrossfadeMode::Smart,
            6_000,
            &track("a9", None, 9),
            &track("b1", Some(2), 1)
        ));
        // Out of order is still not a sequence.
        assert!(should_crossfade(
            CrossfadeMode::Smart,
            6_000,
            &track("a1", None, 1),
            &track("a3", None, 3)
        ));
        // Without track numbers there is no order to keep.
        let mut loose = track("x", None, 1);
        loose.track_number = None;
        assert!(should_crossfade(
            CrossfadeMode::Smart,
            6_000,
            &loose,
            &track("a2", None, 2)
        ));
    }

    #[test]
    fn deferred_commands_keep_the_latest_setting_and_every_new_queue() {
        let mut deferred = Vec::new();
        // The session restore, then a volume drag long enough to fill the
        // buffer many times over, then a sign-out and another account's play.
        defer_command(
            &mut deferred,
            PlayerCommand::RestoreQueue(super::RestoredQueue {
                tracks: vec![queue_track("old")],
                index: 0,
                position_ms: 0,
                order: None,
            }),
        );
        for step in 0..=1_000 {
            defer_command(
                &mut deferred,
                PlayerCommand::SetVolume {
                    volume: step as f32 / 1_000.0,
                },
            );
        }
        defer_command(&mut deferred, PlayerCommand::Toggle);
        defer_command(&mut deferred, PlayerCommand::ResetQueue);
        // The old queue and what was meant for it are moot now.
        assert_eq!(deferred.len(), 2);
        assert!(matches!(deferred[0], PlayerCommand::SetVolume { volume } if volume == 1.0));
        assert!(matches!(deferred[1], PlayerCommand::ResetQueue));
        defer_command(
            &mut deferred,
            PlayerCommand::PlayQueue {
                tracks: vec![queue_track("new")],
                start_index: 0,
                in_order: false,
            },
        );
        assert_eq!(deferred.len(), 2);
        assert!(matches!(
            &deferred[1],
            PlayerCommand::PlayQueue { tracks, .. } if tracks[0].item_id == "new"
        ));
    }

    #[test]
    fn a_full_deferred_buffer_still_takes_a_new_queue() {
        let mut deferred = Vec::new();
        for _ in 0..super::DEFERRED_COMMAND_LIMIT + 10 {
            defer_command(
                &mut deferred,
                PlayerCommand::SetStopAfter {
                    mode: StopAfter::Off,
                    entry: None,
                },
            );
        }
        assert_eq!(deferred.len(), super::DEFERRED_COMMAND_LIMIT, "bounded");
        defer_command(&mut deferred, PlayerCommand::ResetQueue);
        assert!(matches!(deferred.last(), Some(PlayerCommand::ResetQueue)));
    }

    #[test]
    fn ticks_keep_their_cadence_under_a_stream_of_commands() {
        let start = std::time::Instant::now();
        let mut clock = TickClock::new(start);
        // A volume drag: a command every 50 ms for two seconds. The loop
        // waits until the next command or the wake-up moment, whichever
        // comes first.
        let command_every = Duration::from_millis(50);
        let mut now = start;
        let mut next_command = start + command_every;
        let mut ticks = Vec::new();
        while now < start + Duration::from_secs(2) {
            let wake_at = clock.wake_at(now, TICK);
            now = next_command.min(wake_at);
            if now == next_command {
                next_command += command_every;
            }
            if clock.tick_due(now, wake_at) {
                ticks.push(now - start);
            }
        }
        assert_eq!(ticks.len(), 5, "{ticks:?}");
        assert!(ticks.windows(2).all(|pair| pair[1] - pair[0] == TICK));

        // A planned transition wants it sooner: the next wake-up comes then.
        let early = clock.wake_at(now, Duration::from_millis(20));
        assert_eq!(early, now + Duration::from_millis(20));
        assert!(clock.tick_due(early, early));
    }

    /// A new player starts in the real format of its first source: a stereo
    /// track comes out as stereo from its very first sample — not folded to
    /// mono and stretched, as with the placeholder an empty player reports.
    #[test]
    fn a_new_player_plays_its_first_samples_in_their_own_format() {
        let rate = rodio::SampleRate::new(48_000).unwrap();
        let stereo = rodio::ChannelCount::new(2).unwrap();
        let (mixer, mixed) = rodio::mixer::mixer(stereo, rate);
        let mix = Mix {
            mixer,
            channels: stereo,
            rate,
        };
        // Left and right apart, so a channel mix-up shows.
        let frames: Vec<f32> = (0..2_000).flat_map(|_| [0.5, -0.5]).collect();
        let _player = start_player(&mix, 1.0, false, |player| {
            player.append(SamplesBuffer::new(stereo, rate, frames))
        });
        let out: Vec<f32> = mixed.take(2_000).collect();
        assert_eq!(out.len(), 2_000);
        assert!(
            out.chunks(2).all(|frame| frame == [0.5, -0.5]),
            "first samples converted in the wrong format: {:?}",
            &out[..16]
        );
    }

    #[test]
    fn a_player_started_paused_is_silent_from_its_first_sample() {
        let rate = rodio::SampleRate::new(48_000).unwrap();
        let mono = rodio::ChannelCount::new(1).unwrap();
        let (mixer, mixed) = rodio::mixer::mixer(mono, rate);
        let mix = Mix {
            mixer,
            channels: mono,
            rate,
        };
        let _player = start_player(&mix, 1.0, true, |player| {
            player.append(SamplesBuffer::new(mono, rate, vec![0.5; 4_000]))
        });
        assert!(mixed.take(2_000).all(|sample| sample == 0.0));
    }

    #[test]
    fn the_sleep_fade_runs_down_to_silence_over_its_length() {
        let fade = Duration::from_secs(30);
        assert_eq!(sleep_level(Duration::from_secs(31), fade), None, "not yet");
        assert_eq!(sleep_level(fade, fade), Some(1.0));
        assert_eq!(sleep_level(Duration::from_secs(15), fade), Some(0.5));
        assert_eq!(sleep_level(Duration::ZERO, fade), Some(0.0));
    }

    #[test]
    fn a_resume_hands_over_to_the_sleep_fade_once_its_ramp_is_done() {
        let fade = Some((0.3, Duration::from_secs(9)));
        let ramping = Some(Duration::from_millis(120));
        let done = Some(Duration::ZERO);
        let fall = SleepFadeStep::Fall {
            over: Duration::from_secs(9),
        };
        // Coming back to the fade's level: left alone until that is done,
        // then the fade carries on down from there.
        assert_eq!(sleep_fade_step(true, ramping, fade), SleepFadeStep::Keep);
        assert_eq!(sleep_fade_step(true, done, fade), fall);
        // Re-timed meanwhile (a seek, a timer or speed change took the mark):
        // up to the new level by the same moment, not a swell to full volume.
        assert_eq!(
            sleep_fade_step(false, ramping, fade),
            SleepFadeStep::Rise {
                to: 0.3,
                over: Duration::from_millis(120),
            }
        );
        // No resume: armed stays armed, anything else is armed now.
        assert_eq!(sleep_fade_step(true, None, fade), SleepFadeStep::Keep);
        assert_eq!(sleep_fade_step(false, None, fade), fall);
        // No fade (cancelled, moved out of its window): nothing to do, also
        // during or after a resume.
        for resume in [None, ramping, done] {
            for armed in [false, true] {
                assert_eq!(sleep_fade_step(armed, resume, None), SleepFadeStep::Keep);
            }
        }
    }

    #[test]
    fn only_a_short_stop_with_the_download_unfinished_is_a_broken_stream() {
        let four_minutes = 240_000;
        // Played through, all downloaded.
        assert!(!stream_broke_off(240_000, four_minutes, Some((0.0, 1.0))));
        // The connection died at 40 %: the download stopped there, and so
        // did the music.
        assert!(stream_broke_off(96_000, four_minutes, Some((0.0, 0.45))));
        // Same after a reopen at 2:00, which downloads from there on.
        assert!(stream_broke_off(150_000, four_minutes, Some((0.5, 0.65))));
        // Reopened at 3:55 (a seek, a device switch, Play after an error):
        // the rest of the file came first and played out, while the part
        // before 3:55 is still being filled in. That is a natural end.
        assert!(!stream_broke_off(240_000, four_minutes, Some((0.0, 0.3))));
        // A stated length a little off makes no difference either.
        assert!(!stream_broke_off(237_000, four_minutes, Some((0.0, 0.3))));
        assert!(!stream_broke_off(235_000, four_minutes, Some((0.0, 0.3))));
        assert!(stream_broke_off(234_999, four_minutes, Some((0.0, 0.3))));
        // Stopped short with the whole file on disk: a damaged frame, not
        // the stream.
        assert!(!stream_broke_off(96_000, four_minutes, Some((0.0, 1.0))));
        // A live transcode has no length to tell by; nor has a track
        // without a duration.
        assert!(!stream_broke_off(96_000, four_minutes, None));
        assert!(!stream_broke_off(96_000, 0, Some((0.0, 0.45))));
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
    fn album_shuffle_plays_an_album_queued_twice_as_two_blocks() {
        let track = |id: &str, album: &str, number: i32| {
            let mut t = queue_track(id);
            t.album_id = Some(album.into());
            t.track_number = Some(number);
            t
        };
        // Album x, a single y, then album x again (the same three tracks).
        let tracks = vec![
            track("x1", "x", 1),
            track("x2", "x", 2),
            track("x3", "x", 3),
            track("y1", "y", 1),
            track("x1", "x", 1),
            track("x2", "x", 2),
            track("x3", "x", 3),
        ];
        for seed in 0..32 {
            let order = build_album_shuffle_order(&tracks, 0, seed);
            assert_eq!(&order[..3], &[0, 1, 2], "the first copy, in track order");
            let second = order.iter().position(|&index| index == 4).unwrap();
            assert_eq!(
                &order[second..second + 3],
                &[4, 5, 6],
                "the second copy is a block of its own, not 1,1,2,2,3,3"
            );
            let mut sorted = order;
            sorted.sort_unstable();
            assert_eq!(sorted, (0..tracks.len()).collect::<Vec<_>>());
        }
        // Started on the second copy: that copy plays on from there.
        let order = build_album_shuffle_order(&tracks, 5, 7);
        assert_eq!(&order[..3], &[5, 6, 4]);
    }

    #[test]
    fn the_counts_the_queue_shows_match_what_cleanup_removes() {
        let tracks = ["a", "b", "a", "a", "c", "b"].map(queue_track);
        for current in 0..tracks.len() {
            assert_eq!(
                duplicate_count(&tracks),
                duplicate_indices(&tracks, current).len()
            );
        }
        assert_eq!(played_count(5, 3, &[2, 0, 3, 1, 4]), 2);
        assert_eq!(played_count(5, 2, &[]), 2);
        assert_eq!(played_count(0, 0, &[]), 0);
    }

    #[test]
    fn a_seed_this_build_does_not_know_reads_as_the_track_seed() {
        let mode: AutoDjSeedMode = serde_json::from_str(r#""mood""#).unwrap();
        assert_eq!(mode, AutoDjSeedMode::Track);
        let mode: AutoDjSeedMode = serde_json::from_str(r#""artist""#).unwrap();
        assert_eq!(mode, AutoDjSeedMode::Artist);
        assert_eq!(
            serde_json::to_string(&AutoDjSeedMode::Track).unwrap(),
            r#""track""#
        );
    }

    /// The seed list as 0.6.0 knows it, without the sound seed and without
    /// any stand-in for unknown values.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum OlderSeed {
        Track,
        Artist,
        Genre,
        Album,
        Playlist,
    }

    #[test]
    fn a_sound_reason_in_the_queue_still_reads_in_older_builds() {
        #[derive(serde::Deserialize)]
        struct OlderReason {
            kind: OlderSeed,
            label: String,
        }
        let reason = AutoDjReason {
            kind: AutoDjSeedMode::Sonic,
            label: "Song".into(),
        };
        let json = serde_json::to_string(&reason).unwrap();
        let older: OlderReason = serde_json::from_str(&json).unwrap();
        assert_eq!(
            (older.kind, older.label.as_str()),
            (OlderSeed::Track, "Song")
        );
        // Read back here, it is the sound seed again.
        assert_eq!(serde_json::from_str::<AutoDjReason>(&json).unwrap(), reason);
        // As builds before this one wrote it.
        let dev: AutoDjReason = serde_json::from_str(r#"{"kind":"sonic","label":"Song"}"#).unwrap();
        assert_eq!(dev.kind, AutoDjSeedMode::Sonic);
        // Any other seed is written as before, without the flag.
        let artist = AutoDjReason {
            kind: AutoDjSeedMode::Artist,
            label: "A".into(),
        };
        assert_eq!(
            serde_json::to_string(&artist).unwrap(),
            r#"{"kind":"artist","label":"A"}"#
        );
    }

    #[test]
    fn stored_extras_with_the_sound_seed_still_read_in_older_builds() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OlderExtras {
            auto_dj_enabled: bool,
            auto_dj_seed_mode: OlderSeed,
            auto_dj_max_tracks: u32,
        }
        let settings = ExtrasSettings {
            auto_dj_enabled: true,
            auto_dj_seed_mode: AutoDjSeedMode::Sonic,
            auto_dj_max_tracks: 40,
            ..ExtrasSettings::default()
        };
        let stored = settings.to_stored_json().unwrap();
        let older: OlderExtras = serde_json::from_str(&stored).unwrap();
        assert!(older.auto_dj_enabled);
        assert_eq!(older.auto_dj_seed_mode, OlderSeed::Track);
        assert_eq!(older.auto_dj_max_tracks, 40);
        assert_eq!(ExtrasSettings::from_stored_json(&stored), Some(settings));
        // A blob from before: "sonic" spelled out, or a plain seed.
        let dev = ExtrasSettings::from_stored_json(r#"{"autoDjSeedMode":"sonic"}"#).unwrap();
        assert_eq!(dev.auto_dj_seed_mode, AutoDjSeedMode::Sonic);
        let plain = ExtrasSettings::from_stored_json(r#"{"autoDjSeedMode":"genre"}"#).unwrap();
        assert_eq!(plain.auto_dj_seed_mode, AutoDjSeedMode::Genre);
        // The UI still gets the seed as it is.
        let ui = serde_json::to_value(ExtrasSettings {
            auto_dj_seed_mode: AutoDjSeedMode::Sonic,
            ..ExtrasSettings::default()
        })
        .unwrap();
        assert_eq!(ui["autoDjSeedMode"], "sonic");
    }

    /// A handle on a plain channel, without a player thread behind it.
    fn bare_handle() -> (
        super::PlayerHandle,
        std::sync::mpsc::Receiver<super::PlayerCommand>,
    ) {
        use std::sync::{Arc, Mutex};
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = super::PlayerHandle {
            tx,
            state: Default::default(),
            queue: Arc::new(Mutex::new(super::QueueSnapshot {
                revision: 0,
                tracks: Vec::new(),
                index: 0,
                order: Vec::new(),
                can_undo: false,
                played_count: 0,
                duplicate_count: 0,
            })),
            dsp: Arc::new(super::AudioDsp::new(Default::default())),
            tap: Arc::new(super::tap::VisualizerTap::new()),
            waveforms: Default::default(),
            analyses: Arc::new(super::analysis::AnalysisCache::new(None)),
            play_requests: Default::default(),
            enqueue_turn: Default::default(),
        };
        (handle, rx)
    }

    /// What reached the player, in order, by the first track of each.
    fn sent(rx: &std::sync::mpsc::Receiver<super::PlayerCommand>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|cmd| match cmd {
                super::PlayerCommand::PlayQueue {
                    tracks, in_order, ..
                } => Some(format!(
                    "{} {}",
                    if in_order { "in order" } else { "play" },
                    tracks[0].item_id
                )),
                super::PlayerCommand::PlayNext(tracks) => {
                    Some(format!("next {}", tracks[0].item_id))
                }
                super::PlayerCommand::PlayLast(tracks) => {
                    Some(format!("last {}", tracks[0].item_id))
                }
                _ => None,
            })
            .collect()
    }

    /// Let the spawned requests run as far as they get.
    async fn let_run() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_play_request_overtaken_by_a_newer_one_replaces_nothing() {
        let (player, rx) = bare_handle();
        // A slow instant mix, then an album clicked while it is fetched.
        let mix = player.play_ticket();
        let album = player.play_ticket();
        player
            .play_queue(album, vec![queue_track("album")], 0)
            .await
            .unwrap();
        // The mix comes back last: overtaken, it must not replace the album.
        player
            .play_queue(mix, vec![queue_track("mix")], 0)
            .await
            .unwrap();
        assert_eq!(sent(&rx), ["play album"]);
        // A sonic radio goes out to be played in its own order.
        let radio = player.play_ticket();
        player
            .play_in_order(radio, vec![queue_track("seed")])
            .await
            .unwrap();
        assert_eq!(sent(&rx), ["in order seed"]);
    }

    #[tokio::test]
    async fn a_refused_request_gives_way_to_an_older_one_still_fetching() {
        let (player, rx) = bare_handle();
        // A big genre is shuffled, then a sonic radio is refused (too few
        // tracks analysed): the genre still plays when it is fetched.
        let genre = player.play_ticket();
        let radio = player.play_ticket();
        drop(radio);
        player
            .play_queue(genre, vec![queue_track("genre")], 0)
            .await
            .unwrap();
        assert_eq!(sent(&rx), ["play genre"]);
    }

    #[tokio::test]
    async fn an_older_request_waits_for_a_newer_one_still_fetching() {
        let (player, rx) = bare_handle();
        let album = player.play_ticket();
        let radio = player.play_ticket();
        // The album is back first, while the radio still loads its index.
        let waiting = tokio::spawn({
            let player = player.clone();
            async move {
                player
                    .play_queue(album, vec![queue_track("album")], 0)
                    .await
            }
        });
        let_run().await;
        assert!(sent(&rx).is_empty(), "the radio may still play");
        // The radio is refused: the album plays after all.
        drop(radio);
        waiting.await.unwrap().unwrap();
        assert_eq!(sent(&rx), ["play album"]);

        // This time the newer one plays, and the older one is dropped.
        let album = player.play_ticket();
        let mix = player.play_ticket();
        let waiting = tokio::spawn({
            let player = player.clone();
            async move {
                player
                    .play_queue(album, vec![queue_track("album")], 0)
                    .await
            }
        });
        let_run().await;
        player
            .play_queue(mix, vec![queue_track("mix")], 0)
            .await
            .unwrap();
        waiting.await.unwrap().unwrap();
        assert_eq!(sent(&rx), ["play mix"]);
    }

    #[tokio::test]
    async fn an_addition_lands_behind_the_play_asked_for_before_it() {
        let (player, rx) = bare_handle();
        // A big genre is shuffled, then an album added while it loads.
        let genre = player.play_ticket();
        let turn = player.enqueue_turn().await;
        let adding = tokio::spawn({
            let player = player.clone();
            async move { player.enqueue(turn, vec![queue_track("b")], false).await }
        });
        let_run().await;
        assert!(sent(&rx).is_empty(), "the album waits for the genre");
        player
            .play_queue(genre, vec![queue_track("genre")], 0)
            .await
            .unwrap();
        adding.await.unwrap().unwrap();
        assert_eq!(sent(&rx), ["play genre", "last b"]);

        // A play that fails lets the addition go to the queue there is.
        let failed = player.play_ticket();
        let turn = player.enqueue_turn().await;
        let adding = tokio::spawn({
            let player = player.clone();
            async move { player.enqueue(turn, vec![queue_track("c")], true).await }
        });
        let_run().await;
        drop(failed);
        adding.await.unwrap().unwrap();
        assert_eq!(sent(&rx), ["next c"]);

        // A play asked for after the addition does not hold it up.
        let turn = player.enqueue_turn().await;
        let _later = player.play_ticket();
        player
            .enqueue(turn, vec![queue_track("d")], false)
            .await
            .unwrap();
        assert_eq!(sent(&rx), ["last d"]);
    }

    #[test]
    fn an_empty_auto_dj_answer_waits_before_the_seed_asks_again() {
        let now = std::time::Instant::now();
        let retry = AutoDjRetry {
            seed: "a".into(),
            attempts: 1,
            not_before: now + AUTO_DJ_RETRY_AFTER,
        };
        assert!(retry.holds("a", now), "not on the next tick");
        assert!(!retry.holds("a", now + AUTO_DJ_RETRY_AFTER));
        assert!(!retry.holds("b", now), "another seed asks at once");
        let spent = AutoDjRetry {
            attempts: AUTO_DJ_ATTEMPTS,
            ..retry
        };
        assert!(spent.holds("a", now + AUTO_DJ_RETRY_AFTER * 10));
    }

    #[test]
    fn an_auto_dj_mix_leaves_out_what_was_heard_by_item_and_by_recording() {
        let dto = |id: &str, artist: &str, name: &str| crate::api::types::TrackDto {
            id: id.into(),
            artist: artist.into(),
            name: name.into(),
            duration_ms: 200_000,
            ..Default::default()
        };
        let existing = std::collections::HashSet::from(["queued".to_string()]);
        let mut heard = crate::commands_sonic::Recordings::default();
        heard.insert("Band", "Hit", 200_500);
        let mix = vec![
            dto("copy", "band", "Hit"),
            dto("queued", "Other", "Queued"),
            dto("n1", "New", "One"),
            dto("n2", "New", "Two"),
            dto("n3", "New", "Three"),
        ];
        let fresh = fresh_mix(mix, &existing, &heard, 2);
        let ids: Vec<&str> = fresh.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["n1", "n2"]);
    }

    #[test]
    fn prefetch_opens_the_next_track_only_once() {
        const W: u64 = super::PREFETCH_OPEN_REMAINING_MS;
        assert!(should_start_prefetch(
            false, None, None, "b", 200_000, 15_000, W
        ));
        assert!(
            !should_start_prefetch(false, None, None, "b", 200_000, 60_000, W),
            "too early"
        );
        assert!(
            should_start_prefetch(false, None, None, "b", 0, 0, W),
            "unknown length opens at once"
        );
        assert!(
            !should_start_prefetch(true, None, None, "b", 200_000, 15_000, W),
            "already open or opening"
        );
        // The gapless hand-off took the prefetched source: both slots are
        // empty again, but the track already sits in the sink.
        assert!(!should_start_prefetch(
            false,
            Some("b"),
            None,
            "b",
            200_000,
            2_000,
            W
        ));
        // The queue changed after the hand-off: the new next track is opened.
        assert!(should_start_prefetch(
            false,
            Some("b"),
            None,
            "c",
            200_000,
            2_000,
            W
        ));
        // Beat transitions open earlier, for the analysis.
        assert!(should_start_prefetch(
            false,
            None,
            None,
            "b",
            200_000,
            45_000,
            super::PREFETCH_OPEN_BEAT_MS
        ));
    }

    #[test]
    fn a_failed_prefetch_is_not_retried_on_every_tick() {
        const W: u64 = super::PREFETCH_OPEN_REMAINING_MS;
        // Both slots are empty again after the failure; without the memo
        // the next tick would open the same track again.
        assert!(!should_start_prefetch(
            false,
            None,
            Some("b"),
            "b",
            200_000,
            15_000,
            W
        ));
        // Nor for a track of unknown length, which would retry it for the
        // whole of the current one.
        assert!(!should_start_prefetch(false, None, Some("b"), "b", 0, 0, W));
        // A different next track (the queue changed) is opened as usual.
        assert!(should_start_prefetch(
            false,
            None,
            Some("b"),
            "c",
            200_000,
            15_000,
            W
        ));
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
        assert_eq!(restored_index(&old, None), (2, 0));
        assert_eq!(
            restored_index(&old, Some(r#"{"snapshot_id":"a","index":5}"#)),
            (2, 0)
        );

        let json = serde_json::to_string(&PersistedQueue {
            tracks: vec![queue_track("t")],
            index: 2,
            server_url: None,
            user_id: None,
            snapshot_id: Some("a".into()),
            order: None,
        })
        .unwrap();
        let current: PersistedQueue = serde_json::from_str(&json).unwrap();
        assert_eq!(restored_index(&current, None), (2, 0));
        assert_eq!(
            restored_index(&current, Some(r#"{"snapshot_id":"a","index":5}"#)),
            (5, 0),
            "an entry from before positions were saved"
        );
        assert_eq!(
            restored_index(
                &current,
                Some(r#"{"snapshot_id":"a","index":5,"position_ms":83500}"#)
            ),
            (5, 83_500)
        );
        assert_eq!(
            restored_index(
                &current,
                Some(r#"{"snapshot_id":"b","index":5,"position_ms":83500}"#)
            ),
            (2, 0),
            "left over from an earlier track list"
        );
        assert_eq!(restored_index(&current, Some("not json")), (2, 0));
    }

    #[test]
    fn a_track_change_drops_the_saved_position() {
        // The index entry written on a track change says nothing about a
        // position: the new track starts from the beginning.
        let entry = super::PersistedQueueIndex {
            snapshot_id: "a".into(),
            index: 3,
            position_ms: None,
        };
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"{"snapshot_id":"a","index":3}"#
        );
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
            std::env::temp_dir().join(format!("sonarune-queue-test-{}", uuid::Uuid::new_v4())),
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
            order: Some(PersistedOrder {
                mode: ShuffleMode::Tracks,
                order: vec![2, 0, 1],
            }),
        };
        store
            .set(keys::QUEUE, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        store
            .set(keys::QUEUE_INDEX, r#"{"snapshot_id":"s1","index":2}"#)
            .unwrap();
        let restored = load_persisted_queue(&store).expect("same account");
        assert_eq!((restored.tracks.len(), restored.index), (3, 2));
        assert_eq!(
            restored_order(restored.order, ShuffleMode::Tracks, 3),
            Some(vec![2, 0, 1]),
            "the shuffle order comes back with it"
        );

        store.set(keys::USER_ID, "u2").unwrap();
        assert!(load_persisted_queue(&store).is_none(), "another account");
        assert!(store.get(keys::QUEUE).is_none());
        assert!(store.get(keys::QUEUE_INDEX).is_none());
    }

    #[test]
    fn a_listen_after_the_server_ended_the_session_still_has_its_account() {
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = TempDir(
            std::env::temp_dir().join(format!("sonarune-listener-{}", uuid::Uuid::new_v4())),
        );
        let store = Store::open(&dir.0).unwrap();
        store.set(keys::SERVER_URL, "https://music.home").unwrap();
        store.set(keys::USER_ID, "u1").unwrap();
        store.record_server("https://music.home", "s1").unwrap();
        // The session's own account, on the session's server.
        let mut client = JellyfinClient::new("https://other.home", "test", Vec::new())
            .unwrap()
            .with_session("token".into(), "u2".into());
        client.set_server_id(Some("s2".into()));
        let owner = listener(Some(&client), &store).unwrap();
        assert_eq!(
            (owner.user.as_str(), owner.server.id.as_deref()),
            ("u2", Some("s2"))
        );
        // Ended by the server: the track plays on, and the account the
        // store names is still the one listening.
        let owner = listener(None, &store).unwrap();
        assert_eq!(
            (owner.user.as_str(), owner.server.id.as_deref()),
            ("u1", Some("s1"))
        );
        // Signed out: nobody's.
        store.delete(keys::USER_ID).unwrap();
        assert!(listener(None, &store).is_none());
    }

    #[test]
    fn a_shuffle_order_is_restored_only_where_it_still_fits() {
        let saved = |mode, order: &[usize]| {
            Some(PersistedOrder {
                mode,
                order: order.to_vec(),
            })
        };
        assert_eq!(
            restored_order(
                saved(ShuffleMode::Albums, &[1, 2, 0]),
                ShuffleMode::Albums,
                3
            ),
            Some(vec![1, 2, 0])
        );
        // Built for track shuffle; album shuffle is on now.
        assert_eq!(
            restored_order(
                saved(ShuffleMode::Tracks, &[1, 2, 0]),
                ShuffleMode::Albums,
                3
            ),
            None
        );
        // Not a permutation of the queue it came with.
        for broken in [&[1, 2][..], &[1, 2, 2], &[0, 1, 3], &[0, 1, 2, 3]] {
            assert_eq!(
                restored_order(saved(ShuffleMode::Tracks, broken), ShuffleMode::Tracks, 3),
                None,
                "{broken:?}"
            );
        }
        assert_eq!(restored_order(None, ShuffleMode::Tracks, 3), None);
        assert_eq!(
            restored_order(saved(ShuffleMode::Off, &[0, 1, 2]), ShuffleMode::Off, 3),
            None
        );
    }

    #[test]
    fn a_snapshot_with_its_order_still_reads_in_older_builds() {
        // 0.6.0's snapshot had no order; it ignores the field it does not know.
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct OlderSnapshot {
            tracks: Vec<QueueTrack>,
            index: usize,
        }
        let json = serde_json::to_string(&PersistedQueue {
            tracks: vec![queue_track("a"), queue_track("b")],
            index: 1,
            server_url: None,
            user_id: None,
            snapshot_id: None,
            order: Some(PersistedOrder {
                mode: ShuffleMode::Tracks,
                order: vec![1, 0],
            }),
        })
        .unwrap();
        let older: OlderSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!((older.tracks.len(), older.index), (2, 1));
        // And a snapshot an older build wrote has no order: shuffled anew.
        let without: PersistedQueue = serde_json::from_str(r#"{"tracks":[],"index":0}"#).unwrap();
        assert!(without.order.is_none());
    }

    #[test]
    fn an_order_this_build_cannot_read_costs_the_order_not_the_queue() {
        let tracks = serde_json::to_value(vec![queue_track("a"), queue_track("b")]).unwrap();
        // A later build's shuffle mode, and an order that is no order at all.
        for order in [
            serde_json::json!({ "mode": "artists", "order": [1, 0] }),
            serde_json::json!({ "mode": "tracks", "order": "1,0" }),
            serde_json::json!(null),
        ] {
            let json = serde_json::json!({ "tracks": tracks, "index": 1, "order": order });
            let snapshot: PersistedQueue = serde_json::from_value(json).unwrap();
            assert_eq!((snapshot.tracks.len(), snapshot.index), (2, 1), "{order}");
            assert!(snapshot.order.is_none(), "{order}");
        }
        // An order it can read still comes back.
        let json = serde_json::json!({
            "tracks": tracks, "index": 0, "order": { "mode": "albums", "order": [1, 0] },
        });
        let snapshot: PersistedQueue = serde_json::from_value(json).unwrap();
        assert_eq!(
            snapshot.order,
            Some(PersistedOrder {
                mode: ShuffleMode::Albums,
                order: vec![1, 0]
            })
        );
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
