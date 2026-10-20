//! Output limiter: the last stage before the device. Upstream of it the mix
//! may go past full scale — a positive normalization gain or preamp, an EQ
//! boost, two tracks overlapping in a crossfade or a beat transition — and
//! the device would flatten every such peak. This stage turns the whole mix
//! down just enough instead, and back up once the peaks have passed.
//!
//! It reads one short block ahead: the gain glides down over that block
//! before a peak instead of dropping on it. Below full scale the mix passes
//! bit for bit, one block later. It sits behind the per-player volume, so a
//! lower volume leaves more room and the limiter has less to do.
//!
//! Runs on the audio callback: every buffer is allocated in [`wrap`], and the
//! inner source has to keep one format (the device's mix format).

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use std::time::Duration;

/// The mix never leaves the device above this.
const CEILING: f32 = 1.0;
/// How far ahead the gain is planned; also how long it takes to come down.
const LOOKAHEAD: Duration = Duration::from_millis(2);
/// The gain stays down this long after the last peak, so a sustained loud
/// passage keeps one gain instead of pumping between the peaks of each wave
/// (a 50 Hz wave peaks every 10 ms).
const HOLD: Duration = Duration::from_millis(20);
/// Time constant of the recovery once the peaks are over.
const RELEASE: Duration = Duration::from_millis(150);
/// A gain this close to 1 is 1, so the limiter returns to an exact
/// passthrough after a loud passage (1e-6 is -0.00001 dB).
const UNITY_SNAP: f32 = 1e-6;

pub struct Limiter<S> {
    inner: S,
    channels: usize,
    sample_rate: SampleRate,
    /// Frames per block: the look-ahead.
    block: usize,
    /// Two blocks of interleaved samples. `playing` is the half being
    /// handed out, the other one holds the frames read ahead.
    samples: Vec<f32>,
    /// Per frame of both halves: the gain that keeps it under the ceiling.
    needed: Vec<f32>,
    /// Per frame of the playing half: the gain it is played with.
    gains: Vec<f32>,
    /// Frames each half holds; less than `block` only once the inner source
    /// has ended.
    filled: [usize; 2],
    playing: usize,
    /// Next sample of the playing half.
    pos: usize,
    primed: bool,
    ended: bool,
    /// How far below 1 the gain of the last frame was. Kept as the distance
    /// rather than the gain: near 1, a release step is smaller than what an
    /// f32 can still tell apart from 1, and the gain would stop short.
    reduction: f32,
    /// Frames left before the gain may rise again.
    hold_left: u32,
    hold_frames: u32,
    /// How far the gain may fall per frame: all the way within one block.
    attack_step: f32,
    /// Per frame, what is left of the reduction while it recovers.
    release: f32,
}

/// Wrap the mix in front of the device.
pub fn wrap<S: Source>(inner: S) -> Limiter<S> {
    let channels = inner.channels().get() as usize;
    let sample_rate = inner.sample_rate();
    let rate = sample_rate.get() as f32;
    let block = ((rate * LOOKAHEAD.as_secs_f32()).ceil() as usize).max(1);
    Limiter {
        inner,
        channels,
        sample_rate,
        block,
        samples: vec![0.0; 2 * block * channels],
        needed: vec![1.0; 2 * block],
        gains: vec![1.0; block],
        filled: [0, 0],
        playing: 0,
        pos: 0,
        primed: false,
        ended: false,
        reduction: 0.0,
        hold_left: 0,
        hold_frames: (rate * HOLD.as_secs_f32()) as u32,
        attack_step: 1.0 / block as f32,
        release: (-1.0 / (rate * RELEASE.as_secs_f32())).exp(),
    }
}

impl<S: Source> Limiter<S> {
    /// Read the next block of the mix into `half`, with the gain each frame
    /// needs.
    fn read(&mut self, half: usize) {
        let ch = self.channels;
        let start = half * self.block;
        let mut frames = 0;
        while frames < self.block && !self.ended {
            let mut peak = 0.0f32;
            for c in 0..ch {
                let sample = match self.inner.next() {
                    // A NaN would pass every comparison below and reach the
                    // device; nothing upstream should produce one, but this
                    // is the last place to catch it.
                    Some(s) if s.is_finite() => s,
                    Some(_) => 0.0,
                    None => {
                        self.ended = true;
                        0.0
                    }
                };
                self.samples[(start + frames) * ch + c] = sample;
                peak = peak.max(sample.abs());
            }
            if self.ended {
                break;
            }
            self.needed[start + frames] = if peak > CEILING { CEILING / peak } else { 1.0 };
            frames += 1;
        }
        self.filled[half] = frames;
        for slot in &mut self.needed[start + frames..start + self.block] {
            *slot = 1.0;
        }
    }

    /// Gains for the playing half. Backwards over both halves, a gain may
    /// rise by at most one attack step per frame, so it is already on its
    /// way down where a peak is still ahead; forwards, it holds after a peak
    /// and then recovers slowly. Never above what a frame needs.
    fn plan(&mut self) {
        let block = self.block;
        let mut ahead = 1.0f32;
        for i in (0..2 * block).rev() {
            let half = (self.playing + i / block) % 2;
            ahead = (ahead + self.attack_step).min(self.needed[half * block + i % block]);
            if i < block {
                self.gains[i] = ahead;
            }
        }
        for i in 0..block {
            let planned = 1.0 - self.gains[i];
            let released = if self.hold_left > 0 {
                self.hold_left -= 1;
                self.reduction
            } else {
                self.reduction * self.release
            };
            let reduction = if planned >= released {
                // A peak here or just ahead sets the gain: hold it from now.
                self.hold_left = self.hold_frames;
                planned
            } else {
                released
            };
            self.reduction = if reduction < UNITY_SNAP {
                0.0
            } else {
                reduction
            };
            self.gains[i] = 1.0 - self.reduction;
        }
    }

