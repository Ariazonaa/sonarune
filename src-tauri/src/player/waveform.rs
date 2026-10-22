//! Whole-track waveform for the seek bar.
//!
//! Jellyfin has no audio waveform endpoint, so the peaks are computed here —
//! without a second download: a stream with a known length is downloaded
//! completely into a temp file anyway. Once that download has finished
//! without gaps, a background thread decodes the same file a second time
//! (through its own handle, see `source.rs`) and folds it into [`BUCKETS`]
//! peak values. Live transcodes without a length get no waveform, just as
//! they get no buffered range.
//!
//! The same pass feeds the tempo/key analysis (`analysis.rs`), so a track is
//! decoded once for both.

use super::analysis::{AnalysisCache, Analyzer, TrackAnalysis};
use rodio::Source;
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

/// Resolution of the stored peaks; the UI groups them to its bar count.
pub const BUCKETS: usize = 512;

/// Analysed tracks kept for the session (512 bytes each).
const CACHE_TRACKS: usize = 64;

/// Called from the analysis thread with the item id and its peaks.
pub type WaveformNotify = Arc<dyn Fn(&str, &[u8]) + Send + Sync>;
/// Called from the analysis thread with the item id and its analysis.
pub type AnalysisNotify = Arc<dyn Fn(&str, &TrackAnalysis) + Send + Sync>;

/// Peaks of recently analysed tracks, plus the ones being analysed right now
/// — the same track is opened more than once (prefetch, replay, seek) and
/// must not be decoded twice in parallel.
#[derive(Default)]
pub struct WaveformCache {
    done: Mutex<VecDeque<(String, Arc<Vec<u8>>)>>,
    running: Mutex<HashSet<String>>,
}

impl WaveformCache {
    pub fn get(&self, item_id: &str) -> Option<Arc<Vec<u8>>> {
        self.done
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| id == item_id)
            .map(|(_, peaks)| peaks.clone())
    }

    /// Claim the analysis of `item_id`; false when it is running, or cached
    /// and not `needed` anyway (the tempo analysis may still be missing).
    fn begin(&self, item_id: &str, needed: bool) -> bool {
        if !needed && self.get(item_id).is_some() {
            return false;
        }
        self.running.lock().unwrap().insert(item_id.to_string())
    }

    fn finish(&self, item_id: &str, peaks: Option<Arc<Vec<u8>>>) {
        if let Some(peaks) = peaks {
            let mut done = self.done.lock().unwrap();
            done.retain(|(id, _)| id != item_id);
            done.push_back((item_id.to_string(), peaks));
            while done.len() > CACHE_TRACKS {
                done.pop_front();
            }
        }
        self.running.lock().unwrap().remove(item_id);
    }
}

/// What an open needs to hand a finished download to the analysis.
#[derive(Clone)]
pub struct WaveformRequest {
    pub cache: Arc<WaveformCache>,
    pub notify: WaveformNotify,
    pub analyses: Arc<AnalysisCache>,
    pub analysis_notify: AnalysisNotify,
}

impl WaveformRequest {
    /// Whether opening `item_id` from its start should arm a job.
    pub fn wanted(&self, item_id: &str, duration_ms: u64) -> bool {
        self.cache.get(item_id).is_none()
            || self.analyses.get_matching(item_id, duration_ms).is_none()
    }
}

/// One track's pending analysis, armed by the open and fired by the download
/// task once the whole file is on disk.
pub struct WaveformJob {
    pub item_id: String,
    pub duration_ms: u64,
    /// The server the track is streamed from, fixed when the job is armed:
    /// the download can end after a sign-in to another one.
    pub server: crate::identity::Server,
    pub request: WaveformRequest,
    /// A second handle on the stream's temp file, opened when the file was
    /// created — the download task's own handles are not reachable.
    pub file: Arc<Mutex<Option<File>>>,
}

