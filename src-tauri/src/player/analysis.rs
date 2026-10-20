//! Tempo, beat grid and key of a whole track, for transitions in time.
//!
//! Runs on the waveform thread over the same decode (see `waveform.rs`), so a
//! track is never decoded twice for it. Everything here is offline and
//! approximate: a constant-tempo beat grid and one key per track, which is
//! what most dance, pop and rock music has. Every estimate carries its own
//! acceptance test; anything that fails it stays `None`, and the transition
//! falls back to a plain crossfade.
//!
//! - Tempo: spectral-flux onset envelope (~86 frames/s), autocorrelated over
//!   70..180 BPM with a mild preference for 120 BPM.
//! - Beat grid: the phase of that period, fitted separately to the first and
//!   the last 30 s of audible audio — a live drummer drifts, and only the
//!   ends matter for a transition.
//! - Key: a chromagram summed over the track, matched against the
//!   Krumhansl–Kessler major/minor profiles.
//! - Sound: timbre, brightness, attack and dynamics (`timbre.rs`), for sonic
//!   similarity.

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Bump when the algorithm changes; stored results of older versions are
/// recomputed.
pub const VERSION: u32 = 1;

/// Analysis rate after downmix and decimation (approximately; the exact rate
/// is the source rate divided by a whole factor).
const TARGET_RATE: f32 = 11_025.0;
const ONSET_FFT: usize = 1024;
const ONSET_HOP: usize = 128;
const CHROMA_FFT: usize = 4096;
/// Chroma frames every this many onset hops (4096 / 128 / 2 = 50 % overlap).
const CHROMA_EVERY: usize = 16;
/// Upper edge of the band whose onsets place the beat grid (kick, bass).
const LOW_BAND_HZ: f32 = 180.0;
const MIN_BPM: f32 = 70.0;
const MAX_BPM: f32 = 180.0;
/// Seconds at either end the beat grid is fitted to.
const GRID_WINDOW_S: f32 = 30.0;
/// A track this quiet relative to its loudest hop counts as silent there.
pub(super) const SILENCE_DB: f32 = -45.0;
/// Acceptance thresholds (see the tests for what clean and noisy input give).
const MIN_TEMPO_CONFIDENCE: f32 = 0.15;
const MIN_GRID_CONFIDENCE: f32 = 1.5;
const MIN_KEY_CORRELATION: f32 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicalKey {
    /// Pitch class of the tonic, 0 = C … 11 = B.
    pub tonic: u8,
    pub minor: bool,
}

impl MusicalKey {
    /// Camelot wheel number (1..=12); relative major/minor share it.
    pub fn camelot_number(self) -> u8 {
        let major_tonic = if self.minor {
            (self.tonic + 3) % 12
        } else {
            self.tonic
        };
        // Steps of a fifth from C, where C major is 8B.
        let fifths = (u32::from(major_tonic) * 7) % 12;
        ((fifths + 7) % 12 + 1) as u8
    }

    /// "8A" (minor) / "8B" (major).
    pub fn camelot(self) -> String {
        format!(
            "{}{}",
            self.camelot_number(),
            if self.minor { 'A' } else { 'B' }
        )
    }

    /// Harmonic-mixing rule of thumb: same key, relative major/minor, or one
    /// step around the wheel in the same mode.
    pub fn compatible(self, other: MusicalKey) -> bool {
        let (a, b) = (self.camelot_number(), other.camelot_number());
        if a == b {
            return true;
        }
        let step = (i32::from(a) - i32::from(b)).rem_euclid(12);
        self.minor == other.minor && (step == 1 || step == 11)
    }
}

/// One edge of the track: a beat of the grid there, and how well the grid
/// fits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatGrid {
    /// A beat position (ms from the start of the track): the first beat of
    /// the intro grid, the last beat of the outro grid.
    pub beat_ms: f64,
    /// Beat length there (the local tempo, refined from the global one).
    pub period_ms: f64,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackAnalysis {
    pub version: u32,
    pub bpm: Option<f32>,
    pub tempo_confidence: f32,
    /// Grid near the start / the end; `None` where it does not fit.
    pub intro: Option<BeatGrid>,
    pub outro: Option<BeatGrid>,
    pub key: Option<MusicalKey>,
    pub key_correlation: f32,
    /// First and last moment above [`SILENCE_DB`].
    pub audible_start_ms: f64,
    pub audible_end_ms: f64,
    /// The decoded length.
    pub duration_ms: f64,
    /// The length the server gave the track when it was analysed. The decoded
    /// length can be seconds off it for good (a VBR MP3 whose length the
    /// server estimated), so only this one tells a replaced file. Absent in
    /// analyses stored before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_duration_ms: Option<u64>,
    /// How the track sounds (`timbre.rs`). Absent in analyses stored before it
    /// existed; the library scan fills those in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sonic: Option<super::timbre::SonicFeatures>,
}

/// A server length further than this from the one an analysis was made for
/// is another file under the same item id.
const REPLACED_TOLERANCE_MS: f64 = 2_000.0;

impl TrackAnalysis {
    pub fn camelot(&self) -> Option<String> {
        self.key.map(MusicalKey::camelot)
    }

    /// Whether the analysis still describes the file the server has under
    /// the item id, given the length the server reports now (0: unknown). A
    /// replaced file has a different length. Analyses from before the server
    /// length was recorded can only be held against the decoded one.
    pub fn fits(&self, duration_ms: u64) -> bool {
        if duration_ms == 0 {
            return true;
        }
        let analysed_for = self
            .server_duration_ms
            .map_or(self.duration_ms, |ms| ms as f64);
        (analysed_for - duration_ms as f64).abs() <= REPLACED_TOLERANCE_MS
    }
}

/// How a crossfade lands in time: the next track starts when the current one
/// reaches `start_at_ms`, so that its first beat falls on a beat of the
/// current one; the current one fades out over `fade_out_ms`, the next one in
/// over `fade_in_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionPlan {
    pub start_at_ms: u64,
    pub fade_out_ms: u64,
    pub fade_in_ms: u64,
}