    /// The playing half is done: the read-ahead half plays next, and the
    /// freed one reads ahead.
    fn advance(&mut self) {
        if self.primed {
            self.playing ^= 1;
            self.read(self.playing ^ 1);
        } else {
            self.primed = true;
            self.read(0);
            self.read(1);
        }
        self.pos = 0;
        self.plan();
    }
}

impl<S: Source> Iterator for Limiter<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if !self.primed || self.pos >= self.block * self.channels {
            self.advance();
        }
        if self.pos >= self.filled[self.playing] * self.channels {
            return None;
        }
        let frame = self.pos / self.channels;
        let sample =
            self.samples[self.playing * self.block * self.channels + self.pos] * self.gains[frame];
        self.pos += 1;
        // The gain keeps every frame at or under the ceiling; the clamp only
        // catches the last rounding bit of `CEILING / peak`.
        Some(sample.clamp(-CEILING, CEILING))
    }
}

impl<S: Source> Source for Limiter<S> {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(self.channels as u16).expect("channels")
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, _pos: Duration) -> Result<(), SeekError> {
        Err(SeekError::NotSupported {
            underlying_source: "Limiter",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    const RATE: u32 = 48_000;

    fn buffer(channels: u16, samples: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(
            ChannelCount::new(channels).unwrap(),
            SampleRate::new(RATE).unwrap(),
            samples,
        )
    }

    /// Stereo sine, both channels alike.
    fn sine(freq: f32, amplitude: f32, seconds: f32) -> Vec<f32> {
        let frames = (seconds * RATE as f32) as usize;
        (0..frames)
            .flat_map(|i| {
                let s = amplitude * (std::f32::consts::TAU * freq * i as f32 / RATE as f32).sin();
                [s, s]
            })
            .collect()
    }

    #[test]
    fn below_full_scale_it_is_bit_identical() {
        let input: Vec<f32> = (0..20_000)
            .map(|i| ((i as f32 * 0.37).sin() * 0.999).clamp(-1.0, 1.0))
            .chain([1.0, -1.0, 0.0, -0.0])
            .collect();
        let out: Vec<f32> = wrap(buffer(2, input.clone())).collect();
        assert_eq!(out.len(), input.len());
        for (a, b) in input.iter().zip(&out) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn nothing_leaves_above_full_scale() {
        // +6 dB over full scale, and a lone spike well beyond it.
        let mut input = sine(440.0, 2.0, 0.5);
        input[30_001] = 40.0;
        let out: Vec<f32> = wrap(buffer(2, input)).collect();
        assert!(
            out.iter().all(|s| s.abs() <= CEILING),
            "a sample left above the ceiling"
        );
    }

    #[test]
    fn a_loud_passage_is_turned_down_not_flattened() {
        // A sustained 100 Hz sine 6 dB over full scale. Clipping would play
        // its peaks flat (gain 0.5 at the top, 1.0 near the zero crossings);
        // the limiter plays the whole wave at one gain.
        let input = sine(100.0, 2.0, 1.0);
        let out: Vec<f32> = wrap(buffer(2, input.clone())).collect();
        let settled = RATE as usize; // from 0.5 s on, in samples (stereo)
        for (x, y) in input[settled..].iter().zip(&out[settled..]) {
            if x.abs() > 0.2 {
                let gain = y / x;
                assert!((0.48..=0.51).contains(&gain), "gain {gain} at {x}");
            }
        }
    }

    #[test]
    fn the_gain_comes_down_before_a_peak() {
        // Quiet, then one loud frame: the frames just before it are already
        // turned down, so the peak is not a step from full gain.
        let mut input = vec![0.5f32; 2 * 4_000];
        input[2 * 2_000] = 4.0;
        input[2 * 2_000 + 1] = 4.0;
        let out: Vec<f32> = wrap(buffer(2, input)).collect();
        assert!(
            (out[2 * 2_000] - 1.0).abs() < 1e-6,
            "the peak lands on the ceiling"
        );
        let before = out[2 * 1_990] / 0.5;
        assert!(before < 1.0, "no ramp before the peak: gain {before}");
        assert_eq!(out[0], 0.5, "far ahead of the peak nothing changes");
    }

    #[test]
    fn it_recovers_to_an_exact_passthrough() {
        let mut input = sine(200.0, 3.0, 0.2);
        let quiet = sine(200.0, 0.5, 4.0);
        input.extend_from_slice(&quiet);
        let out: Vec<f32> = wrap(buffer(2, input.clone())).collect();
        // Seconds after the loud part, the quiet one plays untouched.
        let tail = input.len() - RATE as usize;
        for (a, b) in input[tail..].iter().zip(&out[tail..]) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn overlapping_tracks_are_limited_as_one_mix() {
        // A beat transition can hold the outgoing track at half gain while
        // the incoming one is already at full gain: their sum is 1.5x.
        let (mixer, mixed) = rodio::mixer::mixer(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(RATE).unwrap(),
        );
        let a: Vec<f32> = sine(120.0, 0.5, 0.5);
        let b: Vec<f32> = sine(120.0, 1.0, 0.5);
        mixer.add(buffer(2, a));
        mixer.add(buffer(2, b));
        let out: Vec<f32> = wrap(mixed).take(RATE as usize).collect();
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= CEILING, "peak {peak}");
        assert!(peak > 0.95, "turned down, not muted: {peak}");
    }

    #[test]
    fn a_nan_leaves_as_silence() {
        let out: Vec<f32> = wrap(buffer(1, vec![0.25, f32::NAN, f32::INFINITY, 0.5])).collect();
        assert_eq!(out, vec![0.25, 0.0, 0.0, 0.5]);
    }
}
