//! Gain-ramp wrapper: the audible half of crossfades and click-free
//! pause/resume. The player thread steers a source's gain through a shared
//! handle; the ramp itself runs sample-accurately inside the audio callback.

use rodio::Source;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Look at the shared state this many times a second: a ramp starts at most a
/// millisecond late at any sample rate. (A fixed frame count would be 64 ms at
/// 8 kHz, longer than `pause()` waits on top of its fade.)
const CHECKS_PER_SECOND: u32 = 1000;

/// `from` bit-pattern meaning "start from whatever the current gain is".
const FROM_CURRENT: f32 = f32::NAN;

struct FadeShared {
    /// f32 bits; NaN = ramp from the current gain.
    from: AtomicU32,
    /// f32 bits.
    to: AtomicU32,
    duration_ms: AtomicU32,
    revision: AtomicU64,
}

#[derive(Clone)]
pub struct FadeHandle {
    shared: Arc<FadeShared>,
}

impl FadeHandle {
    /// Ramp from the current gain to `to` over `duration`.
    pub fn ramp_to(&self, to: f32, duration: Duration) {
        self.begin(FROM_CURRENT, to, duration);
    }

    /// Jump to `from`, then ramp to `to` over `duration` (crossfade-in).
    pub fn begin(&self, from: f32, to: f32, duration: Duration) {
        self.shared.from.store(from.to_bits(), Ordering::Relaxed);
        self.shared.to.store(to.to_bits(), Ordering::Relaxed);
        self.shared
            .duration_ms
            .store(duration.as_millis() as u32, Ordering::Relaxed);
        self.shared.revision.fetch_add(1, Ordering::Release);
    }
}

pub struct FadeSource<S> {
    inner: S,
    shared: Arc<FadeShared>,
    gain: f32,
    /// The ramp under way: from where to where, its length in frames and how
    /// far it got. The gain is worked out from the start of the ramp each
    /// frame (in f64) rather than stepped: a long fade at a high rate takes
    /// steps below the precision of an f32 gain, which then drifts or stalls.
    ramp_from: f32,
    target: f32,
    ramp_frames: u64,
    ramp_done: u64,
    /// Gain change per frame of the ramp.
    ramp_step: f64,
    seen_revision: u64,
    channels: u16,
    channel_idx: u16,
    until_check: u32,
}

pub fn wrap<S>(source: S) -> (FadeSource<S>, FadeHandle)
where
    S: Source,
{
    let shared = Arc::new(FadeShared {
        from: AtomicU32::new(1f32.to_bits()),
        to: AtomicU32::new(1f32.to_bits()),
        duration_ms: AtomicU32::new(0),
        revision: AtomicU64::new(0),
    });
    let channels = source.channels().get();
    (
        FadeSource {
            inner: source,
            shared: shared.clone(),
            gain: 1.0,
            ramp_from: 1.0,
            target: 1.0,
            ramp_frames: 0,
            ramp_done: 0,
            ramp_step: 0.0,
            seen_revision: 0,
            channels,
            channel_idx: 0,
            until_check: 0,
        },
        FadeHandle { shared },
    )
}

impl<S: Source> FadeSource<S> {
    fn sync(&mut self) {
        let revision = self.shared.revision.load(Ordering::Acquire);
        if revision == self.seen_revision {
            return;
        }
        self.seen_revision = revision;
        let from = f32::from_bits(self.shared.from.load(Ordering::Relaxed));
        if !from.is_nan() {
            self.gain = from;
        }
        self.target = f32::from_bits(self.shared.to.load(Ordering::Relaxed));
        let duration_ms = self.shared.duration_ms.load(Ordering::Relaxed);
        let frames =
            (u64::from(duration_ms) * u64::from(self.inner.sample_rate().get()) / 1000).max(1);
        self.ramp_from = self.gain;
        self.ramp_frames = frames;
        self.ramp_done = 0;
        self.ramp_step = f64::from(self.target - self.gain) / frames as f64;
    }
}

