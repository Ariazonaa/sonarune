//! Playback speed: one time-stretch stage (Signalsmith Stretch) between the
//! player's own mixer and the device. Everything upstream — positions,
//! seeks, fades, crossfades, beat plans — stays in track time; only the
//! device consumes that time faster or slower. At 1× the stage is bypassed
//! sample for sample, so normal playback is untouched.

use crate::store::{keys, Store};
use rodio::Source;
use serde::{Deserialize, Serialize};
use signalsmith_stretch::Stretch;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;

/// Output frames per stretch call.
const BLOCK: usize = 512;
/// Bypass: look at the shared settings this often (frames).
const PARAM_CHECK_INTERVAL: u32 = 512;
/// Bypass fade-out before the stretch takes over, so the hand-over is a
/// short dip instead of a click.
const HANDOVER_FADE_FRAMES: u32 = 256;

/// A speed as the player uses it: finite, in range, on a 0.01 grid.
pub fn clamp_speed(speed: f32) -> f32 {
    if !speed.is_finite() {
        return 1.0;
    }
    ((speed * 100.0).round() / 100.0).clamp(MIN_SPEED, MAX_SPEED)
}

pub fn is_unity(speed: f32) -> bool {
    (speed - 1.0).abs() < 0.005
}

/// The persisted choice (store key `playback_speed`). Kept apart from the
/// playback settings: the player bar changes it, and an open settings page
/// saving its older copy of those must not undo that.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpeedSetting {
    pub speed: f32,
    /// Keep the pitch (time-stretch); off, the pitch follows the speed
    /// like a record played faster.
    pub preserve_pitch: bool,
}

impl Default for SpeedSetting {
    fn default() -> Self {
        Self {
            speed: 1.0,
            preserve_pitch: true,
        }
    }
}

impl SpeedSetting {
    pub fn clamped(self) -> Self {
        Self {
            speed: clamp_speed(self.speed),
            ..self
        }
    }

    pub fn load(store: &Store) -> Self {
        store
            .get(keys::PLAYBACK_SPEED)
            .and_then(|json| serde_json::from_str::<Self>(&json).ok())
            .unwrap_or_default()
            .clamped()
    }

    pub fn save(&self, store: &Store) {
        if let Ok(json) = serde_json::to_string(self) {
            let _ = store.set(keys::PLAYBACK_SPEED, &json);
        }
    }
}

struct TempoShared {
    /// f32 bits.
    speed: AtomicU32,
    preserve_pitch: AtomicBool,
}

/// Steers the stage from the player thread; survives output rebuilds.
#[derive(Clone)]
pub struct TempoHandle {
    shared: Arc<TempoShared>,
}

impl TempoHandle {
    pub fn new(speed: f32, preserve_pitch: bool) -> Self {
        let handle = Self {
            shared: Arc::new(TempoShared {
                speed: AtomicU32::new(1f32.to_bits()),
                preserve_pitch: AtomicBool::new(true),
            }),
        };
        handle.set(speed, preserve_pitch);
        handle
    }

    pub fn set(&self, speed: f32, preserve_pitch: bool) {
        self.shared
            .preserve_pitch
            .store(preserve_pitch, Ordering::Relaxed);
        self.shared
            .speed
            .store(clamp_speed(speed).to_bits(), Ordering::Relaxed);
    }

    pub fn speed(&self) -> f32 {
        f32::from_bits(self.shared.speed.load(Ordering::Relaxed))
    }

