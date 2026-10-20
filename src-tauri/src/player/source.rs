//! Opening a queue track as a playable, DSP-wrapped rodio source.
//! Runs on short-lived background threads (never on the player thread) so a
//! slow or stalled server can never freeze the player command loop.

mod mp3;
mod stream;

use super::dsp::{self, AudioDsp, DspSource};
use super::fade::{self, FadeHandle, FadeSource};
use super::opus::{OggOpusSource, OpusProbeError};
use super::tap::{self, TapSource, VisualizerTap};
use super::waveform::{WaveformJob, WaveformRequest};
use super::QueueTrack;
use crate::api::JellyfinClient;
use crate::error::{AppError, AppResult};
use rodio::Source;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::Duration;
use tokio_util::sync::{CancellationToken, DropGuard};

/// Cap for streams that never announce a length (live transcodes). Such a
/// stream is treated as unseekable anyway, so a circular buffer costs nothing
/// — and it stops a server that just keeps sending from filling the disk.
/// 64 MiB is ~27 minutes at 320 kbit/s, far more than the read-ahead needs.
const UNBOUNDED_STREAM_CAP: usize = 64 * 1024 * 1024;

/// Adaptive: an unknown-length stream gets a bounded circular buffer (the
/// writer pauses when the reader falls behind), while a stream that announces
/// its size keeps a plain, fully seekable temp file.
type Storage =
    stream_download::storage::adaptive::AdaptiveStorageProvider<StreamFiles, StreamFiles>;

/// What the decoders read: the download, see `source/stream.rs`.
pub type Reader = stream::StreamReader;

/// Name prefix of every stream copy, so a leftover is ours beyond doubt.
const STREAM_FILE_PREFIX: &str = "sonarune-";

/// Where stream copies live: a folder of our own in the temp directory. Each
/// is a whole track (tens of MB of FLAC, hundreds for hi-res) and is deleted
/// when its source is dropped — which never happens when the process ends any
/// other way than a clean quit: a Windows shutdown with the app in the tray, a
/// crash, the installer replacing it. So the first open of a run empties the
/// folder; only one instance runs at a time (the single-instance plugin), so
/// whatever is in there belongs to a run that is over. `None` when the folder
/// cannot be made or written to (a shared `/tmp` where another user made it
/// first): the copies then go to the temp directory itself.
fn stream_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("sonarune-streams");
        let swept = std::fs::create_dir_all(&dir).map(|()| sweep_stream_dir(&dir));
        let usable = swept.and_then(|swept| {
            stream_download::storage::temp::tempfile::Builder::new()
                .prefix(STREAM_FILE_PREFIX)
                .tempfile_in(&dir)
                .map(|_| swept)
        });
        match usable {
            Ok(swept) => {
                if swept > 0 {
                    tracing::info!("removed {swept} stream copies an earlier run left behind");
                }
                Some(dir)
            }
            Err(e) => {
                tracing::warn!("no folder of our own for stream copies: {e}");
                None
            }
        }
    })
    .as_deref()
}

/// Delete the stream copies in `dir`; returns how many went.
fn sweep_stream_dir(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(STREAM_FILE_PREFIX)
        })
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
}

/// A further handle on a stream's temp file, put in when the file is made:
/// the download task keeps its own handles private.
type FileSlot = Arc<Mutex<Option<std::fs::File>>>;

/// The temp-file storage of one stream, and the slot that gets the reader's
/// own handle on it (see `stream::StreamReader`). One provider fills both
/// slots of stream-download's adaptive storage, which keeps a stream that
/// announces its length in the first slot up to [`UNBOUNDED_STREAM_CAP`] and
/// in the second above it, and gives a stream without one (a live transcode)
/// the first slot as a bounded ring. With `wave_file`, the waveform job gets
/// a handle too.
fn storage(wave_file: Option<FileSlot>) -> (Storage, FileSlot) {
    let reading = FileSlot::default();
    let files = StreamFiles {
        dir: stream_dir(),
        wave_file,
        reading: reading.clone(),
    };
    let storage = stream_download::storage::adaptive::AdaptiveStorageProvider::new(
        files,
        std::num::NonZeroUsize::new(UNBOUNDED_STREAM_CAP)
            .expect("UNBOUNDED_STREAM_CAP is a non-zero constant"),
    );
    (storage, reading)
}

/// stream-download's temp-file storage, handing out more handles on the
/// file as it is made.
#[derive(Clone)]
struct StreamFiles {
    dir: Option<&'static Path>,
    wave_file: Option<FileSlot>,
    reading: FileSlot,
}

impl stream_download::storage::StorageProvider for StreamFiles {
    type Reader = stream_download::storage::temp::tempfile::NamedTempFile;
    type Writer = std::fs::File;

    fn into_reader_writer(
        self,
        _content_length: Option<u64>,
    ) -> std::io::Result<(Self::Reader, Self::Writer)> {
        let file = stream_file(self.dir)?;
        if let Some(slot) = &self.wave_file {
            if let Ok(second) = file.reopen() {
                *slot.lock().unwrap() = Some(second);
            }
        }
        *self.reading.lock().unwrap() = Some(file.reopen()?);
        let writer = file.reopen()?;
        Ok((file, writer))
    }
}

/// A new stream copy, in `dir` when there is one. A temp-folder cleanup may
/// remove the (empty) folder while the app runs, so it is made again then;
/// failing that, the copy goes to the temp directory itself.
fn stream_file(
    dir: Option<&Path>,
) -> std::io::Result<stream_download::storage::temp::tempfile::NamedTempFile> {
    let mut builder = stream_download::storage::temp::tempfile::Builder::new();
    builder.prefix(STREAM_FILE_PREFIX);
    if let Some(dir) = dir {
        let file = builder.tempfile_in(dir).or_else(|_| {
            std::fs::create_dir_all(dir)?;
            builder.tempfile_in(dir)
        });
        if file.is_ok() {
            return file;
        }
    }
    builder.tempfile()
}