/// Longest the beats of the two tracks may drift apart over the overlap
/// before it sounds like a flam.
const MAX_DRIFT_MS: f64 = 35.0;
/// The next track's first beat may come this late; a longer beatless intro
/// would have to start under the current track for too long.
const MAX_FIRST_BEAT_MS: f64 = 10_000.0;
const MAX_OVERLAP_MS: f64 = 20_000.0;

/// Plan a beat-aligned transition from `a` into `b` for a crossfade of about
/// `requested_fade_ms`. `None` whenever either grid is missing or the tempos
/// do not fit together; the caller then crossfades as usual.
///
/// - The overlap is a whole number of bars of `a` (four beats each), short
///   enough that the two grids drift apart by at most [`MAX_DRIFT_MS`], and
///   a single bar when the keys clash. Less than a bar would be a cut, not
///   a blend: then no plan.
/// - It ends where `a` turns silent, on `a`'s grid.
/// - `b` starts so that its first beat lands on the overlap's first beat;
///   a beatless intro of `b` plays under the end of `a`.
pub fn plan_transition(
    a: &TrackAnalysis,
    b: &TrackAnalysis,
    requested_fade_ms: u64,
) -> Option<TransitionPlan> {
    let outro = a.outro?;
    let intro = b.intro?;
    let beat_a = outro.period_ms;
    if beat_a < 200.0 || intro.period_ms < 100.0 {
        return None;
    }
    // b may run at half or double a's tempo; its beats still land on a's.
    let mismatch = [0.5, 1.0, 2.0]
        .iter()
        .map(|m| (intro.period_ms * m - beat_a).abs())
        .fold(f64::INFINITY, f64::min);
    if mismatch / beat_a > 0.04 {
        return None;
    }
    let mut bars = ((requested_fade_ms as f64 / beat_a / 4.0).round() as u32).max(1);
    if let (Some(ka), Some(kb)) = (a.key, b.key) {
        if !ka.compatible(kb) {
            bars = 1;
        }
    }
    if mismatch > 0.0 {
        let fitting = (MAX_DRIFT_MS / mismatch / 4.0)
            .floor()
            .min(f64::from(u32::MAX)) as u32;
        bars = bars.min(fitting);
    }
    if bars == 0 {
        return None;
    }
    let beats = bars * 4;
    let overlap = f64::from(beats) * beat_a;
    let first_beat = intro.beat_ms;
    if first_beat > MAX_FIRST_BEAT_MS || first_beat + overlap > MAX_OVERLAP_MS {
        return None;
    }
    let end = a.audible_end_ms.min(a.duration_ms);
    // The latest beat of a's grid that leaves room for the overlap.
    let latest = end - overlap;
    let downbeat = outro.beat_ms + ((latest - outro.beat_ms) / beat_a).floor() * beat_a;
    let start = downbeat - first_beat;
    if start <= a.audible_start_ms {
        return None;
    }
    Some(TransitionPlan {
        start_at_ms: start.round() as u64,
        fade_out_ms: (first_beat + overlap).round() as u64,
        fade_in_ms: (first_beat + overlap / 2.0).max(500.0).round() as u64,
    })
}

/// Analyses of recent tracks in memory, all of them in the database: a track
/// analysed once is ready the next time it comes up, even after a restart.
pub struct AnalysisCache {
    recent: Mutex<VecDeque<(String, Arc<TrackAnalysis>)>>,
    store: Option<Arc<crate::store::Store>>,
}

const RECENT_ANALYSES: usize = 128;

impl AnalysisCache {
    pub fn new(store: Option<Arc<crate::store::Store>>) -> Self {
        Self {
            recent: Mutex::new(VecDeque::new()),
            store,
        }
    }

    pub fn get(&self, item_id: &str) -> Option<Arc<TrackAnalysis>> {
        if let Some(found) = self
            .recent
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| id == item_id)
            .map(|(_, a)| a.clone())
        {
            return Some(found);
        }
        let stored = self
            .store
            .as_ref()?
            .get_analysis(item_id)
            .and_then(|json| serde_json::from_str::<TrackAnalysis>(&json).ok())
            .filter(|a| a.version == VERSION)
            .map(Arc::new)?;
        self.remember(item_id, stored.clone());
        Some(stored)
    }

    /// [`get`](Self::get), but only while the analysis still fits the
    /// track: a file replaced on the server under the same item id has a
    /// different length ([`TrackAnalysis::fits`]).
    pub fn get_matching(&self, item_id: &str, duration_ms: u64) -> Option<Arc<TrackAnalysis>> {
        self.get(item_id).filter(|a| a.fits(duration_ms))
    }

    pub fn put(&self, item_id: &str, analysis: TrackAnalysis) -> Arc<TrackAnalysis> {
        if let (Some(store), Ok(json)) = (&self.store, serde_json::to_string(&analysis)) {
            if let Err(e) = store.put_analysis(item_id, &json) {
                tracing::warn!("cannot store the analysis of {item_id}: {e}");
            }
        }
        let analysis = Arc::new(analysis);
        self.remember(item_id, analysis.clone());
        analysis
    }

    fn remember(&self, item_id: &str, analysis: Arc<TrackAnalysis>) {
        let mut recent = self.recent.lock().unwrap();
        recent.retain(|(id, _)| id != item_id);
        recent.push_back((item_id.to_string(), analysis));
        while recent.len() > RECENT_ANALYSES {
            recent.pop_front();
        }
    }
}

/// Streaming analyser: feed interleaved samples, then [`Analyzer::finish`].
pub struct Analyzer {
    channels: usize,
    channel: usize,
    frame_sum: f32,
    decimation: usize,
    decimated_in: usize,
    decimated_sum: f32,
    rate: f32,
    /// Decimated mono, the last [`CHROMA_FFT`] samples plus the pending hop.
    buffer: Vec<f32>,
    total: usize,
    hops: usize,
    onset_fft: Arc<dyn Fft<f32>>,
    chroma_fft: Arc<dyn Fft<f32>>,
    onset_window: Vec<f32>,
    chroma_window: Vec<f32>,
    scratch: Vec<Complex32>,
    previous_spectrum: Vec<f32>,
    primed: bool,
    /// Onset strength per hop: broadband (tempo) and the kick/bass band
    /// (beat phase — hats on the offbeat dominate the broadband flux).
    flux: Vec<f32>,
    low_flux: Vec<f32>,
    /// Onset strength within the fixed band of the sound features
    /// ([`super::timbre::Timbre::flux_bins`]): the broadband flux reaches up
    /// to the Nyquist frequency, which moves with the source rate.
    band_flux: Vec<f32>,
    /// `None` for a source too low to carry anything above the band.
    anti_alias: Option<AntiAlias>,
    /// Loudness per hop (dB, unreferenced).
    level_db: Vec<f32>,
    chroma: [f64; 12],
    /// Pitch class per chroma bin, or 12 for bins outside the used range.
    chroma_bins: Vec<u8>,
    timbre: super::timbre::Timbre,
    /// Power spectrum of the current onset frame, for [`Self::timbre`].
    power: Vec<f32>,
}

