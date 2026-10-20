//! Per-track DSP chain: volume normalization (Jellyfin NormalizationGain +
//! preamp) and an optional 10-band peaking EQ. A boost may take a track past
//! full scale here; the output limiter (`limiter.rs`) turns the mix down
//! after the volume instead of this stage clipping it. Standalone besides
//! rodio/serde/std.

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

const BAND_COUNT: usize = 10;
/// Standard 10-band graphic EQ centers (Hz).
const EQ_BANDS_HZ: [f32; BAND_COUNT] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
const EQ_BAND_Q: f32 = 1.1;
const EQ_GAIN_LIMIT_DB: f32 = 12.0;
const PREAMP_LIMIT_DB: f32 = 15.0;
/// Bound for the server-supplied per-track normalization gain. Real
/// ReplayGain values sit within a few dB of zero; this leaves generous room
/// while keeping a hostile or corrupt value from reaching the mixer.
const NORM_LIMIT_DB: f32 = 30.0;
/// Params are also re-checked at every span boundary; this bounds the
/// reaction latency to a settings change inside a long span.
const PARAM_CHECK_INTERVAL: u32 = 1024;
/// A settings change glides over this long: a gain that steps, or an EQ
/// switched on or off from one sample to the next, clicks on sustained
/// sound.
const RAMP: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DspParams {
    pub eq_enabled: bool,
    /// Per-band gain in dB, clamped to -12..=12 on application.
    pub eq_gains_db: [f32; BAND_COUNT],
    pub normalization_enabled: bool,
    /// Album mode: every track of an album is played at one gain, so the quiet
    /// piece stays quieter than the loud one — what an album was mixed for.
    /// Off means per-track. A flag rather than a third state of
    /// `normalization_enabled`, so settings saved by older builds keep working.
    #[serde(default)]
    pub album_normalization: bool,
    /// Extra gain in dB on top of the track's normalization gain, clamped to
    /// -15..=15. Only applied when a track gain is present.
    pub preamp_db: f32,
}

impl Default for DspParams {
    fn default() -> Self {
        Self {
            eq_enabled: false,
            eq_gains_db: [0.0; BAND_COUNT],
            normalization_enabled: true,
            album_normalization: false,
            preamp_db: 0.0,
        }
    }
}

/// Shared DSP settings. Commands update it; playing [`DspSource`]s poll the
/// revision counter and pick changes up within [`PARAM_CHECK_INTERVAL`] samples.
#[derive(Debug)]
pub struct AudioDsp {
    params: RwLock<DspParams>,
    revision: AtomicU64,
}

impl AudioDsp {
    pub fn new(initial: DspParams) -> Self {
        Self {
            params: RwLock::new(initial),
            revision: AtomicU64::new(0),
        }
    }