/// Follow redirects only inside the server's own origin. Jellyfin behind a
/// reverse proxy may redirect within itself; a redirect that leaves the origin
/// would turn a play click into a request to an arbitrary host (SSRF) or
/// silently downgrade the stream to plain http.
pub fn same_origin_redirects(base_url: String) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if same_origin(attempt.url().as_str(), &base_url) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Why a background open was started: to play right now, to have the next
/// track ready for the gapless hand-off, or to reopen the current track at a
/// position (a seek on a live transcode, a switch to another output device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPurpose {
    Play,
    Prefetch,
    SeekRestart { position_ms: u64 },
}

/// Everything a background open needs from the session (snapshotted so the
/// open thread never touches the session lock).
#[derive(Clone)]
pub struct StreamAuth {
    pub auth_header: String,
    pub trusted: Vec<String>,
    /// Origin of the session this header belongs to. A queue entry carries an
    /// absolute `stream_url` built when it was queued, so without this check a
    /// queue left over from another server would receive the current server's
    /// token.
    pub base_url: String,
}

impl StreamAuth {
    pub fn from_client(client: &JellyfinClient) -> Self {
        Self {
            auth_header: client.auth_header(),
            trusted: client.trusted_fingerprints().to_vec(),
            base_url: client.base_url().to_string(),
        }
    }
}

/// True when both URLs share scheme, host and port. Compared on parsed URLs
/// rather than as strings so `https://h:443/x` and `https://h/x` agree and no
/// prefix trick (`https://evil.test/?x=https://real.test`) slips through.
fn same_origin(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a), reqwest::Url::parse(b)) {
        (Ok(a), Ok(b)) => {
            a.scheme() == b.scheme()
                && a.host_str() == b.host_str()
                && a.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    }
}

/// Symphonia handles flac/mp3/ogg-vorbis/wav; Ogg-Opus (Jellyfin's transcode
/// target and .opus libraries) goes through our libopus-backed decoder.
/// The Opus variant is boxed: its inline state dwarfs the Symphonia handle.
pub enum EitherSource {
    /// The flag mirrors what `OggOpusSource` tracks internally: whether the
    /// byte stream has a known end and real random access. rodio runs
    /// `try_seek` on the audio callback thread, so seeking a live transcode
    /// would block on not-yet-downloaded ranges and stall all audio.
    Symphonia(rodio::Decoder<Reader>, bool),
    Opus(Box<OggOpusSource<Reader>>),
}

impl Iterator for EitherSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match self {
            EitherSource::Symphonia(s, _) => s.next(),
            EitherSource::Opus(s) => s.next(),
        }
    }
}

impl Source for EitherSource {
    fn current_span_len(&self) -> Option<usize> {
        match self {
            EitherSource::Symphonia(s, _) => s.current_span_len(),
            EitherSource::Opus(s) => s.current_span_len(),
        }
    }