/// Timbre frames every this many onset hops (~21 frames/s): plenty for
/// means and spreads, a quarter of the cost of every hop.
const TIMBRE_EVERY: usize = 4;

/// Cutoff of the low-pass in front of the decimation: the top of the band
/// the sound features look at (`timbre.rs`).
const ANTI_ALIAS_HZ: f64 = 5_000.0;

/// Sixth-order Butterworth low-pass (three biquads) at the source rate,
/// ahead of the boxcar decimation. The boxcar alone lets a hi-hat at 8 kHz
/// fold down into the analysed band at only -7 to -9 dB, and onto another
/// frequency for every source rate (3 kHz from 44.1 kHz, 4 kHz from 48 kHz):
/// a FLAC and its Opus transcode would not sound alike. With the filter in
/// front, whatever could fold is some 25 dB further down.
struct AntiAlias {
    sections: [Biquad; 3],
}

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn low_pass(rate: f64, cutoff: f64, q: f64) -> Self {
        let (sin, cos) = (std::f64::consts::TAU * cutoff / rate).sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        // A long digital silence lets the state decay into subnormal numbers,
        // which are many times slower to compute with.
        if self.z1.abs() < 1e-30 && self.z2.abs() < 1e-30 {
            self.z1 = 0.0;
            self.z2 = 0.0;
        }
        y
    }
}

impl AntiAlias {
    /// `None` when the source cannot carry the cutoff anyway: nothing above
    /// the band to fold.
    fn new(sample_rate: u32) -> Option<Self> {
        let rate = f64::from(sample_rate);
        if ANTI_ALIAS_HZ >= rate * 0.45 {
            return None;
        }
        // The pole pairs of a sixth-order Butterworth: Q = 1 / (2 cos θ) for
        // θ = 15°, 45°, 75°.
        let section = |q| Biquad::low_pass(rate, ANTI_ALIAS_HZ, q);
        Some(Self {
            sections: [
                section(0.517_638),
                section(std::f64::consts::FRAC_1_SQRT_2),
                section(1.931_852),
            ],
        })
    }

    #[inline]
    fn process(&mut self, sample: f32) -> f32 {
        self.sections
            .iter_mut()
            .fold(f64::from(sample), |value, section| section.process(value)) as f32
    }
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = std::f32::consts::PI * 2.0 * i as f32 / n as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect()
}