    /// Track time `media` takes this long on the clock.
    pub fn wall(&self, media: Duration) -> Duration {
        media.div_f32(self.speed())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Bypass,
    /// Bypass, fading out over the remaining frames before the stretch
    /// takes over.
    HandOver(u32),
    /// Bypass, fading in over the remaining frames: after the stretch handed
    /// back (its last output tapers to silence), or a hand-over called off
    /// once the bypass had faded out.
    FadeIn(u32),
    Stretch,
}

pub struct TempoSource<S> {
    inner: S,
    shared: Arc<TempoShared>,
    channels: usize,
    sample_rate: rodio::SampleRate,
    /// Built when the output opens, so the audio callback never allocates
    /// a stretcher.
    stretch: Stretch,
    mode: Mode,
    /// What the stretch runs with right now.
    speed: f32,
    preserve_pitch: bool,
    /// Fractional input frames carried to the next block, so the ratio is
    /// exact over time.
    carry: f64,
    input: Vec<f32>,
    output: Vec<f32>,
    out_pos: usize,
    out_len: usize,
    /// Bypass: index inside the current frame, and frames to the next look
    /// at the settings.
    channel_idx: usize,
    until_check: u32,
}

/// Wrap the player's mixer output. The source never ends: a mixer that runs
/// dry (nothing playing) reads as silence, so the stage stays on the device.
pub fn wrap<S: Source>(inner: S, handle: &TempoHandle) -> TempoSource<S> {
    let channels = inner.channels().get() as usize;
    let sample_rate = inner.sample_rate();
    let stretch = Stretch::preset_default(channels as u32, sample_rate.get());
    let in_lat = stretch.input_latency();
    let out_lat = stretch.output_latency();
    // The biggest single pull: a block at top speed, or the latency fed
    // when starting and when handing back to the bypass.
    let in_frames = (BLOCK as f32 * MAX_SPEED).ceil() as usize + in_lat + 2;
    let out_frames = BLOCK.max(out_lat + (in_lat as f32 / MIN_SPEED).ceil() as usize + 2);
    TempoSource {
        inner,
        shared: handle.shared.clone(),
        channels,
        sample_rate,
        stretch,
        mode: Mode::Bypass,
        speed: 1.0,
        preserve_pitch: true,
        carry: 0.0,
        input: vec![0.0; in_frames * channels],
        output: vec![0.0; out_frames * channels],
        out_pos: 0,
        out_len: 0,
        channel_idx: 0,
        until_check: 0,
    }
}

impl<S: Source> TempoSource<S> {
    fn wanted(&self) -> (f32, bool) {
        (
            f32::from_bits(self.shared.speed.load(Ordering::Relaxed)),
            self.shared.preserve_pitch.load(Ordering::Relaxed),
        )
    }

    /// Pull `frames` frames from the mixer into `input`.
    fn pull(&mut self, frames: usize) -> usize {
        let samples = frames * self.channels;
        for slot in &mut self.input[..samples] {
            *slot = self.inner.next().unwrap_or(0.0);
        }
        samples
    }

    fn apply_pitch(&mut self) {
        let factor = if self.preserve_pitch { 1.0 } else { self.speed };
        self.stretch.set_transpose_factor(factor, None);
    }

    /// Start stretching from here: the stretch's processing time is placed
    /// on the next input, so only its output latency is pre-roll.
    fn start_stretch(&mut self, speed: f32, preserve_pitch: bool) {
        self.speed = speed;
        self.preserve_pitch = preserve_pitch;
        self.stretch.reset();
        self.apply_pitch();
        let frames = self.stretch.input_latency();
        let samples = self.pull(frames);
        self.stretch.seek(&self.input[..samples], f64::from(speed));
        self.carry = 0.0;
        self.mode = Mode::Stretch;
    }

    /// Stretch the next block (or, back at 1×, play out what the stretch
    /// still holds and return to the bypass).
    fn fill(&mut self) {
        let (speed, preserve_pitch) = self.wanted();
        let ch = self.channels;
        if is_unity(speed) {
            // Move the processing time up to everything pulled so far, at
            // the old speed, and read out what the stretch still holds. The
            // library's recipe for that is `input_latency` frames of silence
            // and then a flush: real input there would be taken and never
            // played, as the processing time only reaches the input before
            // it. So the bypass goes on with the very next input frame, and
            // fades in, as the flushed output tapers to silence.
            let in_lat = self.stretch.input_latency();
            let samples = in_lat * ch;
            self.input[..samples].fill(0.0);
            let tail = (in_lat as f64 / f64::from(self.speed)).round() as usize;
            let out_lat = self.stretch.output_latency();
            self.stretch
                .process(&self.input[..samples], &mut self.output[..tail * ch]);
            self.stretch
                .flush(&mut self.output[tail * ch..(tail + out_lat) * ch]);
            self.out_len = (tail + out_lat) * ch;
            self.out_pos = 0;
            self.mode = Mode::FadeIn(HANDOVER_FADE_FRAMES);
            self.speed = 1.0;
            self.channel_idx = 0;
            self.until_check = PARAM_CHECK_INTERVAL;
            return;
        }
        if speed != self.speed || preserve_pitch != self.preserve_pitch {
            self.speed = speed;
            self.preserve_pitch = preserve_pitch;
            self.apply_pitch();
        }
        let wanted = BLOCK as f64 * f64::from(self.speed) + self.carry;
        let frames = wanted.floor() as usize;
        self.carry = wanted - frames as f64;
        let samples = self.pull(frames);
        self.stretch
            .process(&self.input[..samples], &mut self.output[..BLOCK * ch]);
        self.out_len = BLOCK * ch;
        self.out_pos = 0;
    }
}

impl<S: Source> Iterator for TempoSource<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.out_pos < self.out_len {
            let sample = self.output[self.out_pos];
            self.out_pos += 1;
            return Some(sample);
        }
        if self.mode == Mode::Stretch {
            self.fill();
            return self.next();
        }
        if self.channel_idx == 0 {
            match self.mode {
                Mode::HandOver(0) => {
                    let (speed, preserve_pitch) = self.wanted();
                    if is_unity(speed) {
                        // Called off: the bypass is at silence by now, so it
                        // fades back in rather than jumping to full level.
                        self.mode = Mode::FadeIn(HANDOVER_FADE_FRAMES);
                    } else {
                        self.start_stretch(speed, preserve_pitch);
                        self.fill();
                        return self.next();
                    }
                }
                Mode::HandOver(left) => self.mode = Mode::HandOver(left - 1),
                Mode::FadeIn(left) => {
                    self.mode = if left > 1 {
                        Mode::FadeIn(left - 1)
                    } else {
                        Mode::Bypass
                    };
                }
                _ => {
                    if self.until_check == 0 {
                        self.until_check = PARAM_CHECK_INTERVAL;
                        if !is_unity(self.wanted().0) {
                            self.mode = Mode::HandOver(HANDOVER_FADE_FRAMES);
                        }
                    }
                    self.until_check -= 1;
                }
            }
        }
        let gain = match self.mode {
            Mode::HandOver(left) => left as f32 / HANDOVER_FADE_FRAMES as f32,
            Mode::FadeIn(left) => 1.0 - left as f32 / HANDOVER_FADE_FRAMES as f32,
            _ => 1.0,
        };
        self.channel_idx += 1;
        if self.channel_idx >= self.channels {
            self.channel_idx = 0;
        }
        Some(self.inner.next().unwrap_or(0.0) * gain)
    }
}