    fn channels(&self) -> rodio::ChannelCount {
        match self {
            EitherSource::Symphonia(s, _) => s.channels(),
            EitherSource::Opus(s) => s.channels(),
        }
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        match self {
            EitherSource::Symphonia(s, _) => s.sample_rate(),
            EitherSource::Opus(s) => s.sample_rate(),
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        match self {
            EitherSource::Symphonia(s, _) => s.total_duration(),
            EitherSource::Opus(s) => s.total_duration(),
        }
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        match self {
            EitherSource::Symphonia(_, false) => Err(rodio::source::SeekError::NotSupported {
                underlying_source: "unseekable stream (live transcode)",
            }),
            EitherSource::Symphonia(s, _) => s.try_seek(pos),
            EitherSource::Opus(s) => s.try_seek(pos),
        }
    }
}

pub type TrackSource = Reaped<TapSource<FadeSource<DspSource<EitherSource>>>>;

/// Reaper slots: two or three sources go per track change at most, and the
/// reaper empties them several times a second.
const REAP_SLOT_COUNT: usize = 16;
const REAP_POLL: Duration = Duration::from_millis(250);
const REAP_FREE: u8 = 0;
/// One thread owns the slot (filling or emptying it).
const REAP_CLAIMED: u8 = 1;
const REAP_FULL: u8 = 2;

/// Where finished sources go to be dropped. Dropping one deletes its temp file
/// and frees its decoder, and rodio drops a source that ended or was stopped
/// inside the audio callback. The hand-over works like the visualizer tap's
/// chunk pool (`tap.rs`): one compare-and-swap claims a slot, its mutex is
/// only ever `try_lock`ed by the claimant (so it never has a waiter to wake),
/// and the reaper polls instead of being woken.
struct ReapSlot {
    state: AtomicU8,
    item: Mutex<Option<Box<dyn Send>>>,
}

static REAP_SLOTS: [ReapSlot; REAP_SLOT_COUNT] = [const {
    ReapSlot {
        state: AtomicU8::new(REAP_FREE),
        item: Mutex::new(None),
    }
}; REAP_SLOT_COUNT];

/// Whether the reaper thread runs. Started by the first source, on its open
/// thread — the audio callback only ever reads this.
static REAPER: OnceLock<bool> = OnceLock::new();

fn start_reaper() {
    REAPER.get_or_init(|| {
        std::thread::Builder::new()
            .name("sonarune-reaper".into())
            .spawn(|| loop {
                std::thread::sleep(REAP_POLL);
                for slot in &REAP_SLOTS {
                    drop(slot.take());
                }
            })
            .is_ok()
    });
}

fn try_lock_reap<T>(item: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match item.try_lock() {
        Ok(guard) => Some(guard),
        // Nothing panics while holding a slot; the value is fine either way.
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl ReapSlot {
    fn put(&self, item: Box<dyn Send>) -> Result<(), Box<dyn Send>> {
        if self
            .state
            .compare_exchange(REAP_FREE, REAP_CLAIMED, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return Err(item);
        }
        let Some(mut held) = try_lock_reap(&self.item) else {
            self.state.store(REAP_FREE, Ordering::Release);
            return Err(item);
        };
        *held = Some(item);
        drop(held);
        self.state.store(REAP_FULL, Ordering::Release);
        Ok(())
    }

    fn take(&self) -> Option<Box<dyn Send>> {
        if self
            .state
            .compare_exchange(REAP_FULL, REAP_CLAIMED, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let Some(mut held) = try_lock_reap(&self.item) else {
            self.state.store(REAP_FULL, Ordering::Release);
            return None;
        };
        let item = held.take();
        drop(held);
        self.state.store(REAP_FREE, Ordering::Release);
        item
    }
}

/// Hand `item` to the reaper. Gives it back when the reaper is not running or
/// every slot is taken; the caller then drops it itself.
fn reap(mut item: Box<dyn Send>) -> Option<Box<dyn Send>> {
    if REAPER.get() != Some(&true) {
        return Some(item);
    }
    for slot in &REAP_SLOTS {
        match slot.put(item) {
            Ok(()) => return None,
            Err(back) => item = back,
        }
    }
    Some(item)
}

/// A source that is dropped by the reaper (see [`ReapSlot`]) rather than
/// wherever it is let go of.
pub struct Reaped<S: Send + 'static>(Option<Box<S>>);

impl<S: Send + 'static> Reaped<S> {
    fn new(source: S) -> Self {
        start_reaper();
        Self(Some(Box::new(source)))
    }

    fn inner(&self) -> &S {
        self.0
            .as_deref()
            .expect("a reaped source is only empty while it is dropped")
    }
}

impl<S: Send + 'static> Drop for Reaped<S> {
    fn drop(&mut self) {
        if let Some(source) = self.0.take() {
            // Handed back when there is no room: dropped here after all.
            drop(reap(source));
        }
    }
}

impl<S: Iterator<Item = f32> + Send + 'static> Iterator for Reaped<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        self.0.as_mut()?.next()
    }
}

impl<S: Source + Send + 'static> Source for Reaped<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner().current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner().channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner().sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner().total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        match self.0.as_mut() {
            Some(source) => source.try_seek(pos),
            None => Err(rodio::source::SeekError::NotSupported {
                underlying_source: "a source being dropped",
            }),
        }
    }
}

/// How much of a stream is downloaded — the seek bar's buffered range, and
/// exactly what is on disk — and how the download ended. Written by the
/// download task (its progress callback, and the stream when it gives up),
/// read by the player worker on its tick, the head-start wait and the reader.
/// The two ends of the buffered range are separate atomics; a read that lands
/// between two updates can pair ends of neighboring ranges, which
/// [`Self::fraction`] tolerates (it is a display hint, never a seek bound).
#[derive(Debug, Default)]
struct DownloadState {
    /// Contiguous downloaded byte range around the download head. After a
    /// seek past the downloaded part, this is the range the new request
    /// fills — not everything from the start.
    start: AtomicU64,
    end: AtomicU64,
    /// Stream length in bytes; 0 = unknown (live transcode).
    total: AtomicU64,
    /// The download is done, all of it.
    finished: AtomicBool,
    /// It ended without the rest: given up after an outage, refused by the
    /// server, or a live transcode cut off.
    broken: AtomicBool,
    /// Every range on disk, merged and in order. From the progress reports,
    /// so never ahead of stream-download's own record; what the reader goes
    /// by (`stream::StreamReader`).
    on_disk: Mutex<Vec<std::ops::Range<u64>>>,
    /// Where the download writes next, as last reported.
    head: AtomicU64,
    /// Wakes a reader waiting in [`Self::on_disk_from`] (with `on_disk`
    /// held) whenever anything here changes.
    arrived: std::sync::Condvar,
    /// Wakes the head-start wait the same way.
    changed: tokio::sync::Notify,
}

impl DownloadState {
    /// One of stream-download's progress reports on a stream of `total`
    /// bytes. True when it says the whole stream is on disk, gap-free.
    fn report(&self, progress: stream_download::StreamState, total: Option<u64>) -> bool {
        use stream_download::StreamPhase;
        // A prefetch reports `0..position` whatever it fetched. With it off
        // that is the lone first byte after a seek (see the stream's
        // `poll_next`), which the next report covers properly.
        if matches!(progress.phase, StreamPhase::Prefetching { .. }) {
            return false;
        }
        let complete = matches!(progress.phase, StreamPhase::Complete);
        // Finished AND gap-free: a seek past the downloaded part starts a new
        // range, and a file with holes must not be decoded as if it were
        // whole.
        let whole = complete
            && progress.current_chunk.start == 0
            && Some(progress.current_chunk.end) == total;
        self.head
            .store(progress.current_position, Ordering::Relaxed);
        self.record(progress.current_chunk, total);
        if complete {
            self.finish();
        }
        whole
    }

    fn record(&self, chunk: std::ops::Range<u64>, total: Option<u64>) {
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
        self.start.store(chunk.start, Ordering::Relaxed);
        self.end.store(chunk.end, Ordering::Relaxed);
        merge_range(&mut self.on_disk.lock().unwrap(), chunk);
        self.arrived.notify_all();
        self.changed.notify_one();
    }