impl Analyzer {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let decimation = ((sample_rate as f32 / TARGET_RATE).round() as usize).max(1);
        let rate = sample_rate as f32 / decimation as f32;
        let mut planner = FftPlanner::new();
        let chroma_bins = (0..CHROMA_FFT / 2)
            .map(|bin| {
                let freq = bin as f32 * rate / CHROMA_FFT as f32;
                if !(100.0..=2_000.0).contains(&freq) {
                    return 12;
                }
                let midi = 69.0 + 12.0 * (freq / 440.0).log2();
                (midi.round() as i32).rem_euclid(12) as u8
            })
            .collect();
        Self {
            channels: channels.max(1),
            channel: 0,
            frame_sum: 0.0,
            decimation,
            decimated_in: 0,
            decimated_sum: 0.0,
            rate,
            buffer: Vec::with_capacity(CHROMA_FFT + ONSET_HOP),
            total: 0,
            hops: 0,
            onset_fft: planner.plan_fft_forward(ONSET_FFT),
            chroma_fft: planner.plan_fft_forward(CHROMA_FFT),
            onset_window: hann(ONSET_FFT),
            chroma_window: hann(CHROMA_FFT),
            scratch: vec![Complex32::default(); CHROMA_FFT],
            previous_spectrum: vec![0.0; ONSET_FFT / 2],
            primed: false,
            flux: Vec::new(),
            low_flux: Vec::new(),
            band_flux: Vec::new(),
            anti_alias: AntiAlias::new(sample_rate),
            level_db: Vec::new(),
            chroma: [0.0; 12],
            chroma_bins,
            timbre: super::timbre::Timbre::new(ONSET_FFT / 2, rate / ONSET_FFT as f32),
            power: vec![0.0; ONSET_FFT / 2],
        }
    }

    #[inline]
    pub fn push(&mut self, sample: f32) {
        let sample = if sample.is_finite() { sample } else { 0.0 };
        self.frame_sum += sample;
        self.channel += 1;
        if self.channel < self.channels {
            return;
        }
        let mono = self.frame_sum / self.channels as f32;
        self.channel = 0;
        self.frame_sum = 0.0;
        let mono = match &mut self.anti_alias {
            Some(filter) => filter.process(mono),
            None => mono,
        };
        // Boxcar decimation: on its own a crude low-pass, which is enough for
        // pitch (below ~2 kHz) and onsets; the filter above keeps what lies
        // beyond the band from folding into it.
        self.decimated_sum += mono;
        self.decimated_in += 1;
        if self.decimated_in < self.decimation {
            return;
        }
        let value = self.decimated_sum / self.decimation as f32;
        self.decimated_in = 0;
        self.decimated_sum = 0.0;
        self.buffer.push(value);
        self.total += 1;
        if self.total.is_multiple_of(ONSET_HOP) {
            self.hop();
        }
    }

    fn hop(&mut self) {
        let len = self.buffer.len();
        let hop = &self.buffer[len - ONSET_HOP..];
        let power = hop.iter().map(|v| v * v).sum::<f32>() / ONSET_HOP as f32;
        self.level_db.push(10.0 * power.max(1e-12).log10());

        if len >= ONSET_FFT {
            let frame = &self.buffer[len - ONSET_FFT..];
            for (slot, (v, w)) in self
                .scratch
                .iter_mut()
                .zip(frame.iter().zip(&self.onset_window))
            {
                *slot = Complex32::new(v * w, 0.0);
            }
            self.onset_fft.process(&mut self.scratch[..ONSET_FFT]);
            let bin_hz = self.rate / ONSET_FFT as f32;
            let low_bins = (LOW_BAND_HZ / bin_hz).ceil() as usize;
            let band_bins = self.timbre.flux_bins();
            let (mut flux, mut low, mut band) = (0.0, 0.0, 0.0);
            for (bin, previous) in self.previous_spectrum.iter_mut().enumerate() {
                // Log compression keeps a loud kick from drowning the hats.
                let magnitude = (1.0 + 100.0 * self.scratch[bin].norm()).ln();
                let rise = (magnitude - *previous).max(0.0);
                flux += rise;
                if bin >= 1 && bin <= low_bins {
                    low += rise;
                }
                if band_bins.contains(&bin) {
                    band += rise;
                }
                *previous = magnitude;
            }
            // The first frame has nothing to rise from.
            let first = !self.primed;
            self.primed = true;
            self.flux.push(if first { 0.0 } else { flux });
            self.low_flux.push(if first { 0.0 } else { low });
            self.band_flux.push(if first { 0.0 } else { band });
            if self.hops.is_multiple_of(TIMBRE_EVERY) {
                for (slot, value) in self.power.iter_mut().zip(&self.scratch) {
                    *slot = value.norm_sqr();
                }
                self.timbre.push(self.hops, &self.power);
            }
        } else {
            self.flux.push(0.0);
            self.low_flux.push(0.0);
            self.band_flux.push(0.0);
        }

        self.hops += 1;
        if len >= CHROMA_FFT && self.hops.is_multiple_of(CHROMA_EVERY) {
            let frame = &self.buffer[len - CHROMA_FFT..];
            for (slot, (v, w)) in self
                .scratch
                .iter_mut()
                .zip(frame.iter().zip(&self.chroma_window))
            {
                *slot = Complex32::new(v * w, 0.0);
            }
            self.chroma_fft.process(&mut self.scratch);
            let mut frame_chroma = [0f64; 12];
            for (bin, &pc) in self.chroma_bins.iter().enumerate() {
                if pc < 12 {
                    frame_chroma[usize::from(pc)] += f64::from(self.scratch[bin].norm_sqr());
                }
            }
            // Per-frame normalization: every moment votes equally, a loud
            // chorus does not outweigh the rest.
            let sum: f64 = frame_chroma.iter().sum();
            if sum > 1e-9 {
                for (total, value) in self.chroma.iter_mut().zip(frame_chroma) {
                    *total += (value / sum).sqrt();
                }
            }
        }

        // Keep what the next frames still need.
        if len > CHROMA_FFT {
            self.buffer.drain(..len - CHROMA_FFT);
        }
    }

    pub fn finish(self) -> TrackAnalysis {
        let hop_ms = ONSET_HOP as f64 * 1000.0 / f64::from(self.rate);
        let duration_ms = self.total as f64 * 1000.0 / f64::from(self.rate);
        let (start_hop, end_hop) = audible_range(&self.level_db);
        let audible_start_ms = start_hop as f64 * hop_ms;
        let audible_end_ms = (end_hop as f64 * hop_ms).min(duration_ms);

        let envelope = onset_envelope(&self.flux);
        let low_envelope = onset_envelope(&self.low_flux);
        let frame_rate = 1000.0 / hop_ms;
        let tempo = estimate_tempo(&envelope[start_hop..end_hop.max(start_hop)], frame_rate);
        let (bpm, tempo_confidence) = match tempo {
            Some((bpm, confidence)) if confidence >= MIN_TEMPO_CONFIDENCE => {
                (Some(bpm), confidence)
            }
            Some((_, confidence)) => (None, confidence),
            None => (None, 0.0),
        };

        let (intro, outro) = match bpm {
            Some(bpm) => {
                let period = frame_rate * 60.0 / f64::from(bpm);
                let window = (f64::from(GRID_WINDOW_S) * frame_rate) as usize;
                let intro_end = (start_hop + window).min(end_hop);
                let outro_start = end_hop.saturating_sub(window).max(start_hop);
                let grid = |from: usize, to: usize, last: bool| {
                    fit_grid(&low_envelope, from, to, period)
                        .filter(|fit| fit.confidence >= MIN_GRID_CONFIDENCE)
                        // A track without kick or bass: the broadband onsets.
                        .or_else(|| fit_grid(&envelope, from, to, period))
                        .map(|fit| BeatGrid {
                            beat_ms: (if last { fit.last } else { fit.first }) * hop_ms,
                            period_ms: fit.period * hop_ms,
                            confidence: fit.confidence,
                        })
                };
                let intro = grid(start_hop, intro_end, false);
                let outro = grid(outro_start, end_hop, true);
                (
                    intro.filter(|g| g.confidence >= MIN_GRID_CONFIDENCE),
                    outro.filter(|g| g.confidence >= MIN_GRID_CONFIDENCE),
                )
            }
            None => (None, None),
        };

        let band_envelope = onset_envelope(&self.band_flux);
        let sonic = Some(
            self.timbre
                .finish(
                    start_hop,
                    end_hop,
                    &self.level_db,
                    &self.band_flux,
                    &band_envelope,
                    &low_envelope,
                )
                .unwrap_or_else(super::timbre::SonicFeatures::none),
        );

        let (key, key_correlation) = match estimate_key(&self.chroma) {
            Some((key, r)) if r >= MIN_KEY_CORRELATION => (Some(key), r),
            Some((_, r)) => (None, r),
            None => (None, 0.0),
        };

        TrackAnalysis {
            version: VERSION,
            bpm,
            tempo_confidence,
            intro,
            outro,
            key,
            key_correlation,
            audible_start_ms,
            audible_end_ms,
            duration_ms,
            // Only the caller knows what the server says (`waveform.rs`).
            server_duration_ms: None,
            sonic,
        }
    }
}

