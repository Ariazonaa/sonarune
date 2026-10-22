//! What a player hands the mixer. rodio's mixer converts every source it is
//! given to its own format (`UniformSourceIterator`), and builds that
//! conversion anew wherever the source says its format may change — at every
//! span, on the audio callback — and allocates each time, even when there
//! is nothing to convert. The tracks come at the output's rate already
//! (resampled on their decoder threads, `source/resample.rs`), so this stage
//! does the mixer's conversion in its place, without allocating:
//!
//! - the channels as rodio converts them (mono to both sides, extra channels
//!   dropped, missing ones silent), re-read where each span ends — a gapless
//!   hand-over may change them;
//! - the rate only for a source that is not at the output's (one opened for
//!   an output that has been replaced since): rodio's own conversion then,
//!   allocating as it always did.
//!
//! The mixer sees one endless span in its own format and passes it through.

use rodio::conversions::{ChannelCountConverter, SampleRateConverter};
use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use std::time::Duration;

/// rodio's limit on a span, which this keeps: a source that reports a longer
/// one has its format looked at again after this many samples — rounded down
/// to whole frames, unlike rodio. Its limit is no multiple of 3, 5, 6 or 7
/// channels: there a span ended mid-frame, the channels dropped in a downmix
/// were read from the next span, and from then on the wrong ones played.
const MAX_SPAN: usize = 32_768;

/// `source` in the mixer's format (`channels`, `rate`). Built on the
/// player's thread: its first conversion is set up here, not on the callback.
pub fn wrap<S: Source>(source: S, channels: ChannelCount, rate: SampleRate) -> Uniform<S> {
    Uniform {
        stage: Some(Stage::new(source, channels, rate)),
        channels,
        rate,
    }
}

/// A span of the source: it ends where the source says its format may
/// change.
struct Span<S> {
    source: S,
    left: Option<usize>,
}

impl<S: Iterator<Item = f32>> Iterator for Span<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match &mut self.left {
            Some(0) => None,
            Some(left) => {
                *left -= 1;
                self.source.next()
            }
            None => self.source.next(),
        }
    }
}

enum Stage<S: Iterator<Item = f32>> {
    /// At the mixer's rate: only the channels to convert.
    Direct(ChannelCountConverter<Span<S>>),
    /// At another rate: as rodio's mixer converts it.
    Converted(ChannelCountConverter<SampleRateConverter<Span<S>>>),
}

impl<S: Source> Stage<S> {
    fn new(source: S, channels: ChannelCount, rate: SampleRate) -> Self {
        let from_channels = source.channels();
        let from_rate = source.sample_rate();
        let frame = usize::from(from_channels.get());
        let span = Span {
            left: source.current_span_len().map(|len| {
                if len > frame {
                    len.min(MAX_SPAN) / frame * frame
                } else {
                    len
                }
            }),
            source,
        };
        if from_rate == rate {
            Self::Direct(ChannelCountConverter::new(span, from_channels, channels))
        } else {
            let converted = SampleRateConverter::new(span, from_rate, rate, from_channels);
            Self::Converted(ChannelCountConverter::new(
                converted,
                from_channels,
                channels,
            ))
        }
    }

    fn source(&mut self) -> &mut S {
        match self {
            Self::Direct(stage) => &mut stage.inner_mut().source,
            Self::Converted(stage) => &mut stage.inner_mut().inner_mut().source,
        }
    }

    fn into_source(self) -> S {
        match self {
            Self::Direct(stage) => stage.into_inner().source,
            Self::Converted(stage) => stage.into_inner().into_inner().source,
        }
    }
}

impl<S: Source> Iterator for Stage<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        match self {
            Self::Direct(stage) => stage.next(),
            Self::Converted(stage) => stage.next(),
        }
    }
}

/// See [`wrap`].
pub struct Uniform<S: Source> {
    /// Always there, but for the moment it is rebuilt in.
    stage: Option<Stage<S>>,
    channels: ChannelCount,
    rate: SampleRate,
}

impl<S: Source> Iterator for Uniform<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let stage = self.stage.as_mut()?;
        if let Some(sample) = stage.next() {
            return Some(sample);
        }
        // The span is over: the format may have changed. As rodio does it,
        // and moving the source only; a conversion at the mixer's rate holds
        // nothing on the heap.
        let source = self.stage.take()?.into_source();
        let mut stage = Stage::new(source, self.channels, self.rate);
        let sample = stage.next();
        self.stage = Some(stage);
        sample
    }
}