    fn finish(&self) {
        self.finished.store(true, Ordering::Relaxed);
        self.wake();
    }

    fn broke_off(&self) {
        self.broken.store(true, Ordering::Relaxed);
        self.wake();
    }

    /// Something changed that a waiting reader has to look at. The lock makes
    /// sure the news cannot slip in between its look and its wait.
    fn wake(&self) {
        drop(self.on_disk.lock().unwrap());
        self.arrived.notify_all();
        self.changed.notify_one();
    }

    fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    /// Where the next range on disk after `pos` starts, if one does.
    fn on_disk_after(&self, pos: u64) -> Option<u64> {
        let ranges = self.on_disk.lock().unwrap();
        let at = ranges.partition_point(|range| range.start <= pos);
        ranges.get(at).map(|range| range.start)
    }

    /// How many bytes from `pos` are on disk, up to `want`. With `patience`,
    /// waits that long for news when there are none yet.
    fn on_disk_from(&self, pos: u64, want: u64, patience: Option<Duration>) -> u64 {
        let mut ranges = self.on_disk.lock().unwrap();
        let mut on_disk = disk_run(&ranges, pos, want);
        if let (0, Some(patience)) = (on_disk, patience) {
            ranges = self.arrived.wait_timeout(ranges, patience).unwrap().0;
            on_disk = disk_run(&ranges, pos, want);
        }
        on_disk
    }

    /// `bytes` from the start are in (or all of a shorter stream), or the
    /// download is over, one way or the other.
    fn has_head_start(&self, bytes: u64) -> bool {
        if self.finished.load(Ordering::Relaxed) || self.is_broken() {
            return true;
        }
        let total = self.total.load(Ordering::Relaxed);
        let end = self.end.load(Ordering::Relaxed);
        self.start.load(Ordering::Relaxed) == 0 && (end >= bytes || (total > 0 && end >= total))
    }

    /// The downloaded range as fractions of the stream (`0.0..=1.0`), or
    /// `None` while the length is unknown or nothing is downloaded yet.
    pub fn fraction(&self) -> Option<(f64, f64)> {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return None;
        }
        let start = self.start.load(Ordering::Relaxed).min(total);
        let end = self.end.load(Ordering::Relaxed).min(total);
        (end > start).then(|| (start as f64 / total as f64, end as f64 / total as f64))
    }

    /// The whole stream is on disk, gap-free from the first byte — so a native
    /// seek never waits on the network. Always false for a live transcode.
    pub fn is_complete(&self) -> bool {
        let total = self.total.load(Ordering::Relaxed);
        total > 0
            && self.start.load(Ordering::Relaxed) == 0
            && self.end.load(Ordering::Relaxed) >= total
    }
}

/// Add `new` to the ordered, disjoint `ranges`, merging what overlaps or
/// touches it.
fn merge_range(ranges: &mut Vec<std::ops::Range<u64>>, new: std::ops::Range<u64>) {
    if new.is_empty() {
        return;
    }
    let (mut start, mut end) = (new.start, new.end);
    ranges.retain(|range| {
        if range.end < start || range.start > end {
            return true;
        }
        start = start.min(range.start);
        end = end.max(range.end);
        false
    });
    let at = ranges.partition_point(|range| range.start < start);
    ranges.insert(at, start..end);
}

/// How many bytes from `pos` on the `ranges` cover, up to `want`.
fn disk_run(ranges: &[std::ops::Range<u64>], pos: u64, want: u64) -> u64 {
    let at = ranges.partition_point(|range| range.end <= pos);
    ranges
        .get(at)
        .filter(|range| range.start <= pos)
        .map_or(0, |range| (range.end - pos).min(want))
}

/// The player's hold on a source's download, with the life cycle of the
/// source: dropping it stops the download, and that also wakes a decoder
/// waiting for it. Which matters most when that decoder is the audio
/// callback: rodio drops a stopped source only on the callback, so a callback
/// stuck waiting for a stalled server would never drop it.
pub struct DownloadProgress {
    state: Arc<DownloadState>,
    _stop: DropGuard,
}

impl DownloadProgress {
    /// See [`DownloadState::fraction`].
    pub fn fraction(&self) -> Option<(f64, f64)> {
        self.state.fraction()
    }

    /// See [`DownloadState::is_complete`].
    pub fn is_complete(&self) -> bool {
        self.state.is_complete()
    }

    /// The download ended without the rest of the stream. A source that runs
    /// dry then has not reached the end of its track: its stream broke off.
    pub fn broke_off(&self) -> bool {
        self.state.is_broken()
    }
}

/// Opens the player has started and not received yet. An open thread can be
/// stuck waiting for the network, and its result is thrown away once the
/// player has moved on; by then its download is better stopped, which also
/// ends the wait.
#[derive(Default)]
pub struct OpenTracker {
    opens: Vec<TrackedOpen>,
}

struct TrackedOpen {
    generation: u64,
    /// A prefetch's: a queue edit makes it stale too.
    prefetch_generation: Option<u64>,
    stop: CancellationToken,
    delivered: Arc<AtomicBool>,
}

/// An open thread's part of the tracking.
pub struct OpenTicket {
    stop: CancellationToken,
    delivered: Arc<AtomicBool>,
}