impl WaveformJob {
    /// Start the analysis on its own thread. Never blocks the caller (the
    /// download task) beyond taking the file handle.
    pub fn start(self) {
        let analysis_missing = self
            .request
            .analyses
            .get_matching(&self.item_id, self.duration_ms)
            .is_none();
        if !self.request.cache.begin(&self.item_id, analysis_missing) {
            return;
        }
        let Some(file) = self.file.lock().unwrap().take() else {
            self.request.cache.finish(&self.item_id, None);
            return;
        };
        let WaveformJob {
            item_id,
            duration_ms,
            server,
            request,
            ..
        } = self;
        let spawned = std::thread::Builder::new()
            .name("sonarune-waveform".into())
            .spawn({
                let item_id = item_id.clone();
                let request = request.clone();
                move || {
                    // A decoder panicking on a malformed file must still
                    // close the job, or the track stays marked as in progress.
                    let (peaks, analysis) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            analyse(file, duration_ms)
                        }))
                        .ok()
                        .flatten()
                        .map_or((None, None), |(peaks, analysis)| {
                            (Some(Arc::new(peaks)), Some(analysis))
                        });
                    if let Some(analysis) = analysis {
                        let analysis = request.analyses.put(Some(&server), &item_id, analysis);
                        (request.analysis_notify)(&item_id, &analysis);
                    }
                    // Cache first: a UI that asks right after the event
                    // must find it.
                    request.cache.finish(&item_id, peaks.clone());
                    match peaks {
                        Some(peaks) => (request.notify)(&item_id, &peaks),
                        None => tracing::debug!("no waveform for {item_id}"),
                    }
                }
            });
        if spawned.is_err() {
            request.cache.finish(&item_id, None);
        }
    }
}

/// Decode a completely downloaded stream and fold it into peaks and the
/// tempo/key analysis. Uses the same decoder choice as playback: libopus for
/// Ogg-Opus, Symphonia for the rest.
fn analyse(mut file: File, duration_ms: u64) -> Option<(Vec<u8>, TrackAnalysis)> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let len = file.metadata().ok()?.len();
    let reader = BufReader::new(file);
    let (peaks, analysis) = match super::opus::probe(reader, true) {
        Ok(opus) => fold(opus, duration_ms),
        Err((reader, _)) => {
            let decoder = rodio::Decoder::builder()
                .with_data(reader)
                .with_byte_len(len)
                .with_gapless(true)
                .build()
                .ok()?;
            fold(decoder, duration_ms)
        }
    }?;
    Some((peaks, made_for(analysis, duration_ms)))
}

/// Remember the length the server gave the track, which is what tells a
/// replaced file later ([`TrackAnalysis::fits`]).
fn made_for(mut analysis: TrackAnalysis, duration_ms: u64) -> TrackAnalysis {
    analysis.server_duration_ms = (duration_ms > 0).then_some(duration_ms);
    analysis
}

/// Only the tempo/key/sound analysis of a track held in memory (the library
/// scan), without peaks.
pub fn analyse_bytes(bytes: Vec<u8>, duration_ms: u64) -> Option<TrackAnalysis> {
    let len = bytes.len() as u64;
    let reader = std::io::Cursor::new(bytes);
    match super::opus::probe(reader, true) {
        Ok(opus) => analysis_only(opus),
        Err((reader, _)) => {
            let decoder = rodio::Decoder::builder()
                .with_data(reader)
                .with_byte_len(len)
                .with_gapless(true)
                .build()
                .ok()?;
            analysis_only(decoder)
        }
    }
    .filter(|analysis| {
        // A transcode that came back much shorter than the track is a
        // broken download, not the track. Longer, or a few seconds short, is
        // the decode being right where the server only estimated.
        duration_ms == 0 || analysis.duration_ms + 5_000.0 >= duration_ms as f64
    })
    .map(|analysis| made_for(analysis, duration_ms))
}

fn analysis_only<S: Source>(source: S) -> Option<TrackAnalysis> {
    let channels = usize::from(source.channels().get());
    let rate = source.sample_rate().get();
    let mut analyzer = Analyzer::new(rate, channels);
    for sample in source {
        analyzer.push(sample);
    }
    Some(analyzer.finish())
}