impl<S: Source> Iterator for FadeSource<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;
        if self.channel_idx == 0 {
            if self.until_check == 0 {
                self.until_check = (self.inner.sample_rate().get() / CHECKS_PER_SECOND).max(1);
                self.sync();
            }
            self.until_check -= 1;
            if self.ramp_done < self.ramp_frames {
                self.ramp_done += 1;
                self.gain = if self.ramp_done == self.ramp_frames {
                    self.target
                } else {
                    (f64::from(self.ramp_from) + self.ramp_step * self.ramp_done as f64) as f32
                };
            }
        }
        self.channel_idx += 1;
        if self.channel_idx >= self.channels {
            self.channel_idx = 0;
        }
        Some(sample * self.gain)
    }
}

impl<S: Source> Source for FadeSource<S> {
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
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn buffer(samples: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(
            rodio::ChannelCount::new(1).unwrap(),
            rodio::SampleRate::new(1000).unwrap(),
            samples,
        )
    }

    #[test]
    fn passthrough_without_ramp() {
        let (source, _handle) = wrap(buffer(vec![0.5; 100]));
        let out: Vec<f32> = source.collect();
        assert_eq!(out, vec![0.5; 100]);
    }

    #[test]
    fn ramp_reaches_target_and_holds() {
        let (source, handle) = wrap(buffer(vec![1.0; 200]));
        // 50 ms at 1 kHz = 50 frames.
        handle.ramp_to(0.0, Duration::from_millis(50));
        let out: Vec<f32> = source.collect();
        assert!(out[0] < 1.0, "ramp starts immediately");
        assert!(out[25] > 0.0 && out[25] < 1.0, "mid-ramp is partial");
        assert_eq!(out[60], 0.0, "target reached");
        assert_eq!(out[199], 0.0, "target held");
    }

    #[test]
    fn begin_from_zero_fades_in() {
        let (source, handle) = wrap(buffer(vec![1.0; 200]));
        handle.begin(0.0, 1.0, Duration::from_millis(50));
        let out: Vec<f32> = source.collect();
        assert!(out[0] < 0.1, "starts silent");
        assert!((out[199] - 1.0).abs() < 1e-6, "ends at full gain");
    }

    /// Endless mono full scale at `rate`: the output is the gain.
    struct Ones(u32);

    impl Iterator for Ones {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            Some(1.0)
        }
    }

    impl Source for Ones {
        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> rodio::ChannelCount {
            rodio::ChannelCount::new(1).unwrap()
        }

        fn sample_rate(&self) -> rodio::SampleRate {
            rodio::SampleRate::new(self.0).unwrap()
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    #[test]
    fn a_long_fade_at_a_high_rate_keeps_its_schedule() {
        // The sleep timer's 60 s fade on a 192 kHz track: 11.52 million
        // frames, a step of 1.46 units in the last place of an f32 gain near 1.
        let (mut source, handle) = wrap(Ones(192_000));
        handle.ramp_to(0.0, Duration::from_secs(60));
        let frames = 192_000 * 60;
        let quarter = source.by_ref().nth(frames / 4 - 1).unwrap();
        assert!((quarter - 0.75).abs() < 1e-4, "a quarter in: {quarter}");
        let end = source.by_ref().nth(frames - frames / 4 - 1).unwrap();
        assert_eq!(end, 0.0, "the end of the ramp");
    }

    #[test]
    fn a_ramp_starts_within_a_millisecond_at_a_low_rate() {
        // An 8 kHz podcast: pause() waits only 20 ms past its 150 ms fade.
        let (mut source, handle) = wrap(Ones(8_000));
        let _ = source.by_ref().nth(100);
        handle.ramp_to(0.0, Duration::from_millis(150));
        let late = source
            .by_ref()
            .take(1_000)
            .position(|gain| gain < 1.0)
            .expect("the ramp started");
        assert!(late <= 8, "started {late} frames late");
    }
}