impl OpenTracker {
    /// Track an open started under these generations (the prefetch one only
    /// for a prefetch).
    pub fn start(&mut self, generation: u64, prefetch_generation: Option<u64>) -> OpenTicket {
        let stop = CancellationToken::new();
        let delivered = Arc::new(AtomicBool::new(false));
        self.opens.push(TrackedOpen {
            generation,
            prefetch_generation,
            stop: stop.clone(),
            delivered: delivered.clone(),
        });
        OpenTicket { stop, delivered }
    }

    /// Stop every open whose result the player would now throw away — the
    /// rule `source_ready` applies, with its one exception: a prefetch of the
    /// generation `adoptable` names may still play (`handle_track_end`) —
    /// and forget the ones that delivered theirs: the source's own handle
    /// ([`DownloadProgress`]) takes over.
    pub fn stop_stale(
        &mut self,
        generation: u64,
        prefetch_generation: u64,
        adoptable: Option<u64>,
    ) {
        self.opens.retain(|open| {
            if open.delivered.load(Ordering::Acquire) {
                return false;
            }
            let adoptable_prefetch =
                open.prefetch_generation.is_some() && Some(open.generation) == adoptable;
            let stale = !adoptable_prefetch
                && (open.generation != generation
                    || open
                        .prefetch_generation
                        .is_some_and(|wanted| wanted != prefetch_generation));
            if stale {
                open.stop.cancel();
            }
            !stale
        });
    }
}

impl OpenTicket {
    /// What the open's download stops on.
    pub fn stop_token(&self) -> CancellationToken {
        self.stop.clone()
    }

    /// The result is on its way to the player: never stopped as stale from
    /// here on. Must come before the result is sent.
    pub fn delivered(self) {
        self.delivered.store(true, Ordering::Release);
    }
}

/// A ready-to-play source plus the handles steering its gain ramps
/// (crossfades, pause/resume fades), its visualizer feed and its download
/// progress.
pub struct OpenedSource {
    pub source: TrackSource,
    pub fade: FadeHandle,
    pub tap_enabled: Arc<std::sync::atomic::AtomicBool>,
    pub download: Arc<DownloadProgress>,
    /// Media position of the source's first sample: the requested start of a
    /// reopen, or 0 (also when a known-length stream could not be positioned).
    pub start_ms: u64,
}

/// Open the track's HTTP stream with read-ahead buffering and pick a decoder.
/// `start_ms` reopens a transcode at that media position (`startTimeTicks`).
/// With `waveform`, the finished download is also analysed for the seek bar.
/// Blocking; call from a dedicated thread.
/// Drop a `playSessionId` the URL already carries. A queue persisted before
/// the session moved from the queue line to the playback attempt has one baked
/// into every entry's URL; appending a second would leave the server to pick
/// between them, and the one it picked might be the stale one.
fn strip_play_session(url: &str) -> String {
    let Some(start) = url
        .find("?playSessionId=")
        .or_else(|| url.find("&playSessionId="))
        .map(|i| i + 1)
    else {
        return url.to_string();
    };
    // Up to and including the separator that ends the parameter, so the one in
    // front of it stays the separator of what follows.
    let end = url[start..].find('&').map_or(url.len(), |i| start + i + 1);
    let mut out = String::with_capacity(url.len());
    out.push_str(&url[..start]);
    out.push_str(&url[end..]);
    // It was the last parameter: the separator in front of it is now dangling.
    while out.ends_with('&') || out.ends_with('?') {
        out.pop();
    }
    out
}

/// What one attempt at playing a track brings along, as opposed to the queue
/// entry, which is the same every time it is played.
pub struct Attempt<'a> {
    /// Goes into the stream URL, so the server can tie its transcode job to
    /// the reports the player sends.
    pub play_session: &'a str,
    /// The normalization gain the player picked (track or album mode) -- it is
    /// deliberately not read off the entry here.
    pub gain_db: Option<f32>,
    /// Start somewhere other than the beginning: a seek in a live transcode,
    /// or a reopen after the output device changed.
    pub start_ms: Option<u64>,
    /// Stops the open, and later the source's download (see
    /// [`OpenTicket::stop_token`]).
    pub stop: CancellationToken,
}

