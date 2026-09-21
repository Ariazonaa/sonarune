//! Opening a queue track as a playable, DSP-wrapped rodio source.
//! Runs on short-lived background threads (never on the player thread) so a
//! slow or stalled server can never freeze the player command loop.

use super::dsp::{self, AudioDsp, DspSource};
use super::fade::{self, FadeHandle, FadeSource};
use super::opus::OggOpusSource;
use super::tap::{self, TapSource, VisualizerTap};
use super::waveform::{WaveformJob, WaveformRequest};
use super::QueueTrack;
use crate::api::JellyfinClient;
use crate::error::{AppError, AppResult};
use rodio::Source;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Cap for streams that never announce a length (live transcodes). Such a
/// stream is treated as unseekable anyway, so a circular buffer costs nothing
/// — and it stops a server that just keeps sending from filling the disk.
/// 64 MiB is ~27 minutes at 320 kbit/s, far more than the read-ahead needs.
const UNBOUNDED_STREAM_CAP: usize = 64 * 1024 * 1024;

/// Adaptive: an unknown-length stream gets a bounded circular buffer (the
/// writer pauses when the reader falls behind), while a stream that announces
/// its size keeps a plain, fully seekable temp file.
pub type Storage = stream_download::storage::adaptive::AdaptiveStorageProvider<
    stream_download::storage::temp::TempStorageProvider,
    stream_download::storage::temp::TempStorageProvider,
>;

pub type Reader = stream_download::StreamDownload<Storage>;

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

pub type TrackSource = TapSource<FadeSource<DspSource<EitherSource>>>;

/// How much of a stream is downloaded — the seek bar's buffered range.
/// Written by the download task's progress callback, read by the player
/// worker on its tick. The two ends are separate atomics; a read that lands
/// between two updates can pair ends of neighboring ranges, which
/// [`Self::fraction`] tolerates (it is a display hint, never a seek bound).
#[derive(Debug, Default)]
pub struct DownloadProgress {
    /// Contiguous downloaded byte range around the download head. After a
    /// seek past the downloaded part, this is the range the new request
    /// fills — not everything from the start.
    start: AtomicU64,
    end: AtomicU64,
    /// Stream length in bytes; 0 = unknown (live transcode).
    total: AtomicU64,
}

impl DownloadProgress {
    fn record(&self, chunk: std::ops::Range<u64>, total: Option<u64>) {
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
        self.start.store(chunk.start, Ordering::Relaxed);
        self.end.store(chunk.end, Ordering::Relaxed);
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
pub fn open_track_source(
    auth: &StreamAuth,
    track: &QueueTrack,
    dsp_state: Arc<AudioDsp>,
    tap_state: Arc<VisualizerTap>,
    start_ms: Option<u64>,
    waveform: Option<WaveformRequest>,
) -> AppResult<OpenedSource> {
    use stream_download::http::HttpStream;
    use stream_download::source::SourceStream;
    use stream_download::storage::adaptive::AdaptiveStorageProvider;
    use stream_download::storage::temp::{tempfile, TempStorageProvider};
    use stream_download::{Settings, StreamDownload, StreamPhase};

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
            .redirect(same_origin_redirects(auth.base_url.clone()))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30)),
        &auth.trusted,
    )?;
    let http = builder.build()?;

    let mut stream_url = track.stream_url.clone();
    if let Some(start_ms) = start_ms {
        // .NET ticks: 100 ns.
        stream_url.push_str(&format!("&startTimeTicks={}", start_ms * 10_000));
    }
    let url = stream_url
        .parse()
        .map_err(|e| AppError::Audio(format!("bad stream url: {e}")))?;

    let download = Arc::new(DownloadProgress::default());
    let progress = download.clone();

    // Waveform: only for a track played from its start, and only once per
    // track. The temp file of a known-length stream gets a second handle the
    // moment it is created — the download task keeps its own handles private.
    let wave_file = Arc::new(std::sync::Mutex::new(None));
    let mut wave_job = waveform
        .filter(|request| start_ms.is_none() && request.cache.get(&track.item_id).is_none())
        .map(|request| WaveformJob {
            item_id: track.item_id.clone(),
            duration_ms: track.duration_ms,
            request,
            file: wave_file.clone(),
        });
    // Used for streams with a known length (the variable one below is the
    // bounded ring buffer of live transcodes, which never holds a whole file).
    let fixed_storage = if wave_job.is_some() {
        TempStorageProvider::with_tempfile_builder(move || {
            let file = tempfile::Builder::new().prefix("jellysic-").tempfile()?;
            if let Ok(second) = file.reopen() {
                *wave_file.lock().unwrap() = Some(second);
            }
            Ok(file)
        })
    } else {
        TempStorageProvider::new()
    };

    let reader = tauri::async_runtime::block_on(async move {
        let stream = HttpStream::new(http, url)
            .await
            .map_err(|e| AppError::Audio(format!("stream open failed: {e}")))?;
        StreamDownload::from_stream(
            stream,
            AdaptiveStorageProvider::with_fixed_and_variable(
                fixed_storage,
                TempStorageProvider::new(),
                std::num::NonZeroUsize::new(UNBOUNDED_STREAM_CAP)
                    .expect("UNBOUNDED_STREAM_CAP is a non-zero constant"),
            ),
            Settings::default().prefetch_bytes(512 * 1024).on_progress(
                move |stream: &HttpStream<reqwest::Client>, state, _| {
                    let total = stream.content_length();
                    // Finished AND gap-free: a seek past the downloaded part
                    // starts a new range, and a file with holes must not be
                    // decoded as if it were whole.
                    let whole = matches!(state.phase, StreamPhase::Complete)
                        && state.current_chunk.start == 0
                        && Some(state.current_chunk.end) == total;
                    progress.record(state.current_chunk, total);
                    if whole {
                        if let Some(job) = wave_job.take() {
                            job.start();
                        }
                    }
                },
            ),
        )
        .await
        .map_err(|e| AppError::Audio(format!("stream buffer failed: {e}")))
    })?;

    // Unknown content length = live transcode: seeking such a stream would
    // block the audio thread on undownloaded byte ranges.
    let byte_len = reader.content_length();
    let seekable = byte_len.is_some();
    let mut source = match super::opus::probe(reader, seekable) {
        Ok(opus) => EitherSource::Opus(Box::new(opus)),
        Err((reader, _)) => {
            let mut builder = rodio::Decoder::builder()
                .with_data(reader)
                .with_gapless(true);
            // Telling Symphonia the length is what enables real random-access
            // seeking; without it rodio reports the stream as unseekable and
            // every backward seek degrades to a forward re-scan.
            if let Some(len) = byte_len {
                builder = builder.with_byte_len(len);
            }
            EitherSource::Symphonia(
                builder
                    .build()
                    .map_err(|e| AppError::Audio(format!("decode failed: {e}")))?,
                seekable,
            )
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

    let (faded, fade) = fade::wrap(dsp::wrap(source, dsp_state, track.normalization_gain));
    let (tapped, tap_enabled) = tap::wrap(faded, tap_state);
    Ok(OpenedSource {
        source: tapped,
        fade,
        tap_enabled,
        download,
        start_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::{same_origin, DownloadProgress};

    #[test]
    fn download_progress_is_a_fraction_of_a_known_length() {
        let progress = DownloadProgress::default();
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
        let progress = DownloadProgress::default();
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