/// First and one-past-last hop above [`SILENCE_DB`] relative to the
/// loudest hop.
fn audible_range(level_db: &[f32]) -> (usize, usize) {
    let loudest = level_db.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !loudest.is_finite() || loudest <= -110.0 {
        return (0, level_db.len());
    }
    let threshold = loudest + SILENCE_DB;
    let start = level_db.iter().position(|&l| l > threshold).unwrap_or(0);
    let end = level_db
        .iter()
        .rposition(|&l| l > threshold)
        .map(|i| i + 1)
        .unwrap_or(level_db.len());
    (start, end.max(start))
}

/// Flux minus its local mean (~0.5 s), floored at zero: what is left are the
/// onsets standing out of their surroundings.
fn onset_envelope(flux: &[f32]) -> Vec<f32> {
    const HALF: usize = 21;
    let mut prefix = Vec::with_capacity(flux.len() + 1);
    prefix.push(0f64);
    for &v in flux {
        prefix.push(prefix.last().unwrap() + f64::from(v));
    }
    (0..flux.len())
        .map(|i| {
            let lo = i.saturating_sub(HALF);
            let hi = (i + HALF + 1).min(flux.len());
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            (f64::from(flux[i]) - mean).max(0.0) as f32
        })
        .collect()
}

/// Best tempo in BPM and its confidence (normalized autocorrelation there).
fn estimate_tempo(envelope: &[f32], frame_rate: f64) -> Option<(f32, f32)> {
    let min_lag = (frame_rate * 60.0 / f64::from(MAX_BPM)).floor() as usize;
    let max_lag = (frame_rate * 60.0 / f64::from(MIN_BPM)).ceil() as usize;
    if envelope.len() < max_lag * 8 || min_lag < 2 {
        return None;
    }
    let mean = envelope.iter().map(|&v| f64::from(v)).sum::<f64>() / envelope.len() as f64;
    let centered: Vec<f64> = envelope.iter().map(|&v| f64::from(v) - mean).collect();
    // Up to twice the longest lag: the score below also reads the harmonic.
    let acf: Vec<f64> = (0..=2 * max_lag + 1)
        .map(|lag| {
            let n = centered.len() - lag;
            let sum: f64 = centered[..n]
                .iter()
                .zip(&centered[lag..])
                .map(|(a, b)| a * b)
                .sum();
            sum / n as f64
        })
        .collect();
    if acf[0] <= 0.0 {
        return None;
    }
    // A beat period also correlates at twice its length; adding half of
    // that harmonic favours the true beat over its half-tempo neighbour.
    let score = |lag: usize| -> f64 {
        let bpm = frame_rate * 60.0 / lag as f64;
        let prior = (-0.5 * ((bpm / 120.0).log2() / 0.9).powi(2)).exp();
        let harmonic = acf.get(lag * 2).copied().unwrap_or(0.0).max(0.0);
        (acf[lag].max(0.0) + 0.5 * harmonic) * prior
    };
    let (best, best_score) = (min_lag..=max_lag)
        .map(|lag| (lag, score(lag)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if best_score <= 0.0 {
        return None;
    }
    // Sub-frame peak: parabola through the neighbours of the raw ACF.
    let lag = if best > min_lag && best < max_lag {
        let (l, c, r) = (acf[best - 1], acf[best], acf[best + 1]);
        let denom = l - 2.0 * c + r;
        if denom.abs() > 1e-12 {
            best as f64 + (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
        } else {
            best as f64
        }
    } else {
        best as f64
    };
    let bpm = (frame_rate * 60.0 / lag) as f32;
    // Normalized autocorrelation at the beat: how much of the envelope's
    // variance repeats with that period (noise: ~0, a drum loop: > 0.3).
    let confidence = (acf[best] / acf[0]) as f32;
    Some((bpm, confidence))
}

/// Linear interpolation into the envelope.
fn sample_at(envelope: &[f32], position: f64) -> f64 {
    if position < 0.0 {
        return 0.0;
    }
    let i = position.floor() as usize;
    let frac = position - i as f64;
    let a = envelope.get(i).copied().unwrap_or(0.0);
    let b = envelope.get(i + 1).copied().unwrap_or(0.0);
    f64::from(a) + (f64::from(b) - f64::from(a)) * frac
}

/// A beat grid fitted to one stretch of the envelope (all in hops).
#[derive(Debug, Clone, Copy)]
struct GridFit {
    /// First and last grid beat that actually carry an onset.
    first: f64,
    last: f64,
    period: f64,
    /// Best comb score over the mean across all phases.
    confidence: f32,
}

/// Fit a beat grid near `period` (in hops) to `envelope[start..end]`. The
/// period is refined within ±1.5 %: the global tempo estimate is a little
/// off, and over 30 s even half a BPM drifts the grid by several hops.
fn fit_grid(envelope: &[f32], start: usize, end: usize, period: f64) -> Option<GridFit> {
    if end <= start || ((end - start) as f64) < period * 8.0 {
        return None;
    }
    const PHASES: usize = 64;
    const PERIOD_STEPS: i32 = 15;
    let comb = |period: f64, phase: f64| -> f64 {
        let mut sum = 0.0;
        let mut beats = 0;
        let mut t = start as f64 + phase;
        while t < end as f64 {
            // A little of the neighbourhood: onsets smear over a hop or two.
            sum += sample_at(envelope, t)
                + 0.5 * (sample_at(envelope, t - 1.0) + sample_at(envelope, t + 1.0));
            beats += 1;
            t += period;
        }
        if beats == 0 {
            0.0
        } else {
            sum / f64::from(beats)
        }
    };
    let mut best: Option<(f64, f64, f64, f64)> = None; // period, phase, score, mean
    for step in -PERIOD_STEPS..=PERIOD_STEPS {
        let p = period * (1.0 + f64::from(step) * 0.001);
        let scores: Vec<(f64, f64)> = (0..PHASES)
            .map(|k| {
                let phase = p * k as f64 / PHASES as f64;
                (phase, comb(p, phase))
            })
            .collect();
        let mean = scores.iter().map(|(_, s)| s).sum::<f64>() / PHASES as f64;
        if let Some(&(phase, score)) = scores.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
            if best.is_none_or(|(_, _, b, _)| score > b) {
                best = Some((p, phase, score, mean));
            }
        }
    }
    let (period, phase, score, mean) = best?;
    if mean <= 0.0 || score <= 0.0 {
        return None;
    }
    // The grid may start before the beat does (a pad or a sung intro) and
    // run on after it stopped: the ends are the beats with at least a third
    // of the typical strength. Any grid beat is in phase; these are where
    // the rhythm is.
    let local = |t: f64| {
        sample_at(envelope, t)
            .max(sample_at(envelope, t - 1.0))
            .max(sample_at(envelope, t + 1.0))
    };
    let strong = |t: f64| local(t) * 2.0 >= score / 3.0;
    let beats: Vec<f64> = (0..)
        .map(|k| start as f64 + phase + k as f64 * period)
        .take_while(|&t| t < end as f64)
        .collect();
    let first = beats
        .iter()
        .copied()
        .find(|&t| strong(t))
        .unwrap_or(beats[0]);
    let last = beats
        .iter()
        .rev()
        .copied()
        .find(|&t| strong(t))
        .unwrap_or(*beats.last().unwrap_or(&first));
    Some(GridFit {
        first,
        last,
        period,
        confidence: (score / mean) as f32,
    })
}

/// Krumhansl–Kessler key profiles (C major / C minor).
const MAJOR_PROFILE: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn pearson(a: &[f64; 12], b: impl Fn(usize) -> f64) -> f64 {
    let mean_a = a.iter().sum::<f64>() / 12.0;
    let mean_b = (0..12).map(&b).sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (i, &value) in a.iter().enumerate() {
        let x = value - mean_a;
        let y = b(i) - mean_b;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da.sqrt() * db.sqrt())
    }
}

fn estimate_key(chroma: &[f64; 12]) -> Option<(MusicalKey, f32)> {
    if chroma.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    let mut best: Option<(MusicalKey, f64)> = None;
    for tonic in 0..12u8 {
        for minor in [false, true] {
            let profile = if minor {
                &MINOR_PROFILE
            } else {
                &MAJOR_PROFILE
            };
            let r = pearson(chroma, |i| profile[(i + 12 - usize::from(tonic)) % 12]);
            if best.is_none_or(|(_, b)| r > b) {
                best = Some((MusicalKey { tonic, minor }, r));
            }
        }
    }
    best.map(|(key, r)| (key, r as f32))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A drum-machine-like loop: a kick (decaying 60 Hz burst) on every beat
    /// and a hat (noise burst) on the offbeats, over a sustained chord.
    pub(crate) fn beat_track(
        rate: u32,
        bpm: f32,
        seconds: f32,
        first_beat_s: f32,
        chord_hz: &[f32],
    ) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        let beat = 60.0 / bpm;
        let mut noise = 0x2545_f491u32;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let mut v = 0.0;
                for &f in chord_hz {
                    v += 0.08 * (2.0 * std::f32::consts::PI * f * t).sin();
                }
                if t >= first_beat_s {
                    let since = (t - first_beat_s) % beat;
                    if since < 0.12 {
                        v += 0.8
                            * (-since * 30.0).exp()
                            * (2.0 * std::f32::consts::PI * 60.0 * since).sin();
                    }
                    let off = (t - first_beat_s + beat / 2.0) % beat;
                    if off < 0.03 {
                        noise ^= noise << 13;
                        noise ^= noise >> 17;
                        noise ^= noise << 5;
                        v += 0.3
                            * (-off * 120.0).exp()
                            * ((noise as f32 / u32::MAX as f32) * 2.0 - 1.0);
                    }
                }
                v
            })
            .collect()
    }

    fn analyse(samples: &[f32], rate: u32) -> TrackAnalysis {
        let mut a = Analyzer::new(rate, 1);
        for &s in samples {
            a.push(s);
        }
        a.finish()
    }

    // A minor triad (A3, C4, E4) plus the bass A2.
    const A_MINOR: [f32; 4] = [110.0, 220.0, 261.63, 329.63];
    // C major triad (C3, E3, G3, C4).
    const C_MAJOR: [f32; 4] = [130.81, 164.81, 196.0, 261.63];

    fn grid_track(bpm: f64, first_beat_ms: f64, last_beat_ms: f64, end_ms: f64) -> TrackAnalysis {
        let period_ms = 60_000.0 / bpm;
        TrackAnalysis {
            version: VERSION,
            bpm: Some(bpm as f32),
            tempo_confidence: 0.8,
            intro: Some(BeatGrid {
                beat_ms: first_beat_ms,
                period_ms,
                confidence: 5.0,
            }),
            outro: Some(BeatGrid {
                beat_ms: last_beat_ms,
                period_ms,
                confidence: 5.0,
            }),
            key: None,
            key_correlation: 0.0,
            audible_start_ms: 0.0,
            audible_end_ms: end_ms,
            duration_ms: end_ms + 1_000.0,
            server_duration_ms: None,
            sonic: None,
        }
    }

    #[test]
    fn a_planned_transition_lands_the_next_first_beat_on_a_beat() {
        // 120 BPM: 500 ms beats; a's grid runs through 200_000 + k * 500.
        let a = grid_track(120.0, 0.0, 200_000.0, 201_300.0);
        let b = grid_track(120.0, 1_250.0, 180_000.0, 190_000.0);
        let plan = plan_transition(&a, &b, 6_000).expect("plan");
        // 6 s = 12 beats of overlap, ending by 201_300: the latest grid beat
        // with room for it is 195_000.
        let downbeat = plan.start_at_ms as f64 + 1_250.0;
        assert_eq!(downbeat, 195_000.0);
        assert_eq!(plan.fade_out_ms, 7_250);
        assert_eq!(plan.fade_in_ms, 4_250);
        assert_eq!((downbeat - 200_000.0).rem_euclid(500.0), 0.0, "on a's grid");
    }

    #[test]
    fn tempos_that_do_not_fit_are_not_beat_matched() {
        let a = grid_track(120.0, 0.0, 200_000.0, 201_000.0);
        assert!(plan_transition(&a, &grid_track(132.0, 0.0, 1.0, 9.0e4), 6_000).is_none());
        // Double time fits.
        assert!(plan_transition(&a, &grid_track(240.0, 0.0, 1.0, 9.0e4), 6_000).is_some());
        // Close tempos shorten the overlap until the drift stays inaudible:
        // 120 vs 121 BPM drift ~4.1 ms per beat -> two bars at most.
        let close = plan_transition(&a, &grid_track(121.0, 0.0, 1.0, 9.0e4), 8_000).unwrap();
        assert_eq!(close.fade_out_ms, 4_000);
        // 120 vs 123 BPM: ~12 ms per beat, not even one bar.
        assert!(plan_transition(&a, &grid_track(123.0, 0.0, 1.0, 9.0e4), 8_000).is_none());
    }

    #[test]
    fn clashing_keys_keep_the_overlap_short() {
        let mut a = grid_track(120.0, 0.0, 200_000.0, 201_000.0);
        let mut b = grid_track(120.0, 0.0, 1.0, 9.0e4);
        a.key = Some(MusicalKey {
            tonic: 0,
            minor: false,
        }); // 8B
        b.key = Some(MusicalKey {
            tonic: 6,
            minor: false,
        }); // 2B
        let plan = plan_transition(&a, &b, 8_000).unwrap();
        assert_eq!(plan.fade_out_ms, 2_000, "one bar");
        b.key = Some(MusicalKey {
            tonic: 7,
            minor: false,
        }); // 9B
        assert_eq!(plan_transition(&a, &b, 8_000).unwrap().fade_out_ms, 8_000);
    }

    #[test]
    fn missing_grids_or_long_intros_fall_back() {
        let a = grid_track(120.0, 0.0, 200_000.0, 201_000.0);
        let mut b = grid_track(120.0, 0.0, 1.0, 9.0e4);
        b.intro = None;
        assert!(plan_transition(&a, &b, 6_000).is_none());
        assert!(plan_transition(&a, &grid_track(120.0, 15_000.0, 1.0, 9.0e4), 6_000).is_none());
        let mut a2 = a.clone();
        a2.outro = None;
        assert!(plan_transition(&a2, &grid_track(120.0, 0.0, 1.0, 9.0e4), 6_000).is_none());
    }

    #[test]
    fn analyses_survive_in_the_store_and_old_versions_are_ignored() {
        let dir = std::env::temp_dir().join(format!("sonarune-analysis-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(crate::store::Store::open(&dir).unwrap());
        let a = grid_track(120.0, 0.0, 1.0, 2.0);
        AnalysisCache::new(Some(store.clone())).put("t", a.clone());
        assert_eq!(
            AnalysisCache::new(Some(store.clone())).get("t").as_deref(),
            Some(&a)
        );
        let mut old = a;
        old.version = VERSION + 1;
        store
            .put_analysis("t", &serde_json::to_string(&old).unwrap())
            .unwrap();
        assert!(AnalysisCache::new(Some(store.clone())).get("t").is_none());
        // A replaced file: same id, other length.
        let cache = AnalysisCache::new(Some(store));
        cache.put("u", grid_track(120.0, 0.0, 1.0, 179_000.0));
        assert!(cache.get_matching("u", 180_500).is_some());
        assert!(cache.get_matching("u", 240_000).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_replaced_file_is_told_by_the_server_length_not_the_decoded_one() {
        // A VBR MP3 whose length the server estimated: it decodes to 247 s,
        // the server says 238 s, and it keeps saying so.
        let mut a = grid_track(120.0, 0.0, 1.0, 246_000.0);
        a.server_duration_ms = Some(238_000);
        assert!(a.fits(238_000), "the same file");
        assert!(a.fits(239_500), "the server rounding its estimate");
        assert!(
            !a.fits(247_000),
            "another file, even one as long as the decode"
        );
        assert!(a.fits(0), "length unknown");
        // Stored before the server length was recorded: only the decode to
        // go by.
        a.server_duration_ms = None;
        assert!(a.fits(247_500) && !a.fits(238_000));
    }

    /// The sound vector of twelve seconds of `signal`, sampled at `rate`.
    fn sonic_vector(rate: u32, signal: impl Fn(f32) -> f32) -> Vec<f32> {
        let samples: Vec<f32> = (0..rate as usize * 12)
            .map(|i| signal(i as f32 / rate as f32))
            .collect();
        let analysis = analyse(&samples, rate);
        analysis
            .sonic
            .filter(|s| !s.vector.is_empty())
            .expect("a sound vector")
            .vector
    }

    #[test]
    fn the_sound_does_not_depend_on_the_source_rate() {
        use std::f32::consts::TAU;
        // A low tone and a loud one far above the analysed band, the way a
        // hi-hat sits over a bass line. From 44.1 kHz the high one used to
        // fold down to ~3 kHz, from 48 kHz to ~4 kHz.
        let signal = |t: f32| 0.4 * (TAU * 220.0 * t).sin() + 0.4 * (TAU * 8_000.0 * t).sin();
        let cd = sonic_vector(44_100, signal);
        let dat = sonic_vector(48_000, signal);
        let low_only = sonic_vector(44_100, |t| 0.4 * (TAU * 220.0 * t).sin());
        // Spectral centroid (kHz) and rolloff: the same, and the high tone
        // is gone from both.
        for (dimension, name) in [(24, "centroid"), (27, "rolloff")] {
            assert!(
                (cd[dimension] - dat[dimension]).abs() < 0.02,
                "{name}: {} vs {}",
                cd[dimension],
                dat[dimension]
            );
            assert!(
                (cd[dimension] - low_only[dimension]).abs() < 0.02,
                "{name} carries the folded tone: {} vs {}",
                cd[dimension],
                low_only[dimension]
            );
        }
    }

    /// Real music: `SONARUNE_ANALYSE_DIR=<folder> cargo test --release
    /// real_files -- --ignored --nocapture` prints what every file gives and
    /// the plan between neighbours. There is no ground truth to assert on.
    #[test]
    #[ignore]
    fn real_files() {
        let Ok(dir) = std::env::var("SONARUNE_ANALYSE_DIR") else {
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        files.sort();
        let mut previous: Option<TrackAnalysis> = None;
        for path in files {
            let started = std::time::Instant::now();
            let file = std::fs::File::open(&path).unwrap();
            let len = file.metadata().unwrap().len();
            let decoder = rodio::Decoder::builder()
                .with_data(std::io::BufReader::new(file))
                .with_byte_len(len)
                .build()
                .unwrap();
            use rodio::Source as _;
            let mut analyzer = Analyzer::new(
                decoder.sample_rate().get(),
                usize::from(decoder.channels().get()),
            );
            for sample in decoder {
                analyzer.push(sample);
            }
            let a = analyzer.finish();
            eprintln!(
                "{}: {:.1} BPM (conf {:.2}) key {:?} (r {:.2}) intro {:?} outro {:?} audible {:.0}..{:.0}/{:.0} ms [{} ms]",
                path.file_name().unwrap().to_string_lossy(),
                a.bpm.unwrap_or(0.0),
                a.tempo_confidence,
                a.camelot(),
                a.key_correlation,
                a.intro.map(|g| (g.beat_ms.round(), g.period_ms, g.confidence)),
                a.outro.map(|g| (g.beat_ms.round(), g.period_ms, g.confidence)),
                a.audible_start_ms,
                a.audible_end_ms,
                a.duration_ms,
                started.elapsed().as_millis()
            );
            if let Some(prev) = &previous {
                eprintln!(
                    "    plan from previous: {:?}",
                    plan_transition(prev, &a, 6_000)
                );
            }
            previous = Some(a);
        }
    }

    #[test]
    fn camelot_codes_follow_the_wheel() {
        let key = |tonic, minor| MusicalKey { tonic, minor };
        assert_eq!(key(0, false).camelot(), "8B"); // C
        assert_eq!(key(9, true).camelot(), "8A"); // Am
        assert_eq!(key(7, false).camelot(), "9B"); // G
        assert_eq!(key(5, false).camelot(), "7B"); // F
        assert_eq!(key(11, false).camelot(), "1B"); // B
        assert_eq!(key(8, true).camelot(), "1A"); // G#m
        assert_eq!(key(2, true).camelot(), "7A"); // Dm
        assert!(key(0, false).compatible(key(9, true)), "relative minor");
        assert!(key(0, false).compatible(key(7, false)), "a fifth up");
        assert!(key(11, false).compatible(key(4, false)), "1B and 12B wrap");
        assert!(!key(0, false).compatible(key(2, false)), "two steps");
        assert!(!key(0, false).compatible(key(4, true)), "8B vs 9A");
    }

    #[test]
    fn a_steady_beat_is_found_with_its_tempo_and_grid() {
        for (bpm, first) in [(128.0f32, 0.5f32), (95.0, 1.3), (174.0, 0.2)] {
            let samples = beat_track(22_050, bpm, 60.0, first, &A_MINOR);
            let a = analyse(&samples, 22_050);
            let found = a.bpm.unwrap_or_else(|| panic!("tempo for {bpm}: {a:?}"));
            assert!((found - bpm).abs() < 1.0, "{bpm} BPM came out as {found}");
            let beat_ms = 60_000.0 / f64::from(bpm);
            let intro = a.intro.expect("intro grid");
            // The first beat, not a later one, within a hop or two.
            let err = intro.beat_ms - f64::from(first) * 1000.0;
            assert!(err.abs() < 15.0, "{bpm}: first beat off by {err} ms");
            let outro = a.outro.expect("outro grid");
            let phase = ((outro.beat_ms - f64::from(first) * 1000.0) / beat_ms).rem_euclid(1.0);
            let off = phase.min(1.0 - phase) * beat_ms;
            assert!(off < 15.0, "{bpm}: outro grid off by {off} ms");
            assert!(
                (outro.period_ms - beat_ms).abs() < 1.0,
                "{bpm}: local period"
            );
        }
    }

    #[test]
    fn the_key_of_a_held_chord_is_found() {
        let a = analyse(&beat_track(22_050, 120.0, 30.0, 0.0, &A_MINOR), 22_050);
        assert_eq!(
            a.key,
            Some(MusicalKey {
                tonic: 9,
                minor: true
            }),
            "{a:?}"
        );
        let c = analyse(&beat_track(22_050, 120.0, 30.0, 0.0, &C_MAJOR), 22_050);
        assert_eq!(
            c.key,
            Some(MusicalKey {
                tonic: 0,
                minor: false
            }),
            "{c:?}"
        );
    }

    #[test]
    fn noise_has_no_tempo_and_silence_has_nothing() {
        let mut state = 1u32;
        let noise: Vec<f32> = (0..22_050 * 30)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        let a = analyse(&noise, 22_050);
        assert_eq!(a.bpm, None, "{a:?}");
        let s = analyse(&vec![0.0; 22_050 * 10], 22_050);
        assert_eq!((s.bpm, s.key), (None, None));
    }

    #[test]
    fn leading_and_trailing_silence_is_measured() {
        let rate = 22_050;
        let mut samples = vec![0.0; rate as usize * 2];
        samples.extend(beat_track(rate, 120.0, 20.0, 0.0, &C_MAJOR));
        samples.extend(vec![0.0; rate as usize * 3]);
        let a = analyse(&samples, rate);
        assert!(
            (a.audible_start_ms - 2000.0).abs() < 30.0,
            "{}",
            a.audible_start_ms
        );
        assert!(
            (a.audible_end_ms - 22_000.0).abs() < 200.0,
            "{}",
            a.audible_end_ms
        );
        assert!((a.duration_ms - 25_000.0).abs() < 30.0);
    }

    #[test]
    fn stereo_and_high_rates_give_the_same_tempo() {
        let mono = beat_track(48_000, 128.0, 40.0, 0.25, &A_MINOR);
        let mut a = Analyzer::new(48_000, 2);
        for s in &mono {
            a.push(*s);
            a.push(*s);
        }
        let a = a.finish();
        assert!((a.bpm.expect("tempo") - 128.0).abs() < 1.0, "{a:?}");
    }
}