impl<S: Source> Source for Uniform<S> {
    /// One endless span: the format never changes.
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        match &mut self.stage {
            Some(stage) => stage.source().try_seek(pos),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::tap::tests::count_heap_ops;
    use rodio::buffer::SamplesBuffer;
    use rodio::source::UniformSourceIterator;

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    fn channels(count: u16) -> ChannelCount {
        ChannelCount::new(count).unwrap()
    }

    /// A player's queue of `parts`, as the mixer gets it, and how many
    /// samples it plays in all.
    fn queue(parts: &[(u16, u32, usize)]) -> (rodio::queue::SourcesQueueOutput, usize) {
        let (input, output) = rodio::queue::queue(false);
        let mut total = 0;
        for (index, (count, hz, frames)) in parts.iter().enumerate() {
            let samples: Vec<f32> = (0..frames * usize::from(*count))
                .map(|i| (index * 100_000 + i) as f32 / 1e6)
                .collect();
            total += samples.len();
            input.append(SamplesBuffer::new(channels(*count), rate(*hz), samples));
        }
        (output, total)
    }

    #[test]
    fn it_converts_as_rodios_mixer_does() {
        // Stereo and mono at the mixer's rate, one at another rate, and a
        // surround one: in a stereo mix at 48 kHz.
        let parts = [
            (2, 48_000, 3_000),
            (1, 48_000, 2_000),
            (2, 44_100, 2_500),
            (6, 48_000, 1_000),
            (2, 48_000, 1_500),
        ];
        let (ours, _) = queue(&parts);
        let ours: Vec<f32> = wrap(ours, channels(2), rate(48_000)).collect();
        let (theirs, _) = queue(&parts);
        let theirs: Vec<f32> =
            UniformSourceIterator::new(theirs, channels(2), rate(48_000)).collect();
        assert_eq!(ours.len(), theirs.len());
        assert_eq!(ours, theirs, "sample for sample");
    }

    #[test]
    fn surround_folds_to_the_front_pair_however_long_its_spans() {
        // A 5.1 track in spans longer than rodio's limit, into stereo: front
        // left and right of every frame, never two other channels.
        let frames = 20_000;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|f| (0..6).map(move |c| (f * 10 + c) as f32))
            .collect();
        let surround = || SamplesBuffer::new(channels(6), rate(48_000), samples.clone());
        let ours: Vec<f32> = wrap(surround(), channels(2), rate(48_000)).collect();
        let expected: Vec<f32> = (0..frames)
            .flat_map(|f| [(f * 10) as f32, (f * 10 + 1) as f32])
            .collect();
        assert_eq!(ours, expected);
        // rodio's own conversion loses the frame at its limit.
        let theirs: Vec<f32> =
            UniformSourceIterator::new(surround(), channels(2), rate(48_000)).collect();
        assert_ne!(theirs, expected, "the test would not see it");
    }

    #[test]
    fn at_the_output_rate_nothing_is_allocated_on_the_callback() {
        // A decoder's spans (one per MP3 frame, say), and a change from
        // stereo to mono and back, as at a gapless hand-over.
        let mut ours = wrap(packets(), channels(2), rate(48_000));
        let ops = count_heap_ops(|| {
            for _ in 0..PACKET * 2 * 60 {
                assert!(std::hint::black_box(ours.next()).is_some());
            }
        });
        assert_eq!(ops, 0, "heap operations on the callback");
        // rodio's mixer, on the same source, allocates at every span.
        let mut theirs = UniformSourceIterator::new(packets(), channels(2), rate(48_000));
        let ops = count_heap_ops(|| {
            for _ in 0..PACKET * 2 * 60 {
                assert!(std::hint::black_box(theirs.next()).is_some());
            }
        });
        assert!(ops >= 59, "the test would not see an allocation: {ops}");
    }

    /// Frames per packet.
    const PACKET: usize = 1_152;

    /// 20 packets of stereo, 20 of mono, 20 of stereo, at 48 kHz.
    fn packets() -> Packets {
        Packets {
            parts: [2, 1, 2]
                .map(|count| {
                    let samples = vec![0.25; PACKET * usize::from(count) * 20];
                    SamplesBuffer::new(channels(count), rate(48_000), samples)
                })
                .into(),
            at: 0,
        }
    }

    /// A decoder whose format changes between its parts, and which reports a
    /// span per packet. A part it is through with stays where it is: nothing
    /// is freed while it plays.
    struct Packets {
        parts: Vec<SamplesBuffer>,
        at: usize,
    }

    impl Iterator for Packets {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            let part = self.parts.get_mut(self.at)?;
            let sample = part.next();
            if part.size_hint().0 == 0 {
                self.at += 1;
            }
            sample
        }
    }

    impl Source for Packets {
        fn current_span_len(&self) -> Option<usize> {
            let Some(part) = self.parts.get(self.at) else {
                return Some(0);
            };
            let packet = PACKET * usize::from(part.channels().get());
            let left = part.size_hint().0;
            Some(match left % packet {
                0 => left.min(packet),
                rest => rest,
            })
        }

        fn channels(&self) -> ChannelCount {
            self.parts
                .get(self.at)
                .map_or(channels(2), |part| part.channels())
        }

        fn sample_rate(&self) -> SampleRate {
            rate(48_000)
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }
}