    pub fn update(&self, params: DspParams) {
        *self.params.write().expect("dsp params lock poisoned") = params;
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub fn params(&self) -> DspParams {
        self.params
            .read()
            .expect("dsp params lock poisoned")
            .clone()
    }

    /// Revision is read first: if an update lands in between, we pair newer
    /// params with an older revision and simply re-read on the next check.
    fn snapshot(&self) -> (DspParams, u64) {
        let revision = self.revision.load(Ordering::Acquire);
        let params = self
            .params
            .read()
            .expect("dsp params lock poisoned")
            .clone();
        (params, revision)
    }
}

/// Audio EQ Cookbook peaking EQ, normalized by a0.
#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoeffs {
    fn peaking(sample_rate: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let a = 10f64.powf(f64::from(gain_db) / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(sample_rate);
        let alpha = w0.sin() / (2.0 * f64::from(q));
        let cos_w0 = w0.cos();
        let a0 = 1.0 + alpha / a;
        Self {
            b0: ((1.0 + alpha * a) / a0) as f32,
            b1: (-2.0 * cos_w0 / a0) as f32,
            b2: ((1.0 - alpha * a) / a0) as f32,
            a1: (-2.0 * cos_w0 / a0) as f32,
            a2: ((1.0 - alpha / a) / a0) as f32,
        }
    }

    /// Transposed direct form II.
    #[inline]
    fn process(&self, state: &mut BiquadState, x: f32) -> f32 {
        // After silence the state decays into subnormal floats, which x86
        // computes in a slow path — per band, per sample, on the audio
        // callback. -400 dB is inaudible; snap it to zero.
        const FLOOR: f32 = 1e-20;
        let y = self.b0 * x + state.z1;
        state.z1 = self.b1 * x - self.a1 * y + state.z2;
        state.z2 = self.b2 * x - self.a2 * y;
        if state.z1.abs() < FLOOR {
            state.z1 = 0.0;
        }
        if state.z2.abs() < FLOOR {
            state.z2 = 0.0;
        }
        y
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

/// A value that moves to its target in equal steps, one per frame.
#[derive(Debug, Clone, Copy)]
struct Ramp {
    value: f32,
    target: f32,
    step: f32,
}

impl Ramp {
    fn at(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
        }
    }

    fn glide(&mut self, target: f32, frames: u32) {
        self.target = target;
        self.step = (target - self.value) / frames.max(1) as f32;
    }

    #[inline]
    fn advance(&mut self) {
        if self.step == 0.0 {
            return;
        }
        self.value += self.step;
        if (self.step > 0.0 && self.value >= self.target)
            || (self.step < 0.0 && self.value <= self.target)
        {
            // Exactly on target, so the stage is bit-exact again afterwards.
            self.value = self.target;
            self.step = 0.0;
        }
    }
}

fn ramp_frames(sample_rate: SampleRate) -> u32 {
    (sample_rate.get() as f32 * RAMP.as_secs_f32()) as u32
}

/// A [`Source`] adapter applying, in order: normalization gain, peaking EQ.
pub struct DspSource<S> {
    inner: S,
    dsp: Arc<AudioDsp>,
    track_gain_db: Option<f32>,
    params: DspParams,
    revision: u64,
    /// Normalization factor (gain and preamp as a multiplier).
    norm: Ramp,
    /// How much of the filtered signal plays: 1 with the EQ on, 0 with it
    /// off. The filters only run while it is above 0.
    eq_mix: Ramp,
    /// Nothing handed out yet: a settings change applies at once, there is
    /// nothing to glide from.
    fresh: bool,
    /// Shared per band across channels; `None` bypasses the band.
    coeffs: [Option<BiquadCoeffs>; BAND_COUNT],
    /// One state set per channel (interleaved input).
    filter_state: Vec<[BiquadState; BAND_COUNT]>,
    channels: ChannelCount,
    sample_rate: SampleRate,
    channel_idx: usize,
    /// Samples left before the inner source may change format. `None` means
    /// the format never changes.
    span_remaining: Option<usize>,
    until_param_check: u32,
}

/// Wraps `source` in the DSP chain.
///
/// `track_gain_db` is Jellyfin's NormalizationGain for this track (dB), if
/// any. Without it normalization applies nothing — not even the preamp.
pub fn wrap<S>(source: S, dsp: Arc<AudioDsp>, track_gain_db: Option<f32>) -> DspSource<S>
where
    S: Source,
{
    let (params, revision) = dsp.snapshot();
    let channels = source.channels();
    let sample_rate = source.sample_rate();
    let span_remaining = source.current_span_len();
    let norm = Ramp::at(norm_factor(&params, track_gain_db));
    let eq_mix = Ramp::at(if params.eq_enabled { 1.0 } else { 0.0 });
    let mut wrapped = DspSource {
        inner: source,
        dsp,
        track_gain_db,
        params,
        revision,
        norm,
        eq_mix,
        fresh: true,
        coeffs: [None; BAND_COUNT],
        filter_state: vec![Default::default(); channels.get() as usize],
        channels,
        sample_rate,
        channel_idx: 0,
        span_remaining,
        // The first sample looks at the settings again: a next track is
        // opened up to a minute ahead and must not start on the ones it was
        // opened with.
        until_param_check: 0,
    };
    wrapped.recompute_coeffs();
    wrapped
}

fn norm_factor(params: &DspParams, track_gain_db: Option<f32>) -> f32 {
    match track_gain_db {
        Some(gain) if params.normalization_enabled => {
            // `gain` is the server's NormalizationGain — untrusted input. A
            // NaN or a wild value here would multiply every sample: NaN
            // propagates through the whole chain, and a large positive gain is
            // full-scale noise straight into the user's ears. Clamp to the
            // range ReplayGain actually uses.
            if !gain.is_finite() {
                return 1.0;
            }
            let gain = gain.clamp(-NORM_LIMIT_DB, NORM_LIMIT_DB);
            let preamp = params.preamp_db.clamp(-PREAMP_LIMIT_DB, PREAMP_LIMIT_DB);
            10f32.powf((gain + preamp) / 20.0)
        }
        _ => 1.0,
    }
}

impl<S> DspSource<S>
where
    S: Source,
{
    /// Called when the inner source's current span is exhausted: pick up
    /// format changes and re-check params.
    fn begin_span(&mut self) {
        let channels = self.inner.channels();
        let sample_rate = self.inner.sample_rate();
        if channels != self.channels || sample_rate != self.sample_rate {
            self.channels = channels;
            self.sample_rate = sample_rate;
            self.recompute_coeffs();
            self.reset_filter_state();
        }
        self.sync_params();
        self.span_remaining = self.inner.current_span_len();
    }

    fn sync_params(&mut self) {
        if self.dsp.revision.load(Ordering::Acquire) == self.revision {
            return;
        }
        let (params, revision) = self.dsp.snapshot();
        let norm = norm_factor(&params, self.track_gain_db);
        let eq_mix = if params.eq_enabled { 1.0 } else { 0.0 };
        if self.fresh {
            self.norm = Ramp::at(norm);
            self.eq_mix = Ramp::at(eq_mix);
        } else {
            if params.eq_enabled && self.eq_mix.value == 0.0 {
                // The filters stood still while the EQ was off. What they
                // still hold is the tail of long-gone audio, and it would
                // ring out as a thump.
                self.clear_filter_memory();
            }
            let frames = ramp_frames(self.sample_rate);
            self.norm.glide(norm, frames);
            self.eq_mix.glide(eq_mix, frames);
        }
        self.params = params;
        self.revision = revision;
        // Filter state is deliberately kept: pure gain changes must not click.
        self.recompute_coeffs();
    }

    fn recompute_coeffs(&mut self) {
        let sample_rate = self.sample_rate.get() as f32;
        // Bands at or beyond 0.45 * fs are numerically unstable: bypass them.
        let max_center = 0.45 * sample_rate;
        for ((coeff, freq), gain) in self
            .coeffs
            .iter_mut()
            .zip(EQ_BANDS_HZ)
            .zip(self.params.eq_gains_db)
        {
            *coeff = (freq < max_center).then(|| {
                let gain = gain.clamp(-EQ_GAIN_LIMIT_DB, EQ_GAIN_LIMIT_DB);
                BiquadCoeffs::peaking(sample_rate, freq, EQ_BAND_Q, gain)
            });
        }
    }

    fn reset_filter_state(&mut self) {
        self.filter_state.clear();
        self.filter_state
            .resize(self.channels.get() as usize, Default::default());
        self.channel_idx = 0;
    }

    /// Zero the filters' memory but keep the position in the frame: a
    /// settings change can land between two channels of one frame.
    fn clear_filter_memory(&mut self) {
        for state in &mut self.filter_state {
            *state = Default::default();
        }
    }
}

impl<S> Iterator for DspSource<S>
where
    S: Source,
{
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        if self.span_remaining == Some(0) {
            self.begin_span();
        }
        if self.until_param_check == 0 {
            self.until_param_check = PARAM_CHECK_INTERVAL;
            self.sync_params();
        }

        let sample = self.inner.next()?;
        self.fresh = false;
        if let Some(remaining) = self.span_remaining.as_mut() {
            *remaining = remaining.saturating_sub(1);
        }
        self.until_param_check -= 1;
        if self.channel_idx == 0 {
            // One step per frame, so all channels of a frame get one gain.
            self.norm.advance();
            self.eq_mix.advance();
        }

        let mut out = sample * self.norm.value;
        // Sanitize BEFORE the biquad, not after: the filter is recursive, so a
        // single non-finite sample would be written into `state.z1`/`z2` and
        // every later sample would come back out as NaN. A guard on the return
        // value alone cannot undo that. (f32 PCM from the server can carry
        // NaN/Inf verbatim.)
        if !out.is_finite() {
            out = 0.0;
        }
        let mix = self.eq_mix.value;
        if mix > 0.0 {
            let dry = out;
            let state = &mut self.filter_state[self.channel_idx];
            for (coeff, state) in self.coeffs.iter().zip(state.iter_mut()) {
                if let Some(coeff) = coeff {
                    out = coeff.process(state, out);
                }
            }
            if mix < 1.0 {
                // The EQ is being switched on or off: blend the two.
                out = dry + (out - dry) * mix;
            }
        }
        self.channel_idx += 1;
        if self.channel_idx >= self.channels.get() as usize {
            self.channel_idx = 0;
        }
        // The filters can still overflow on absurd input; never hand on a
        // non-finite sample.
        Some(if out.is_finite() { out } else { 0.0 })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for DspSource<S>
where
    S: Source,
{
    #[inline]
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    #[inline]
    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    #[inline]
    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)?;
        // Stale IIR state would smear pre-seek audio into the new position.
        self.begin_span();
        self.reset_filter_state();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    fn buffer(channels: u16, sample_rate: u32, data: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(
            NonZero::new(channels).unwrap(),
            NonZero::new(sample_rate).unwrap(),
            data,
        )
    }

    fn dsp(params: DspParams) -> Arc<AudioDsp> {
        Arc::new(AudioDsp::new(params))
    }

    fn all_off() -> DspParams {
        DspParams {
            eq_enabled: false,
            normalization_enabled: false,
            ..DspParams::default()
        }
    }

    fn sine(freq: f32, sample_rate: u32, amplitude: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                amplitude
                    * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin()
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn all_off_is_bit_identical_passthrough() {
        let input = vec![0.5, -0.999, 0.0, -0.0, 1.0, -1.0, 1e-30, 0.123_456_79];
        // Non-default gains/preamp prove they are ignored while disabled.
        let params = DspParams {
            eq_gains_db: [6.0; BAND_COUNT],
            preamp_db: 10.0,
            ..all_off()
        };
        let out: Vec<f32> =
            wrap(buffer(2, 44100, input.clone()), dsp(params), Some(-8.5)).collect();
        assert_eq!(out.len(), input.len());
        for (a, b) in input.iter().zip(&out) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn flat_eq_is_bit_identical_passthrough() {
        let input = vec![0.5, -0.999, 0.25, 1.0, -1.0, 0.001];
        let params = DspParams {
            eq_enabled: true,
            normalization_enabled: false,
            ..DspParams::default()
        };
        let out: Vec<f32> = wrap(buffer(2, 44100, input.clone()), dsp(params), None).collect();
        for (a, b) in input.iter().zip(&out) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn normalization_gain_halves_amplitude() {
        let params = DspParams::default();
        let out: Vec<f32> =
            wrap(buffer(1, 44100, vec![0.8; 64]), dsp(params), Some(-6.02)).collect();
        for s in out {
            assert!((s - 0.4).abs() < 1e-3, "expected ~0.4, got {s}");
        }
    }

    #[test]
    fn preamp_scales_normalization() {
        // gain -6.02 + preamp +6.02 => unity.
        let params = DspParams {
            preamp_db: 6.02,
            ..DspParams::default()
        };
        let out: Vec<f32> =
            wrap(buffer(1, 44100, vec![0.5; 64]), dsp(params), Some(-6.02)).collect();
        for s in out {
            assert!((s - 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn no_track_gain_means_no_gain_at_all() {
        // Preamp alone must not apply when the track has no gain.
        let params = DspParams {
            preamp_db: 15.0,
            ..DspParams::default()
        };
        let input = vec![0.5, -0.25, 0.75];
        let out: Vec<f32> = wrap(buffer(1, 44100, input.clone()), dsp(params), None).collect();
        for (a, b) in input.iter().zip(&out) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn nan_input_does_not_poison_the_eq_state() {
        // A single non-finite sample used to be written into the recursive
        // biquad state, after which every later sample came out NaN.
        let params = DspParams {
            eq_enabled: true,
            eq_gains_db: [6.0; BAND_COUNT],
            normalization_enabled: false,
            ..DspParams::default()
        };
        let mut samples = vec![0.5f32; 8];
        samples[2] = f32::NAN;
        samples[3] = f32::INFINITY;
        let src = buffer(1, 48_000, samples);
        let out: Vec<f32> = wrap(src, dsp(params), None).collect();

        assert!(
            out.iter().all(|s| s.is_finite()),
            "non-finite sample escaped the DSP chain: {out:?}"
        );
        // The samples AFTER the bad ones must still carry signal — that is
        // what proves the filter state survived.
        assert!(
            out[5..].iter().any(|s| s.abs() > 1e-6),
            "filter went silent after a NaN, state was poisoned: {out:?}"
        );
    }

    #[test]
    fn eq_boost_at_1khz_increases_rms() {
        let sample_rate = 44100;
        let input = sine(1000.0, sample_rate, 0.25, sample_rate as usize);
        let flat = DspParams {
            eq_enabled: true,
            normalization_enabled: false,
            ..DspParams::default()
        };
        let mut boosted = flat.clone();
        boosted.eq_gains_db[5] = 12.0; // 1 kHz band

        let flat_out: Vec<f32> =
            wrap(buffer(1, sample_rate, input.clone()), dsp(flat), None).collect();
        let boost_out: Vec<f32> = wrap(buffer(1, sample_rate, input), dsp(boosted), None).collect();

        // Skip the first half to let the filter settle. +12 dB at center ~= 3.98x.
        let flat_rms = rms(&flat_out[flat_out.len() / 2..]);
        let boost_rms = rms(&boost_out[boost_out.len() / 2..]);
        let ratio = boost_rms / flat_rms;
        assert!((3.0..4.5).contains(&ratio), "gain ratio was {ratio}");
    }

    #[test]
    fn levels_past_full_scale_are_left_to_the_output_limiter() {
        // Clipping here would flatten every peak of a boosted track before
        // the volume; the output limiter turns the mix down instead.
        let out: Vec<f32> = wrap(
            buffer(1, 44100, vec![1.5, -3.0, 0.25]),
            dsp(all_off()),
            None,
        )
        .collect();
        assert_eq!(out, vec![1.5, -3.0, 0.25]);

        let boosted: Vec<f32> = wrap(
            buffer(1, 44100, vec![0.9; 8]),
            dsp(DspParams::default()),
            Some(12.0),
        )
        .collect();
        assert!(boosted.iter().all(|s| (s - 0.9 * 3.981).abs() < 1e-2));
    }

    #[test]
    fn a_source_opened_ahead_starts_on_the_latest_settings() {
        // The next track is opened well before it plays; settings changed
        // in between apply from its first sample, not after 1024 of them.
        let shared = dsp(DspParams::default());
        let mut source = wrap(buffer(1, 44100, vec![0.25; 64]), shared.clone(), Some(0.0));
        shared.update(DspParams {
            preamp_db: 6.02,
            ..DspParams::default()
        });
        let first = source.next().unwrap();
        assert!((first - 0.5).abs() < 1e-3, "first sample {first}");
    }

    #[test]
    fn a_gain_change_glides_instead_of_stepping() {
        let shared = dsp(all_off());
        let mut source = wrap(
            buffer(1, 44100, vec![0.5; 8192]),
            shared.clone(),
            Some(-6.02),
        );
        for _ in 0..2048 {
            assert_eq!(source.next(), Some(0.5));
        }
        // Normalization on: the track's -6 dB halve the level.
        shared.update(DspParams::default());
        let rest: Vec<f32> = source.collect();
        let largest_step = rest
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(largest_step < 0.01, "level stepped by {largest_step}");
        assert!((rest[rest.len() - 1] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn switching_the_eq_glides_both_ways() {
        // +12 dB at 1 kHz on a 1 kHz tone: on is four times off.
        let sample_rate = 44100;
        let mut params = DspParams {
            eq_enabled: true,
            normalization_enabled: false,
            ..DspParams::default()
        };
        params.eq_gains_db[5] = 12.0;
        let shared = dsp(params.clone());
        let input = sine(1000.0, sample_rate, 0.1, sample_rate as usize * 2);
        let mut source = wrap(buffer(1, sample_rate, input), shared.clone(), None);
        let _: Vec<f32> = source.by_ref().take(22_050).collect();
        shared.update(DspParams {
            eq_enabled: false,
            ..params.clone()
        });
        let off: Vec<f32> = source.by_ref().take(22_050).collect();
        shared.update(params);
        let on: Vec<f32> = source.collect();
        // Peak level per millisecond: it may change gradually, never jump.
        for part in [&off, &on] {
            let peaks: Vec<f32> = part
                .chunks(44)
                .map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs())))
                .collect();
            for w in peaks.windows(2) {
                let ratio = w[1].min(w[0]) / w[1].max(w[0]);
                assert!(ratio > 0.75, "level jumped {} -> {}", w[0], w[1]);
            }
        }
    }

    #[test]
    fn reenabling_the_eq_starts_from_clean_filters() {
        // Bass boost on a loud 40 Hz tone fills the filters; switched off,
        // they stand still. Switched on again over silence, nothing may ring
        // out of them.
        let sample_rate = 44100;
        let params = DspParams {
            eq_enabled: true,
            normalization_enabled: false,
            eq_gains_db: [12.0, 12.0, 6.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ..DspParams::default()
        };
        let mut input = sine(40.0, sample_rate, 0.5, 8192);
        input.extend(std::iter::repeat_n(0.0, 16_384));
        let shared = dsp(params.clone());
        let mut source = wrap(buffer(1, sample_rate, input), shared.clone(), None);
        let _: Vec<f32> = source.by_ref().take(8192).collect();
        shared.update(DspParams {
            eq_enabled: false,
            ..params.clone()
        });
        // Long enough for the switch to land and the glide to finish.
        let _: Vec<f32> = source.by_ref().take(4096).collect();
        shared.update(params);
        let after: Vec<f32> = source.collect();
        assert!(
            after.iter().all(|s| *s == 0.0),
            "stale filter state rang out: {:?}",
            after.iter().find(|s| **s != 0.0)
        );
    }

    #[test]
    fn param_update_applies_within_check_interval() {
        let shared = dsp(DspParams::default());
        let mut source = wrap(buffer(1, 44100, vec![0.4; 8192]), shared.clone(), Some(0.0));
        for _ in 0..2048 {
            assert_eq!(source.next(), Some(0.4));
        }
        shared.update(DspParams {
            preamp_db: 6.02,
            ..DspParams::default()
        });
        let rest: Vec<f32> = source.collect();
        let late = rest[PARAM_CHECK_INTERVAL as usize + 16];
        assert!((late - 0.8).abs() < 1e-3, "expected ~0.8, got {late}");
    }

    #[test]
    fn filter_state_is_per_channel() {
        // Left carries a sine, right is silent. Shared state would bleed.
        let sample_rate = 44100;
        let left = sine(1000.0, sample_rate, 0.25, 4096);
        let interleaved: Vec<f32> = left.iter().flat_map(|l| [*l, 0.0]).collect();
        let mut params = DspParams {
            eq_enabled: true,
            normalization_enabled: false,
            ..DspParams::default()
        };
        params.eq_gains_db = [6.0; BAND_COUNT];
        let out: Vec<f32> = wrap(buffer(2, sample_rate, interleaved), dsp(params), None).collect();
        assert!(out.iter().skip(1).step_by(2).all(|r| *r == 0.0));
        assert!(rms(&out.iter().step_by(2).copied().collect::<Vec<_>>()) > 0.1);
    }

    #[test]
    fn bands_at_nyquist_are_bypassed() {
        // At 8 kHz the 4/8/16 kHz bands exceed 0.45 * fs and must not blow up.
        let sample_rate = 8000;
        let params = DspParams {
            eq_enabled: true,
            normalization_enabled: false,
            eq_gains_db: [12.0; BAND_COUNT],
            ..DspParams::default()
        };
        let input = sine(1000.0, sample_rate, 0.1, 8000);
        let out: Vec<f32> = wrap(buffer(1, sample_rate, input), dsp(params), None).collect();
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(rms(&out[4000..]) > 0.0);
    }

    #[test]
    fn filter_state_settles_to_exact_zero_after_silence() {
        let coeffs = BiquadCoeffs::peaking(44_100.0, 60.0, 1.0, 12.0);
        let mut state = BiquadState::default();
        coeffs.process(&mut state, 1.0);
        for _ in 0..200_000 {
            coeffs.process(&mut state, 0.0);
        }
        assert_eq!((state.z1, state.z2), (0.0, 0.0));
    }

    #[test]
    fn try_seek_forwards_to_inner() {
        let data: Vec<f32> = (0..500).map(|i| i as f32 / 1000.0).collect();
        let mut source = wrap(buffer(1, 100, data), dsp(all_off()), None);
        for _ in 0..5 {
            source.next();
        }
        source.try_seek(Duration::from_secs(1)).unwrap();
        assert_eq!(source.next(), Some(0.1)); // sample index 100
    }

    #[test]
    fn update_bumps_params() {
        let shared = AudioDsp::new(DspParams::default());
        let new = DspParams {
            eq_enabled: true,
            ..DspParams::default()
        };
        shared.update(new.clone());
        assert_eq!(shared.params(), new);
    }
}