fn fold<S: Source>(source: S, duration_ms: u64) -> Option<(Vec<u8>, TrackAnalysis)> {
    let channels = usize::from(source.channels().get());
    let rate = u64::from(source.sample_rate().get());
    // The seek bar spans the server's duration, so the buckets do too.
    let duration_ms = if duration_ms > 0 {
        duration_ms
    } else {
        source.total_duration()?.as_millis() as u64
    };
    let total_frames = duration_ms.saturating_mul(rate) / 1000;
    let mut analyzer = Analyzer::new(rate as u32, channels);
    let peaks = peaks(
        source.inspect(|&s| analyzer.push(s)),
        channels,
        total_frames,
    );
    Some((peaks, analyzer.finish()))
}

/// Fold interleaved samples into [`BUCKETS`] peaks over `total_frames`
/// frames, scaled so the loudest bucket is 255. Frames past `total_frames`
/// (a server duration that is slightly short) land in the last bucket.
pub fn peaks(samples: impl Iterator<Item = f32>, channels: usize, total_frames: u64) -> Vec<u8> {
    let mut max = vec![0f32; BUCKETS];
    let channels = channels.max(1) as u64;
    let total = total_frames.max(1);
    for (index, sample) in samples.enumerate() {
        let frame = index as u64 / channels;
        let bucket = ((frame * BUCKETS as u64) / total).min(BUCKETS as u64 - 1) as usize;
        let amplitude = sample.abs();
        // NaN compares false and is skipped.
        if amplitude > max[bucket] {
            max[bucket] = amplitude;
        }
    }
    let loudest = max.iter().copied().fold(0f32, f32::max);
    if loudest <= 0.0 {
        return vec![0; BUCKETS];
    }
    max.iter()
        .map(|peak| ((peak / loudest).min(1.0) * 255.0).round() as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{analyse, peaks, WaveformCache, WaveformJob, WaveformRequest, BUCKETS};
    use crate::player::analysis::AnalysisCache;
    use std::fs::File;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    /// A mono 16-bit PCM WAV: half a second of silence, then half a second of
    /// a full-scale square wave.
    fn half_silent_wav(path: &std::path::Path) {
        let rate: u32 = 8000;
        let samples: Vec<i16> = (0..rate)
            .map(|i| match (i < rate / 2, i % 2 == 0) {
                (true, _) => 0,
                (false, true) => i16::MAX,
                (false, false) => -i16::MAX,
            })
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, wav).expect("write test wav");
    }

    fn temp_wav() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("sonarune-wave-test-{}.wav", uuid::Uuid::new_v4()));
        half_silent_wav(&path);
        path
    }

    #[test]
    fn a_real_file_decodes_into_its_loudness_shape() {
        let path = temp_wav();
        let result = analyse(File::open(&path).expect("open test wav"), 1000);
        let _ = std::fs::remove_file(&path);
        let (p, analysis) = result.expect("the wav decodes");
        assert!(
            (analysis.audible_start_ms - 500.0).abs() < 30.0,
            "{analysis:?}"
        );
        assert_eq!(
            analysis.server_duration_ms,
            Some(1000),
            "what it was made for"
        );
        assert_eq!(p.len(), BUCKETS);
        assert!(p[..BUCKETS / 2 - 2].iter().all(|v| *v == 0), "silent half");
        assert!(p[BUCKETS / 2 + 2..].iter().all(|v| *v > 250), "loud half");
    }

    #[test]
    fn a_started_job_fills_the_cache_before_it_notifies() {
        let path = temp_wav();
        let cache = Arc::new(WaveformCache::default());
        let (tx, rx) = mpsc::channel();
        let seen_by_notify = cache.clone();
        let job = WaveformJob {
            item_id: "track".into(),
            duration_ms: 1000,
            server: crate::identity::Server {
                address: "https://music.home".into(),
                id: None,
            },
            request: WaveformRequest {
                cache: cache.clone(),
                notify: Arc::new(move |id: &str, peaks: &[u8]| {
                    let cached = seen_by_notify.get(id).is_some();
                    tx.send((id.to_string(), peaks.len(), cached)).unwrap();
                }),
                analyses: Arc::new(AnalysisCache::new(None)),
                analysis_notify: Arc::new(|_: &str, _: &crate::player::analysis::TrackAnalysis| {}),
            },
            file: Arc::new(Mutex::new(Some(File::open(&path).expect("open test wav")))),
        };
        job.start();
        let (id, len, cached) = rx.recv_timeout(Duration::from_secs(20)).expect("notified");
        let _ = std::fs::remove_file(&path);
        assert_eq!((id.as_str(), len, cached), ("track", BUCKETS, true));
    }

    #[test]
    fn an_analysis_made_while_playing_is_filed_under_the_server_the_track_came_from() {
        // The download of a track from server A ends after a sign-in to B:
        // the job was armed for A, and A's it stays.
        let dir =
            std::env::temp_dir().join(format!("sonarune-wave-store-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(crate::store::Store::open(&dir).expect("open store"));
        store.record_server("http://a.home", "aaaa").unwrap();
        store
            .set(crate::store::keys::SERVER_URL, "https://b.home")
            .unwrap();
        let path = temp_wav();
        let (tx, rx) = mpsc::channel();
        let job = WaveformJob {
            item_id: "track".into(),
            duration_ms: 1000,
            server: crate::identity::Server {
                address: "http://a.home".into(),
                id: Some("aaaa".into()),
            },
            request: WaveformRequest {
                cache: Arc::new(WaveformCache::default()),
                notify: Arc::new(|_: &str, _: &[u8]| {}),
                analyses: Arc::new(AnalysisCache::new(Some(store.clone()))),
                analysis_notify: Arc::new(
                    move |id: &str, _: &crate::player::analysis::TrackAnalysis| {
                        tx.send(id.to_string()).unwrap();
                    },
                ),
            },
            file: Arc::new(Mutex::new(Some(File::open(&path).expect("open test wav")))),
        };
        job.start();
        let id = rx.recv_timeout(Duration::from_secs(20)).expect("analysed");
        let _ = std::fs::remove_file(&path);
        let row = store.analysis_row(&id).expect("stored");
        assert_eq!(
            (row.server_url.as_deref(), row.server_id.as_deref()),
            (Some("http://a.home"), Some("aaaa"))
        );
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn peaks_follow_the_loudness_over_the_timeline() {
        // Mono: a quiet first half, a loud second half.
        let frames = 1024u64;
        let samples = (0..frames).map(|f| if f < frames / 2 { 0.25 } else { -1.0 });
        let p = peaks(samples, 1, frames);
        assert_eq!(p.len(), BUCKETS);
        assert_eq!(p[0], 64);
        assert_eq!(p[BUCKETS - 1], 255);
    }

    #[test]
    fn stereo_frames_share_a_bucket_and_overflow_lands_last() {
        // Two channels; the frame count is understated by half.
        let samples = std::iter::repeat_n(0.5f32, 4000);
        let p = peaks(samples, 2, 1000);
        assert!(p.iter().all(|v| *v == 255));
    }

    #[test]
    fn silence_and_nan_give_a_flat_line() {
        assert!(peaks(std::iter::repeat_n(0.0, 100), 1, 100)
            .iter()
            .all(|v| *v == 0));
        assert!(peaks(std::iter::repeat_n(f32::NAN, 100), 1, 100)
            .iter()
            .all(|v| *v == 0));
    }

    #[test]
    fn the_same_track_is_analysed_once_at_a_time_and_then_cached() {
        let cache = WaveformCache::default();
        assert!(cache.begin("a", false));
        assert!(!cache.begin("a", true), "already running");
        cache.finish("a", Some(Arc::new(vec![1, 2, 3])));
        assert!(!cache.begin("a", false), "cached");
        assert_eq!(cache.get("a").as_deref(), Some(&vec![1, 2, 3]));
        // Cached peaks, but the tempo analysis is still missing.
        assert!(cache.begin("a", true));
        cache.finish("a", None);
        // A failed analysis may be retried.
        assert!(cache.begin("b", false));
        cache.finish("b", None);
        assert!(cache.begin("b", false));
    }
}
