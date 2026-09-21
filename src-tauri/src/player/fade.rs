//! Gain-ramp wrapper: the audible half of crossfades and click-free
//! pause/resume. The player thread steers a source's gain through a shared
//! handle; the ramp itself runs sample-accurately inside the audio callback.

use rodio::Source;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Check the shared state at least this often (frames) inside long spans.
const PARAM_CHECK_INTERVAL: u32 = 512;

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
    target: f32,
    /// Gain delta per frame while ramping.
    step: f32,
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
            target: 1.0,
            step: 0.0,
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
        self.step = (self.target - self.gain) / frames as f32;
    }
}

impl<S: Source> Iterator for FadeSource<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;
        if self.channel_idx == 0 {
            if self.until_check == 0 {
                self.until_check = PARAM_CHECK_INTERVAL;
                self.sync();
            }
            self.until_check -= 1;
            if self.step != 0.0 {
                self.gain += self.step;
                let done = (self.step > 0.0 && self.gain >= self.target)
                    || (self.step < 0.0 && self.gain <= self.target);
                if done {
                    self.gain = self.target;
                    self.step = 0.0;
                }
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
}