/// Open `track` for one attempt at playing it.
pub fn open_track_source(
    auth: &StreamAuth,
    track: &QueueTrack,
    attempt: Attempt<'_>,
    dsp_state: Arc<AudioDsp>,
    tap_state: Arc<VisualizerTap>,
    waveform: Option<WaveformRequest>,
) -> AppResult<OpenedSource> {
    let Attempt {
        play_session,
        gain_db,
        start_ms,
        stop,
    } = attempt;
    use stream_download::source::SourceStream;
    use stream_download::Settings;

    // Never send the current session's token to a host it does not belong to.
    // The queue survives logout and restarts, so a leftover entry can point at
    // a server the user is no longer signed in to.
    if !same_origin(&track.stream_url, &auth.base_url) {
        return Err(AppError::Audio(
            "queue entry belongs to a different server".into(),
        ));
    }

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        auth.auth_header
            .parse()
            .map_err(|_| AppError::Audio("invalid auth header".into()))?,
    );
    // connect/read timeouts (but no total timeout: the download runs for the
    // whole track). Without them one stalled connection would hang this open
    // forever and, worse, previously froze the player command loop.
    let (builder, _observed) = crate::tls::apply(
        reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(crate::api::USER_AGENT)
            .redirect(same_origin_redirects(auth.base_url.clone()))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30)),
        &auth.trusted,
    )?;
    let http = builder.build()?;

    // The queue entry's URL is the track; what belongs to this one attempt at
    // playing it is added here. The play session is what the server correlates
    // its transcode job and our progress reports by.
    let mut stream_url = strip_play_session(&track.stream_url);
    stream_url.push(if stream_url.contains('?') { '&' } else { '?' });
    stream_url.push_str(&format!("playSessionId={play_session}"));
    if let Some(start_ms) = start_ms {
        // .NET ticks: 100 ns.
        stream_url.push_str(&format!("&startTimeTicks={}", start_ms * 10_000));
    }
    let url = stream_url
        .parse()
        .map_err(|e| AppError::Audio(format!("bad stream url: {e}")))?;

    let state = Arc::new(DownloadState::default());
    // From here on, dropping this stops the download: on an error below, when
    // the player throws the open away, or when it lets go of the source.
    let download = Arc::new(DownloadProgress {
        state: state.clone(),
        _stop: stop.clone().drop_guard(),
    });

    // Waveform: once per track, from any play whose download ends up whole.
    // A reopen of a direct-play file counts too: Jellyfin ignores
    // startTimeTicks there and sends the whole file from its start. A live
    // transcode never does, it has no length.
    let wave_file = Arc::new(Mutex::new(None));
    let mut wave_job = waveform
        .filter(|request| request.wanted(&track.item_id, track.duration_ms))
        .map(|request| WaveformJob {
            item_id: track.item_id.clone(),
            duration_ms: track.duration_ms,
            request,
            file: wave_file.clone(),
        });
    let (storage, reading) = storage(wave_job.is_some().then_some(wave_file));

    let progress = state.clone();
    // No prefetch of stream-download's own: see `stream::head_start`.
    let settings = Settings::default().prefetch_bytes(0).on_progress(
        move |stream: &stream::ServerStream, progress_state, _| {
            if progress.report(progress_state, stream.content_length()) {
                if let Some(job) = wave_job.take() {
                    job.start();
                }
            }
        },
    );

    let patience = stream::Patience::PLAYBACK;
    let opened = tauri::async_runtime::block_on(async {
        let opened = stream::open(
            http,
            url,
            (storage, reading),
            settings,
            stop.clone(),
            state.clone(),
            patience,
        )
        .await?;
        // A track starts with a head start — except a direct-play file
        // reopened at a position: that comes whole, from its start, and all
        // the decoder wants from there is its headers before it jumps to the
        // target.
        if start_ms.is_none() || opened.reader.content_length().is_none() {
            stream::head_start(&state, &stop, patience.head_start).await?;
        }
        Ok::<_, AppError>(opened)
    })?;
    let mpeg = opened.mpeg;
    let reader = opened.reader;

    // Unknown content length = live transcode: seeking such a stream would
    // block the audio thread on undownloaded byte ranges.
    let byte_len = reader.content_length();
    let seekable = byte_len.is_some();
    let mut source = match super::opus::probe(reader, seekable) {
        Ok(opus) => EitherSource::Opus(Box::new(opus)),
        // Opus, of a kind our decoder does not take (a surround layout, a
        // broken header). Symphonia has no Opus decoder to fall back on, and
        // its "unrecognized format" would hide why.
        Err((
            _,
            e @ (OpusProbeError::UnsupportedLayout { .. }
            | OpusProbeError::Malformed(_)
            | OpusProbeError::Opus(_)),
        )) => return Err(AppError::Audio(format!("cannot decode the stream: {e}"))),
        Err((reader, _)) => {
            EitherSource::Symphonia(symphonia_decoder(reader, byte_len, mpeg)?, seekable)
        }
    };

    // Jellyfin honours startTimeTicks only when it transcodes; a direct-play
    // file comes back whole, with a known length, and would play from 0 while
    // the UI shows the target. Position that one here, before it reaches the
    // audio graph — rodio would run the seek on the audio callback thread.
    let start_ms = match start_ms {
        Some(ms) if seekable => match source.try_seek(Duration::from_millis(ms)) {
            Ok(()) => ms,
            Err(e) => {
                tracing::warn!("cannot position the reopened stream: {e}");
                0
            }
        },
        Some(ms) => ms,
        None => 0,
    };

    let (faded, fade) = fade::wrap(dsp::wrap(source, dsp_state, gain_db));
    let (tapped, tap_enabled) = tap::wrap(faded, tap_state);
    Ok(OpenedSource {
        source: Reaped::new(tapped),
        fade,
        tap_enabled,
        download,
        start_ms,
    })
}