impl<S: Source> Source for TempoSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        rodio::ChannelCount::new(self.channels as u16).expect("channels")
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(&mut self, _pos: Duration) -> Result<(), rodio::source::SeekError> {
        Err(rodio::source::SeekError::NotSupported {
            underlying_source: "TempoSource",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    const RATE: u32 = 48_000;

    fn tone(seconds: f32, freq: f32) -> SamplesBuffer {
        let n = (seconds * RATE as f32) as usize;
        let samples: Vec<f32> = (0..n)
            .flat_map(|i| {
                let s = (i as f32 * freq * std::f32::consts::TAU / RATE as f32).sin() * 0.5;
                [s, s]
            })
            .collect();
        SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            samples,
        )
    }

    /// Dominant frequency by zero crossings of the left channel.
    fn frequency(samples: &[f32]) -> f32 {
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        let crossings = left
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        crossings as f32 * RATE as f32 / left.len() as f32
    }

    #[test]
    fn clamps_to_the_grid() {
        assert_eq!(clamp_speed(1.234), 1.23);
        assert_eq!(clamp_speed(9.0), MAX_SPEED);
        assert_eq!(clamp_speed(0.1), MIN_SPEED);
        assert_eq!(clamp_speed(f32::NAN), 1.0);
        assert!(is_unity(clamp_speed(1.001)));
    }

    #[test]
    fn unity_is_bit_identical() {
        let handle = TempoHandle::new(1.0, true);
        let input: Vec<f32> = tone(0.5, 440.0).collect();
        let out: Vec<f32> = wrap(tone(0.5, 440.0), &handle).take(input.len()).collect();
        assert_eq!(out, input);
    }

    #[test]
    fn never_ends_on_a_dry_mixer() {
        let handle = TempoHandle::new(1.0, true);
        let out: Vec<f32> = wrap(tone(0.01, 440.0), &handle).take(10_000).collect();
        assert_eq!(out.len(), 10_000);
        assert!(out[2_000..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn faster_keeps_the_pitch_and_shortens_the_track() {
        let handle = TempoHandle::new(1.5, true);
        let out: Vec<f32> = wrap(tone(3.0, 440.0), &handle)
            .take(RATE as usize * 2 * 4)
            .collect();
        let f = frequency(&out[RATE as usize..RATE as usize * 3]);
        assert!((f - 440.0).abs() < 10.0, "pitch kept: {f}");
        // 3 s at 1.5x last 2 s, give or take the stretch's latency.
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let end = left.iter().rposition(|s| s.abs() > 0.05).unwrap();
        let seconds = end as f32 / RATE as f32;
        assert!((seconds - 2.0).abs() < 0.15, "lasted {seconds} s");
    }

    #[test]
    fn without_pitch_correction_the_pitch_follows() {
        let handle = TempoHandle::new(1.5, false);
        let out: Vec<f32> = wrap(tone(4.0, 440.0), &handle)
            .take(RATE as usize * 2 * 2)
            .collect();
        let f = frequency(&out[RATE as usize..]);
        assert!((f - 660.0).abs() < 15.0, "pitch raised: {f}");
    }

    /// Longest run (frames) of near-silence in the left channel.
    fn longest_gap(samples: &[f32]) -> usize {
        let mut longest = 0;
        let mut run = 0;
        for s in samples.iter().step_by(2) {
            if s.abs() < 0.05 {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        longest
    }

    #[test]
    fn switching_on_dips_briefly_and_back_off_not_at_all() {
        let handle = TempoHandle::new(1.0, true);
        let mut tempo = wrap(tone(6.0, 440.0), &handle);
        let _: Vec<f32> = tempo.by_ref().take(RATE as usize).collect();
        handle.set(1.25, true);
        let on: Vec<f32> = tempo.by_ref().take(RATE as usize * 2).collect();
        // A 440 Hz sine crosses zero every ~55 frames; the hand-over dip is
        // the fade plus the stretch's output latency, about 70 ms.
        let gap = longest_gap(&on);
        assert!(gap < RATE as usize / 10, "dip of {gap} frames");
        handle.set(1.0, true);
        let off: Vec<f32> = tempo.by_ref().take(RATE as usize).collect();
        let gap = longest_gap(&off);
        assert!(gap < 60, "gap of {gap} frames going back to 1x");
    }

    /// A source that counts the samples taken from it.
    struct Counted {
        inner: SamplesBuffer,
        taken: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Iterator for Counted {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            self.taken.fetch_add(1, Ordering::Relaxed);
            self.inner.next()
        }
    }

    impl Source for Counted {
        fn current_span_len(&self) -> Option<usize> {
            self.inner.current_span_len()
        }

        fn channels(&self) -> rodio::ChannelCount {
            self.inner.channels()
        }

        fn sample_rate(&self) -> rodio::SampleRate {
            self.inner.sample_rate()
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn back_to_unity_resumes_the_input_where_the_stretch_stopped() {
        let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = Counted {
            inner: tone(4.0, 440.0),
            taken: taken.clone(),
        };
        let handle = TempoHandle::new(1.25, true);
        let mut tempo = wrap(source, &handle);
        // Input frames taken once each output frame was out.
        let mut log = Vec::new();
        let run = |tempo: &mut TempoSource<Counted>, log: &mut Vec<usize>, frames: usize| {
            for _ in 0..frames {
                tempo.next();
                tempo.next();
                log.push(taken.load(Ordering::Relaxed) / 2);
            }
        };
        run(&mut tempo, &mut log, RATE as usize * 2);
        let switch = log.len();
        handle.set(1.0, true);
        run(&mut tempo, &mut log, RATE as usize);

        let lat = Stretch::preset_default(2, RATE).input_latency();
        let out_lat = Stretch::preset_default(2, RATE).output_latency();
        // The stretch starts where one frame takes the start latency plus a
        // block's worth of input at once.
        let start = (1..log.len())
            .find(|&i| log[i] - log[i - 1] > lat)
            .expect("the stretch started");
        // Back in the bypass: one input frame per output frame from here.
        let junction = (switch.max(start + 1)..log.len())
            .find(|&i| log[i] - log[i - 1] == 1 && log[i + 1] - log[i] == 1)
            .expect("the bypass took over again");
        let resumed = log[junction - 1];
        // What the stretch played stands for its input up to the processing
        // time it ended at: its output, less the pre-roll, times the speed.
        let played_through = log[start - 1] + ((junction - start - out_lat) as f64 * 1.25) as usize;
        assert!(
            resumed.abs_diff(played_through) <= 4,
            "the bypass resumed at input frame {resumed}, the stretch played up to \
             {played_through}: {} frames went unplayed",
            resumed as i64 - played_through as i64
        );
    }

    #[test]
    fn a_called_off_hand_over_fades_back_in() {
        // Constant input: the output is the stage's gain.
        let ones = SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            vec![1.0; RATE as usize * 2],
        );
        let handle = TempoHandle::new(1.0, true);
        let mut tempo = wrap(ones, &handle);
        let _: Vec<f32> = tempo.by_ref().take(2_000).collect();
        handle.set(1.05, true);
        // Wait for the hand-over's fade to begin, then change our mind.
        let mut left = Vec::new();
        loop {
            let sample = tempo.next().unwrap();
            tempo.next();
            left.push(sample);
            if sample < 1.0 {
                break;
            }
            assert!(
                left.len() < 4 * PARAM_CHECK_INTERVAL as usize,
                "no hand-over"
            );
        }
        handle.set(1.0, true);
        for _ in 0..2_000 {
            left.push(tempo.next().unwrap());
            tempo.next();
        }
        let step = 1.0 / HANDOVER_FADE_FRAMES as f32 + 1e-6;
        let jump = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jump <= step, "the gain jumped by {jump}");
        assert_eq!(*left.last().unwrap(), 1.0, "back at full level");
    }
}