/// Symphonia's decoder (flac/mp3/vorbis/wav) over `reader`, a stream of
/// `byte_len` bytes when the server said; `mpeg`: it sent MPEG audio.
fn symphonia_decoder<R>(
    mut reader: R,
    byte_len: Option<u64>,
    mpeg: bool,
) -> AppResult<rodio::Decoder<R>>
where
    R: std::io::Read + std::io::Seek + Send + Sync + 'static,
{
    // MP3 has no index, and Symphonia's accurate seek reads every frame from
    // the start (backwards) or the current one up to the target: on the audio
    // callback for a native seek, and waiting for the download up to there
    // for a reopen. Its coarse seek jumps to the byte the target's share of
    // the file points at — and takes the time from that share too, so only a
    // constant-bitrate file gets it (see `mp3`).
    let coarse = mpeg && byte_len.is_some_and(|len| mp3::constant_bitrate(&mut reader, len));
    let mut builder = rodio::Decoder::builder()
        .with_data(reader)
        .with_gapless(true);
    // Telling Symphonia the length is what enables real random-access
    // seeking; without it rodio reports the stream as unseekable and every
    // backward seek degrades to a forward re-scan.
    if let Some(len) = byte_len {
        builder = builder.with_byte_len(len).with_coarse_seek(coarse);
    }
    builder
        .build()
        .map_err(|e| AppError::Audio(format!("decode failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_entrys_play_session_is_replaced_not_doubled() {
        // Middle of the query: the parameter goes, its neighbours stay joined.
        assert_eq!(
            strip_play_session(
                "https://h/Audio/1/universal?userId=u&playSessionId=old&container=x"
            ),
            "https://h/Audio/1/universal?userId=u&container=x"
        );
        // Last parameter: no dangling separator is left behind.
        assert_eq!(
            strip_play_session("https://h/Audio/1/universal?userId=u&playSessionId=old"),
            "https://h/Audio/1/universal?userId=u"
        );
        // Only parameter: the query goes with it.
        assert_eq!(
            strip_play_session("https://h/Audio/1/universal?playSessionId=old"),
            "https://h/Audio/1/universal"
        );
        // A URL built by the current code has none.
        assert_eq!(
            strip_play_session("https://h/Audio/1/universal?userId=u"),
            "https://h/Audio/1/universal?userId=u"
        );
        // A parameter that merely ends in the same name is not one.
        assert_eq!(
            strip_play_session("https://h/Audio/1/universal?notAPlaySessionId=keep"),
            "https://h/Audio/1/universal?notAPlaySessionId=keep"
        );
    }

    #[test]
    fn download_progress_is_a_fraction_of_a_known_length() {
        let progress = DownloadState::default();
        // Nothing reported yet.
        assert_eq!(progress.fraction(), None);

        progress.record(0..250, Some(1000));
        assert_eq!(progress.fraction(), Some((0.0, 0.25)));

        // After a seek the new request fills a later range.
        progress.record(600..800, Some(1000));
        assert_eq!(progress.fraction(), Some((0.6, 0.8)));

        // A range past a (smaller) announced length is clamped.
        progress.record(900..1200, Some(1000));
        assert_eq!(progress.fraction(), Some((0.9, 1.0)));

        // Live transcode: no length, no range to show.
        progress.record(0..4096, None);
        assert_eq!(progress.fraction(), None);

        // An empty range shows nothing.
        progress.record(500..500, Some(1000));
        assert_eq!(progress.fraction(), None);
    }

    #[test]
    fn download_is_complete_only_when_gap_free_from_the_start() {
        let progress = DownloadState::default();
        assert!(!progress.is_complete(), "nothing downloaded");
        progress.record(0..999, Some(1000));
        assert!(!progress.is_complete(), "one byte missing");
        progress.record(0..1000, Some(1000));
        assert!(progress.is_complete());
        // After a seek past the downloaded part: a later range, even if it
        // reaches the end.
        progress.record(400..1000, Some(1000));
        assert!(!progress.is_complete());
        // A live transcode has no length.
        progress.record(0..4096, None);
        assert!(!progress.is_complete());
    }

    #[test]
    fn ranges_on_disk_merge_and_answer_how_much_is_there() {
        let mut ranges = Vec::new();
        merge_range(&mut ranges, 100..200);
        merge_range(&mut ranges, 400..500);
        merge_range(&mut ranges, 0..0);
        assert_eq!(ranges, [100..200, 400..500]);
        // Touching and overlapping ranges become one.
        merge_range(&mut ranges, 200..250);
        merge_range(&mut ranges, 380..410);
        assert_eq!(ranges, [100..250, 380..500]);
        merge_range(&mut ranges, 0..1000);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], 0..1000);

        let ranges = [100..250, 380..500];
        assert_eq!(disk_run(&ranges, 100, 50), 50);
        assert_eq!(
            disk_run(&ranges, 200, 500),
            50,
            "up to the end of the range"
        );
        assert_eq!(disk_run(&ranges, 250, 10), 0, "the end is not on disk");
        assert_eq!(disk_run(&ranges, 300, 10), 0, "a hole");
        assert_eq!(disk_run(&ranges, 99, 10), 0);
        assert_eq!(disk_run(&ranges, 499, 10), 1);
        assert_eq!(disk_run(&[], 0, 10), 0);
    }

    #[test]
    fn the_head_start_is_a_share_of_the_start_or_the_end_of_the_download() {
        let state = DownloadState::default();
        assert!(!state.has_head_start(100));
        state.record(0..60, Some(1000));
        assert!(!state.has_head_start(100));
        state.record(0..100, Some(1000));
        assert!(state.has_head_start(100));
        // A later range (after a seek) is not a head start.
        state.record(500..900, Some(1000));
        assert!(!state.has_head_start(100));
        // A stream shorter than the head start: all of it.
        let short = DownloadState::default();
        short.record(0..40, Some(40));
        assert!(short.has_head_start(100));
        // A live transcode that ended, or any stream that broke off.
        let live = DownloadState::default();
        live.record(0..40, None);
        assert!(!live.has_head_start(100));
        live.finish();
        assert!(live.has_head_start(100));
        let broken = DownloadState::default();
        broken.broke_off();
        assert!(broken.has_head_start(100));
    }

    #[test]
    fn letting_go_of_a_download_stops_it() {
        let stop = CancellationToken::new();
        let state = Arc::new(DownloadState::default());
        let download = DownloadProgress {
            state: state.clone(),
            _stop: stop.clone().drop_guard(),
        };
        assert!(!download.broke_off());
        state.broke_off();
        assert!(download.broke_off());
        assert!(!stop.is_cancelled());
        drop(download);
        assert!(stop.is_cancelled());
    }

    #[test]
    fn a_stale_open_is_stopped_and_a_delivered_one_left_alone() {
        let mut opens = OpenTracker::default();
        let play = opens.start(1, None);
        let prefetch = opens.start(1, Some(7));
        let delivered = opens.start(1, Some(7));
        let (play_stop, prefetch_stop, delivered_stop) = (
            play.stop_token(),
            prefetch.stop_token(),
            delivered.stop_token(),
        );
        delivered.delivered();
        // Nothing changed: all still wanted (the delivered one is forgotten).
        opens.stop_stale(1, 7, None);
        assert!(!play_stop.is_cancelled() && !prefetch_stop.is_cancelled());
        assert_eq!(opens.opens.len(), 2);
        // A queue edit makes the prefetch stale, not the play.
        opens.stop_stale(1, 8, None);
        assert!(prefetch_stop.is_cancelled());
        assert!(!play_stop.is_cancelled());
        // A new generation makes the play stale.
        opens.stop_stale(2, 8, None);
        assert!(play_stop.is_cancelled());
        assert!(opens.opens.is_empty());
        // The delivered open's download belongs to its source by now.
        assert!(!delivered_stop.is_cancelled());
    }

    #[test]
    fn the_prefetch_a_track_end_may_still_adopt_keeps_its_download() {
        // handle_track_end: the prefetch of the track that now loads anew is
        // still on its way under generation 1; a fresh open runs under 2.
        let mut opens = OpenTracker::default();
        let prefetch = opens.start(1, Some(7));
        let other = opens.start(1, None);
        let fresh = opens.start(2, None);
        let (prefetch_stop, other_stop, fresh_stop) = (
            prefetch.stop_token(),
            other.stop_token(),
            fresh.stop_token(),
        );
        opens.stop_stale(2, 7, Some(1));
        assert!(!prefetch_stop.is_cancelled(), "the first to land plays");
        assert!(other_stop.is_cancelled());
        assert!(!fresh_stop.is_cancelled());
        // The fresh open landed first, or a seek or a stop came: no adoption.
        opens.stop_stale(2, 7, None);
        assert!(prefetch_stop.is_cancelled());
        assert!(!fresh_stop.is_cancelled());
    }

    #[test]
    fn stream_copies_an_earlier_run_left_behind_are_swept() {
        let dir =
            std::env::temp_dir().join(format!("sonarune-sweep-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sonarune-folder")).unwrap();
        for name in ["sonarune-a1b2c3", "sonarune-d4e5f6", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(sweep_stream_dir(&dir), 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "sonarune-folder"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_stream_with_a_length_gets_its_waveform_and_reader_handles() {
        use stream_download::storage::StorageProvider;
        // stream-download keeps a stream up to the cap in one slot of the
        // adaptive storage and a longer one in the other: a hi-res FLAC is
        // easily longer.
        for len in [10 * 1024 * 1024, 200 * 1024 * 1024] {
            let wave_file = FileSlot::default();
            let reading = FileSlot::default();
            let files = StreamFiles {
                dir: None,
                wave_file: Some(wave_file.clone()),
                reading: reading.clone(),
            };
            let storage = stream_download::storage::adaptive::AdaptiveStorageProvider::new(
                files,
                std::num::NonZeroUsize::new(UNBOUNDED_STREAM_CAP).unwrap(),
            );
            let (reader, writer) = storage.into_reader_writer(Some(len)).unwrap();
            assert!(
                wave_file.lock().unwrap().is_some(),
                "{len}: no waveform handle"
            );
            assert!(reading.lock().unwrap().is_some(), "{len}: no reader handle");
            drop((reader, writer));
        }
    }

    #[test]
    fn a_stream_copy_survives_its_folder_being_cleaned_away() {
        let dir = std::env::temp_dir().join(format!("sonarune-file-test-{}", uuid::Uuid::new_v4()));
        // The folder is gone (or was never there): made again.
        let file = stream_file(Some(&dir)).expect("a stream copy");
        assert_eq!(file.path().parent(), Some(dir.as_path()));
        assert!(file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(STREAM_FILE_PREFIX));
        drop(file);
        std::fs::remove_dir_all(&dir).unwrap();
        // No folder of our own: the temp directory.
        let file = stream_file(None).expect("a stream copy");
        assert_eq!(file.path().parent(), Some(std::env::temp_dir().as_path()));
    }

    #[test]
    fn a_source_is_dropped_on_the_reaper_thread_not_where_it_is_let_go() {
        struct Witness(Arc<Mutex<Option<std::thread::ThreadId>>>);
        impl Drop for Witness {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = Some(std::thread::current().id());
            }
        }
        let dropped_on = Arc::new(Mutex::new(None));
        drop(Reaped::new(Witness(dropped_on.clone())));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let thread = loop {
            if let Some(thread) = *dropped_on.lock().unwrap() {
                break thread;
            }
            assert!(std::time::Instant::now() < deadline, "never dropped");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_ne!(thread, std::thread::current().id());
    }

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        assert!(same_origin(
            "https://jelly.home/Audio/1/universal?x=1",
            "https://jelly.home"
        ));
        // Default port is equivalent to the explicit one.
        assert!(same_origin(
            "https://jelly.home:443/a",
            "https://jelly.home"
        ));
        assert!(same_origin("http://jelly.home:80/a", "http://jelly.home"));

        // Everything that must NOT pass.
        assert!(!same_origin("https://evil.test/a", "https://jelly.home"));
        assert!(!same_origin("http://jelly.home/a", "https://jelly.home"));
        assert!(!same_origin(
            "https://jelly.home:8443/a",
            "https://jelly.home"
        ));
        // Prefix trickery on the raw string must not fool it.
        assert!(!same_origin(
            "https://evil.test/?u=https://jelly.home",
            "https://jelly.home"
        ));
        assert!(!same_origin(
            "https://jelly.home.evil.test/a",
            "https://jelly.home"
        ));
        // Unparseable input is never the same origin.
        assert!(!same_origin("not a url", "https://jelly.home"));
    }
}
