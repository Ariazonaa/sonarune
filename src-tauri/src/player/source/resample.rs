//! The conversion stage between a source's decoder and its ring (see
//! `feed.rs`), on the decoder thread: the decoder's audio at the output's
//! rate, in the channel count the decoder started with. The mixer then has
//! no rate to convert.
//!
//! - Audio at the output's rate passes untouched: bit for bit, no delay.
//! - Any other rate goes through rubato's FFT resampler: a band-limited
//!   conversion between two fixed rates (rodio's own is linear, which dulls
//!   the treble and folds images back into the audible band). Its delay is
//!   cut off at the start and its tail played out at the end, so a track comes
//!   out as long as it is at the new rate, to the nearest frame
//!   ([`converted_len`]), and starts on its own first sample: a gapless
//!   hand-over loses or adds no frame.
//! - What the resampler sees before the first frame and after the last is
//!   that frame held, not silence: silence would dip a track that starts or
//!   ends loud to about half its level for a few samples, a click; a held
//!   edge leaves a kink only. And each track starts on a grid of its own.
//! - Except at a gapless boundary, where the player couples the two tracks
//!   (`feed::FeedHandle::couple`): the resampler of the track before plays
//!   out into the first frames of the track after ([`Head`]), and the track
//!   after is converted from the last frames of the one before ([`Tail`]), on
//!   the output grid that one ended on. Between two tracks at one rate that
//!   is exactly what one piece would come out as: no kink, which would ring
//!   near the cut-off for a few milliseconds, and no shift of up to half a
//!   sample (10 µs at 48 kHz) where the boundary falls between two output
//!   samples. Between two rates the other track's frames are interpolated
//!   ([`Seam`]), and the grids meet to within half of 1/`unit_in` of an
//!   output sample — 1/294 for 44.1 kHz on 48, 1/4 for 96 kHz — or, for a
//!   track the output's rate passes untouched, not at all.
//! - Rates with too little in common for an FFT block of bounded size (an
//!   odd rate on a 192 kHz device) go through rubato's sinc resampler instead,
//!   starting within half a sample — coupled at a gapless boundary by the
//!   edge frames only, with no grid to meet; a rate outside 4 to 768 kHz, a
//!   broken header's, is not played at all (see [`plan`]). A sample that is
//!   not a number goes in as silence.
//! - A seek starts the resampler afresh, at the target.
//! - A decoder may change its format between spans (a chained Ogg stream).
//!   Channels are converted as rodio converts them (mono to both sides,
//!   extra channels dropped, missing ones silent); a new rate gets a
//!   resampler of its own once the old one has played out.

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};

/// The resampler's FFT block holds at least this many frames at the lower of
/// the two rates. The longer the block, the steeper the anti-alias filter: at
/// 44.1 to 48 kHz (1 176 frames in) it passes everything up to 21.6 kHz
/// and stops images from 22.05 kHz up. Its delay, half a block (13 ms there),
/// costs nothing: it is cut off, on a thread a second and a half ahead.
const BLOCK_FRAMES: usize = 1024;

/// The rates a source may have to be resampled. A WAV header can claim any
/// rate at all (Symphonia takes any that is not zero, FLAC up to 655 kHz):
/// below 4 kHz there is no music (telephone speech has 8, old game sound
/// 5.5), above 768 kHz no PCM format. A track outside is not played: its rate
/// is broken, and converting it would take a resampler of unbounded size.
const RATES: std::ops::RangeInclusive<u32> = 4_000..=768_000;

/// The largest FFT block, in frames at either rate. The standard rates need
/// at most 24 576 (8 to 192 kHz); rates with little in common need more (a
/// block is twice the rate when they share no factor). Beyond this, an FFT
/// block would cost tens of MB and seconds before the first sample, and the
/// sinc resampler takes over, in chunks of [`SINC_CHUNK`].
const MAX_BLOCK_FRAMES: usize = 1 << 17;

/// Input frames per call of the sinc resampler, and its filter: half as long
/// as rubato's default, its table linearly interpolated, for a quarter of the
/// cost (it is for odd rates only, but it ought to keep up in a debug build
/// as well). Images and aliases still stay below -100 dB.
const SINC_CHUNK: usize = 1024;
const SINC_LEN: usize = 128;
const SINC_OVERSAMPLING: usize = 256;

/// How much of a track's start the stage keeps, from its first frame: what
/// the track before plays out into at a gapless boundary (see [`Head`]).
const HEAD_SECONDS: f64 = 0.25;
/// How much of a track's end it keeps at least: what the track after starts
/// from (see [`Tail`]).
const TAIL_SECONDS: f64 = 0.1;
/// Nor more than this many samples (16 MiB) of a track's start, kept to
/// convert it once more as a gapless successor (see [`Stage::restart_after`]).
const MAX_KEPT_SAMPLES: usize = 4 * 1024 * 1024;
/// Frames an interpolation between two tracks at different rates goes
/// through (Lagrange's, see [`Seam`]): a sine of 3 kHz comes out to within
/// 10^-6 of itself, one of 10 kHz to within 10^-2, the high treble less
/// exactly (a tenth of a sine of 15 kHz) — a good deal less than a held edge
/// makes of it all the same.
const STENCIL: usize = 8;

/// How a pair of rates is converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// By FFT, in blocks of this many frames in and out.
    Fft { block_in: usize, block_out: usize },
    /// By sinc interpolation: the rates have too little in common.
    Sinc,
}

/// How to convert `from` to `to`, or None when either is outside [`RATES`].
/// Decided before anything is allocated.
fn plan(from: u32, to: u32) -> Option<Plan> {
    if !RATES.contains(&from) || !RATES.contains(&to) {
        return None;
    }
    let (block_in, block_out) = block_sizes(from, to);
    Some(if block_in.max(block_out) <= MAX_BLOCK_FRAMES {
        Plan::Fft {
            block_in,
            block_out,
        }
    } else {
        Plan::Sinc
    })
}

/// `from` can be played at `to`: the same rate, or two that [`plan`] takes.
pub fn convertible(from: u32, to: u32) -> bool {
    from == to || plan(from, to).is_some()
}

/// The frames of one resampler block, at the decoder's rate and at the
/// output's: an even number of the smallest blocks the ratio allows (147 and
/// 160 for 44.1 to 48 kHz), so that half a block, the delay, is a whole number
/// of frames on both sides. Rates with little in common make for big blocks:
/// 22 222 Hz (a WAV of an odd rate) to 48 kHz takes a second of input per
/// block, and a second and a half before the first sample comes out; beyond
/// [`MAX_BLOCK_FRAMES`] the sinc resampler takes over (see [`plan`]).
fn block_sizes(from: u32, to: u32) -> (usize, usize) {
    let gcd = gcd(from, to);
    let (unit_in, unit_out) = ((from / gcd) as usize, (to / gcd) as usize);
    let units = BLOCK_FRAMES
        .div_ceil(unit_in.min(unit_out))
        .next_multiple_of(2);
    (units * unit_in, units * unit_out)
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Frames at `to` for `frames` frames at `from`: the time they take, to the
/// nearest frame (halves up) — what the stage hands out for that many frames
/// of input. To the nearest rather than up: the next track's start is then
/// at most half a sample off its true time, not up to a whole one.
pub fn converted_len(frames: u64, from: u32, to: u32) -> u64 {
    let gcd = gcd(from, to);
    let (unit_in, unit_out) = (u128::from(from / gcd), u128::from(to / gcd));
    let len = (2 * u128::from(frames) * unit_out + unit_in) / (2 * unit_in);
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// A frame of `raw` (the decoder's channels) in `frame`'s channel count, as
/// rodio's `ChannelCountConverter` makes it.
fn convert_channels(raw: &[f32], frame: &mut [f32]) {
    for (channel, slot) in frame.iter_mut().enumerate() {
        *slot = match raw.get(channel) {
            Some(sample) => *sample,
            // Mono goes to both sides; any other missing channel is silent.
            None if channel == 1 => raw[0],
            None => 0.0,
        };
    }
}

/// One track's frames at a gapless boundary: interleaved, at its rate.
#[derive(Clone, Copy)]
struct Side<'a> {
    frames: &'a [f32],
    channels: usize,
    rate: u32,
}

impl<'a> Side<'a> {
    fn len(&self) -> usize {
        self.frames.len() / self.channels.max(1)
    }

    fn frame(&self, index: usize) -> &'a [f32] {
        &self.frames[index * self.channels..(index + 1) * self.channels]
    }
}

/// The two sides of a gapless boundary: the frames that end there and those
/// that start there, each at its own rate and in its own channels.
struct Seam<'a> {
    before: Side<'a>,
    after: Side<'a>,
}

impl Seam<'_> {
    /// The frames at `times` (seconds after the boundary; before it, when
    /// negative), in `channels`: each through the [`STENCIL`] frames nearest
    /// to it, on either side (Lagrange's interpolation), so that it is never
    /// extrapolated — only beyond the frames there are, where the outermost
    /// is held. On a frame: that frame. What is not a number comes out as
    /// silence.
    fn frames_at(&self, times: impl Iterator<Item = f64>, channels: usize) -> Vec<f32> {
        let (before, after) = (self.before.len(), self.after.len());
        let time_before = |k: usize| -((before - k) as f64) / f64::from(self.before.rate);
        let time_after = |j: usize| j as f64 / f64::from(self.after.rate);
        let first = if before > 0 { time_before(0) } else { 0.0 };
        let last = match (before, after) {
            (_, 1..) => time_after(after - 1),
            (1.., 0) => time_before(before - 1),
            (0, 0) => return Vec::new(),
        };
        let mut out = Vec::new();
        let mut frame = vec![0.0f32; channels];
        let mut converted = vec![0.0f32; channels];
        let mut nodes: Vec<(f64, &[f32], usize)> = Vec::with_capacity(2 * STENCIL);
        for t in times {
            let t = t.clamp(first, last);
            // Up to a stencil on either side, around where `t` falls there.
            nodes.clear();
            for (side, len, time) in [
                (self.before, before, &time_before as &dyn Fn(usize) -> f64),
                (self.after, after, &time_after),
            ] {
                if len == 0 {
                    continue;
                }
                let at = (t - time(0)) * f64::from(side.rate);
                let low = (at.round() as isize - STENCIL as isize / 2)
                    .clamp(0, len.saturating_sub(STENCIL) as isize)
                    as usize;
                for index in low..(low + STENCIL).min(len) {
                    nodes.push((time(index), side.frame(index), side.channels));
                }
            }
            nodes.sort_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()));
            nodes.truncate(STENCIL);
            frame.fill(0.0);
            for (j, (tj, samples, side_channels)) in nodes.iter().enumerate() {
                // Lagrange's weight of node j: 1 there, 0 at the others.
                let weight: f64 = nodes
                    .iter()
                    .enumerate()
                    .filter(|(m, _)| *m != j)
                    .map(|(_, (tm, ..))| (t - tm) / (tj - tm))
                    .product();
                if weight == 0.0 {
                    continue;
                }
                convert_channels(&samples[..*side_channels], &mut converted);
                for (slot, sample) in frame.iter_mut().zip(&converted) {
                    *slot += (weight * f64::from(*sample)) as f32;
                }
            }
            out.extend(frame.iter().map(|s| if s.is_finite() { *s } else { 0.0 }));
        }
        out
    }
}

/// A track's first frames, as its decoder gave them (in the ring's
/// channels): what the resampler of the track before plays out into at a
/// gapless boundary, instead of holding that one's last frame.
#[derive(Debug)]
pub struct Head {
    rate: u32,
    channels: usize,
    frames: Vec<f32>,
}

impl Head {
    /// The decoder's rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }
}

/// A track's last frames, and where its end fell on the output's grid: what
/// the resampler of the track after starts from at a gapless boundary,
/// instead of holding its own first frame — and on which grid.
#[derive(Debug)]
pub struct Tail {
    /// The decoder's rate and channels, and the output's rate.
    rate: u32,
    to: u32,
    channels: usize,
    frames: Vec<f32>,
    /// How far past an output frame the end fell: `end_num / end_den` of
    /// one (0 for a track at the output's rate, which ends on its grid).
    end_num: u64,
    end_den: u64,
}

/// Frames of a track's end to keep for the track after, at the rate of
/// the decoder (`from`) and the output (`to`): enough for one at the same
/// rates to start from on any grid point (see `Converter::couple_after`),
/// and [`TAIL_SECONDS`] at least. None for rates that are not converted.
fn history_frames(from: u32, to: u32) -> usize {
    let least = (TAIL_SECONDS * f64::from(from)).ceil() as usize;
    match plan(from, to) {
        None => 0,
        Some(_) if from == to => least,
        Some(Plan::Fft { block_in, .. }) => {
            least.max(block_in / 2 + (from / gcd(from, to)) as usize)
        }
        Some(Plan::Sinc) => least.max(SINC_LEN),
    }
}

/// What the decoder gave.
enum Pull {
    /// A frame, in `Intake::frame`.
    Frame,
    /// Nothing until a seek: it ran out, or gave up a read.
    End,
    /// Its next frame is at another rate than the one asked for.
    Rate,
}

/// The decoder, read a frame at a time, with its format as of its current
/// span: rodio's sources may change it where a span ends.
struct Intake<S> {
    source: S,
    /// What the decoder delivers now, and the samples left of its span.
    from_channels: usize,
    from_rate: u32,
    span_left: Option<usize>,
    raw: Vec<f32>,
    /// The last frame read, in the ring's channel count.
    frame: Vec<f32>,
}

impl<S: Source> Intake<S> {
    fn new(source: S) -> Self {
        let channels = usize::from(source.channels().get());
        let mut intake = Self {
            from_channels: channels,
            from_rate: source.sample_rate().get(),
            span_left: source.current_span_len(),
            raw: Vec::new(),
            frame: vec![0.0; channels],
            source,
        };
        intake.raw.resize(channels, 0.0);
        intake
    }

    /// Read the decoder's format anew: at the start of a span, after a seek.
    fn refresh(&mut self) {
        self.from_channels = usize::from(self.source.channels().get());
        self.from_rate = self.source.sample_rate().get();
        self.span_left = self.source.current_span_len();
        // Allocates only when the channel count changes, which is once in a
        // chained stream at most.
        self.raw.resize(self.from_channels, 0.0);
    }

    /// The next frame, as long as it is at `rate`. A frame the decoder cuts
    /// short is made up with silence.
    fn pull(&mut self, rate: u32) -> Pull {
        if self.span_left == Some(0) {
            self.refresh();
        }
        if self.from_rate != rate {
            return Pull::Rate;
        }
        for channel in 0..self.from_channels {
            let Some(sample) = self.source.next() else {
                if channel == 0 {
                    return Pull::End;
                }
                self.raw[channel..].fill(0.0);
                break;
            };
            self.raw[channel] = sample;
            if let Some(left) = &mut self.span_left {
                *left = left.saturating_sub(1);
            }
        }
        convert_channels(&self.raw, &mut self.frame);
        Pull::Frame
    }
}

/// One of rubato's resamplers.
/// Boxed, both: they differ in size by hundreds of bytes.
enum Engine {
    Fft(Box<Fft<f32>>),
    Sinc(Box<Async<f32>>),
}

impl Engine {
    fn resampler(&mut self) -> &mut dyn Resampler<f32> {
        match self {
            Self::Fft(fft) => fft.as_mut(),
            Self::Sinc(sinc) => sinc.as_mut(),
        }
    }
}

/// rubato's resampler for one pair of rates, with the bookkeeping that makes
/// its output start and end where the input does.
struct Converter {
    engine: Engine,
    /// The decoder's rate it converts.
    from: u32,
    to: u32,
    channels: usize,
    /// Frames per call in, and at most out.
    block_in: usize,
    block_out: usize,
    /// The two rates' common grid: a point every `unit_in` frames in is one
    /// every `unit_out` frames out.
    unit_in: u64,
    unit_out: u64,
    /// How often the first frame is held in front of it, and the output
    /// frames that come before it (the delay, and what came of those).
    held: usize,
    lead: usize,
    /// The frames in front of the first one, from the grid point the
    /// conversion starts on — `held`, unless it starts on the track before
    /// (see [`Converter::couple_after`]) — and, by FFT, the output frame the
    /// track starts on, counted from there (less the delay).
    lead_in: usize,
    first_out: u64,
    /// What goes in front of the first frame instead of that one held: the
    /// end of the track before, `lead_in` frames.
    prelude: Vec<f32>,
    /// What the end is padded with instead of the last frame: the start of
    /// the track after, and the frames of it used.
    sequel: Vec<f32>,
    sequel_at: usize,
    /// The block being filled, interleaved, and the frames in it.
    input: Vec<f32>,
    filled: usize,
    /// The last frame put in: what the end is padded with.
    last: Vec<f32>,
    /// The first frame since the last reset is in (and held in front of it).
    primed: bool,
    /// Output frames still to be dropped: the resampler's delay and what
    /// came of the held first frame.
    skip: usize,
    /// Frames put in, and handed out, since the last reset.
    consumed: u64,
    emitted: u64,
    /// All the output there is for that input, once it has ended.
    limit: Option<u64>,
}

impl Converter {
    /// None when a rate is out of [`RATES`]: nothing is allocated then.
    fn new(from: u32, to: u32, channels: usize) -> Option<Self> {
        let Some(plan) = plan(from, to) else {
            tracing::warn!("cannot resample {from} Hz to {to} Hz: not a sample rate for music");
            return None;
        };
        Self::with_plan(from, to, channels, plan)
    }

    fn with_plan(from: u32, to: u32, channels: usize, plan: Plan) -> Option<Self> {
        let failed = |e: rubato::ResamplerConstructionError| {
            tracing::error!("cannot resample {from} Hz to {to} Hz: {e}")
        };
        let (engine, block_in, block_out, held, lead) = match plan {
            Plan::Fft {
                block_in,
                block_out,
            } => {
                let fft = Fft::<f32>::new_custom(
                    from as usize,
                    to as usize,
                    block_in,
                    1,
                    channels,
                    WindowFunction::BlackmanHarris2,
                    FixedSync::Both,
                )
                .map_err(failed)
                .ok()?;
                debug_assert_eq!(
                    (fft.input_frames_next(), fft.output_frames_next()),
                    (block_in, block_out)
                );
                // The first block holds half a block of the held first frame
                // and half a block of input; with the delay (half a block
                // too) all of its output comes before the first frame.
                (
                    Engine::Fft(Box::new(fft)),
                    block_in,
                    block_out,
                    block_in / 2,
                    block_out,
                )
            }
            Plan::Sinc => {
                let ratio = f64::from(to) / f64::from(from);
                let parameters =
                    SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2)
                        .oversampling_factor(SINC_OVERSAMPLING)
                        .interpolation(SincInterpolationType::Linear);
                let sinc = Async::<f32>::new_sinc(
                    ratio,
                    1.0,
                    &parameters,
                    SINC_CHUNK,
                    channels,
                    FixedAsync::Input,
                )
                .map_err(failed)
                .ok()?;
                let block_out = sinc.output_frames_max();
                // The filter reaches half its length back, and lags as far
                // (less one output frame, as measured): a whole length of the
                // held frame in front, and what comes of it and the lag
                // dropped. Unlike the FFT's, that is no whole number of output
                // frames: the track starts up to half a sample early or late.
                let held = SINC_LEN;
                let lead = ((held + SINC_LEN / 2) as f64 * ratio - 1.0).round() as usize;
                (
                    Engine::Sinc(Box::new(sinc)),
                    SINC_CHUNK,
                    block_out,
                    held,
                    lead,
                )
            }
        };
        let gcd = gcd(from, to);
        let mut converter = Self {
            engine,
            from,
            to,
            channels,
            block_in,
            block_out,
            unit_in: u64::from(from / gcd),
            unit_out: u64::from(to / gcd),
            held,
            lead,
            lead_in: held,
            first_out: 0,
            prelude: Vec::new(),
            sequel: Vec::new(),
            sequel_at: 0,
            input: vec![0.0; block_in * channels],
            filled: 0,
            last: vec![0.0; channels],
            primed: false,
            skip: 0,
            consumed: 0,
            emitted: 0,
            limit: None,
        };
        converter.reset();
        Some(converter)
    }

    /// Start afresh: nothing put in, nothing to hand out.
    fn reset(&mut self) {
        self.engine.resampler().reset();
        self.filled = 0;
        self.primed = false;
        self.lead_in = self.held;
        // By FFT, half a block of the held frame makes half a block out.
        self.first_out = (self.block_out / 2) as u64;
        self.prelude.clear();
        self.sequel.clear();
        self.sequel_at = 0;
        self.skip = self.lead;
        self.consumed = 0;
        self.emitted = 0;
        self.limit = None;
    }

    fn by_fft(&self) -> bool {
        matches!(self.engine, Engine::Fft(_))
    }

    fn full(&self) -> bool {
        self.filled == self.block_in
    }

    fn write(&mut self, frame: &[f32]) {
        let at = self.filled * self.channels;
        self.input[at..at + self.channels].copy_from_slice(frame);
        self.filled += 1;
    }

    /// Fill the block up: with what there is of the track after, else with
    /// the last frame.
    fn pad(&mut self) {
        let channels = self.channels;
        let at = self.filled * channels;
        for slot in self.input[at..].chunks_exact_mut(channels) {
            let next = self.sequel_at * channels;
            let frame = if next + channels <= self.sequel.len() {
                self.sequel_at += 1;
                &self.sequel[next..next + channels]
            } else if self.sequel.len() >= channels {
                &self.sequel[self.sequel.len() - channels..]
            } else {
                &self.last[..]
            };
            slot.copy_from_slice(frame);
        }
        self.filled = self.block_in;
    }

    /// Start on the end of the track before (`tail`), at a gapless boundary:
    /// its last frames go in front of the first frame, not that one held,
    /// and by FFT the conversion starts on the grid that one's ended on — to
    /// the sample when both are at one rate, else to within half of
    /// 1/`unit_in` of a sample — so that this track takes over exactly where
    /// that one left off. `start` is this track's own start, which the
    /// frames between two rates are interpolated over too. Before the first
    /// frame only.
    fn couple_after(&mut self, tail: &Tail, start: &[f32]) {
        let frames = tail.frames.len() / tail.channels.max(1);
        if self.primed || frames == 0 || tail.to != self.to {
            return;
        }
        if self.by_fft() {
            let (lead_in, first_out) = self.grid_after(tail.end_num, tail.end_den);
            self.lead_in = lead_in;
            self.first_out = first_out;
            self.skip = self.block_out / 2 + first_out as usize;
        }
        // Its frames at this track's rate, up to where this one starts.
        let seam = Seam {
            before: Side {
                frames: &tail.frames,
                channels: tail.channels,
                rate: tail.rate,
            },
            after: Side {
                frames: start,
                channels: self.channels,
                rate: self.from,
            },
        };
        let lead_in = self.lead_in;
        let times = (0..lead_in).map(|i| -((lead_in - i) as f64) / f64::from(self.from));
        self.prelude = seam.frames_at(times, self.channels);
    }

    /// The frames in front of the first one, and the output frame the track
    /// starts on, for a conversion that is to go on from where the track
    /// before ended: `num / den` of an output frame past one. Of the grids
    /// that start `held` to `held + unit_in` frames before the first frame,
    /// the one nearest the track before's.
    fn grid_after(&self, num: u64, den: u64) -> (usize, u64) {
        let (unit_in, unit_out) = (i128::from(self.unit_in), i128::from(self.unit_out));
        let (num, den) = (i128::from(num), i128::from(den.max(1)));
        let whole = den * unit_in;
        // How far the grid starting `lead_in` frames before the first frame
        // lies from the one the track before ended on, in 1/`whole` of an
        // output frame.
        let offset = |lead_in: usize| num * unit_in - lead_in as i128 * unit_out * den;
        let mut best = (i128::MAX, self.held);
        for lead_in in self.held..self.held + self.unit_in as usize {
            let rest = offset(lead_in).rem_euclid(whole);
            let off = rest.min(whole - rest);
            if off < best.0 {
                best = (off, lead_in);
                if off == 0 {
                    break;
                }
            }
        }
        let lead_in = best.1;
        // The output frame of the track before's grid this one starts on,
        // and the one after that track's last (its end to the nearest
        // frame, halves up, as `converted_len` counts): the same one.
        let start = (2 * offset(lead_in) + whole).div_euclid(2 * whole);
        let after_last = i128::from(2 * num >= den);
        (lead_in, (after_last - start) as u64)
    }

    /// End into the start of the track after (`head`), at a gapless
    /// boundary: what the resampler needs past the last frame is that
    /// track's first frames, not this one's last frame held. `end` is this
    /// track's own end, which the frames between two rates are interpolated
    /// over too. Before the end is told only.
    fn continue_into(&mut self, head: &Head, end: &[f32]) {
        let frames = head.frames.len() / head.channels.max(1);
        if self.finishing() || frames == 0 {
            return;
        }
        // More than the end takes: the tail of a block, two blocks more —
        // as far as the head reaches.
        let wanted = if self.by_fft() {
            3 * self.block_in
        } else {
            2 * (SINC_CHUNK + SINC_LEN)
        };
        let reach = (frames - 1) as f64 / f64::from(head.rate);
        let seam = Seam {
            before: Side {
                frames: end,
                channels: self.channels,
                rate: self.from,
            },
            after: Side {
                frames: &head.frames,
                channels: head.channels,
                rate: head.rate,
            },
        };
        let times = (0..wanted)
            .map(|i| i as f64 / f64::from(self.from))
            .take_while(|t| *t <= reach);
        self.sequel = seam.frames_at(times, self.channels);
        self.sequel_at = 0;
    }

    /// The output frames there are for the input: by FFT, from the grid
    /// point the conversion starts on to the end (to the nearest frame),
    /// less those before the track's first.
    fn output_len(&self) -> u64 {
        if self.by_fft() {
            let frames = self.lead_in as u64 + self.consumed;
            converted_len(frames, self.from, self.to).saturating_sub(self.first_out)
        } else {
            converted_len(self.consumed, self.from, self.to)
        }
    }

    /// How far past an output frame the end of the input falls, on the grid
    /// the conversion started on: `(num, den)` of one.
    fn end_fraction(&self) -> (u64, u64) {
        let frames = if self.by_fft() {
            self.lead_in as u64 + self.consumed
        } else {
            self.consumed
        };
        let num = u128::from(frames) * u128::from(self.unit_out) % u128::from(self.unit_in);
        (num as u64, self.unit_in)
    }

    /// Put in a frame. The block must not be full.
    ///
    /// A sample that is not a number (or infinite) goes in as silence: in
    /// the FFT it would spread over its block and the next, some 50 ms.
    fn push(&mut self, frame: &[f32]) {
        for (last, sample) in self.last.iter_mut().zip(frame) {
            *last = if sample.is_finite() { *sample } else { 0.0 };
        }
        let last = std::mem::take(&mut self.last);
        if !self.primed {
            self.primed = true;
            // Fewer than a block: they and the frame fit in the first one.
            if self.prelude.is_empty() {
                for _ in 0..self.held {
                    self.write(&last);
                }
            } else {
                let prelude = std::mem::take(&mut self.prelude);
                for frame in prelude.chunks_exact(self.channels) {
                    self.write(frame);
                }
            }
        }
        self.write(&last);
        self.last = last;
        self.consumed += 1;
    }

    /// The input has ended: play out what it still owes. Returns whether
    /// there is anything left to hand out.
    fn finish(&mut self) -> bool {
        if self.limit.is_none() {
            self.limit = Some(self.output_len());
        }
        self.limit.is_some_and(|limit| self.emitted < limit)
    }

    fn finishing(&self) -> bool {
        self.limit.is_some()
    }

    fn done(&self) -> bool {
        self.limit.is_some_and(|limit| self.emitted >= limit)
    }

    /// Resample the block into `out` (a block of output frames), padded
    /// with the last frame when the input has ended. Returns the frames of
    /// `out` to hand out.
    fn run(&mut self, out: &mut [f32]) -> std::ops::Range<usize> {
        if self.finishing() {
            self.pad();
        }
        debug_assert!(self.full());
        let input = InterleavedSlice::new(&self.input[..], self.channels, self.block_in)
            .expect("the block fits its buffer");
        let mut output = InterleavedSlice::new_mut(out, self.channels, self.block_out)
            .expect("the output fits its buffer");
        // The FFT gives a whole block, the sinc resampler what it can.
        let made = match self
            .engine
            .resampler()
            .process_into_buffer(&input, &mut output, None)
        {
            Ok((_, made)) => made,
            Err(e) => {
                // Cannot happen with buffers of the sizes the resampler asked
                // for; a block of silence beats a crash.
                tracing::error!("the resampler failed: {e}");
                out.fill(0.0);
                self.block_out
                    .min(self.block_in * self.to as usize / self.from as usize)
            }
        };
        self.filled = 0;
        let start = self.skip.min(made);
        self.skip -= start;
        let mut end = made;
        if let Some(limit) = self.limit {
            let left = usize::try_from(limit - self.emitted).unwrap_or(usize::MAX);
            end = end.min(start + left);
        }
        self.emitted += (end - start) as u64;
        start..end
    }
}

/// The decoder's frames, and what the stage keeps of them: the last ones,
/// for the track after, and those since the track's start, for as long as
/// they can be had again without the decoder.
struct Input<S> {
    intake: Intake<S>,
    /// Every frame since the start, while kept (see
    /// [`Stage::keep_from_start`]), up to `kept_max` samples; and how many
    /// of them were taken since the stage started on them (fewer while it
    /// takes them once more, see [`Stage::restart_after`]).
    kept: Option<Vec<f32>>,
    kept_max: usize,
    taken: usize,
    /// The first `head_len` samples of that, once they are in.
    head: Option<Vec<f32>>,
    head_len: usize,
    /// The last frames taken, at `history_rate`, round and round: the next
    /// goes in at `history_at`, `history_len` are in.
    history: Vec<f32>,
    history_rate: u32,
    history_at: usize,
    history_len: usize,
}

impl<S: Source> Input<S> {
    /// The next frame (into `intake.frame`), as long as it is at `rate`.
    fn pull(&mut self, rate: u32) -> Pull {
        let channels = self.intake.frame.len();
        if let Some(kept) = &self.kept {
            let at = self.taken * channels;
            if at < kept.len() {
                self.intake.frame.copy_from_slice(&kept[at..at + channels]);
                self.taken += 1;
                self.remember();
                return Pull::Frame;
            }
        }
        let pulled = self.intake.pull(rate);
        match pulled {
            Pull::Frame => {
                if let Some(kept) = &mut self.kept {
                    if kept.len() + channels > self.kept_max {
                        self.kept = None;
                    } else {
                        kept.extend_from_slice(&self.intake.frame);
                        self.taken += 1;
                        if kept.len() == self.head_len {
                            self.head = Some(kept.clone());
                        }
                    }
                }
                self.remember();
            }
            // Not the track it started as any more, as far as a successor
            // could tell.
            Pull::Rate => self.kept = None,
            Pull::End => {}
        }
        pulled
    }

    /// Keep the last `frames` frames from here on, at the decoder's rate.
    fn restart_history(&mut self, frames: usize) {
        let channels = self.intake.frame.len();
        self.history.clear();
        self.history.resize(frames * channels, 0.0);
        self.history_rate = self.intake.from_rate;
        self.history_at = 0;
        self.history_len = 0;
    }

    /// Remember the frame just taken, a sample that is not a number as
    /// silence (as the resampler takes it).
    fn remember(&mut self) {
        let channels = self.intake.frame.len();
        let capacity = self.history.len() / channels;
        if capacity == 0 {
            return;
        }
        let at = self.history_at * channels;
        let slots = &mut self.history[at..at + channels];
        for (slot, sample) in slots.iter_mut().zip(&self.intake.frame) {
            *slot = if sample.is_finite() { *sample } else { 0.0 };
        }
        self.history_at = (self.history_at + 1) % capacity;
        self.history_len = (self.history_len + 1).min(capacity);
    }

    /// The frames remembered, oldest first.
    fn history(&self) -> Vec<f32> {
        let channels = self.intake.frame.len();
        let capacity = self.history.len() / channels;
        let oldest = if self.history_len < capacity {
            0
        } else {
            self.history_at
        };
        (0..self.history_len)
            .flat_map(|i| {
                let at = (oldest + i) % capacity * channels;
                self.history[at..at + channels].iter().copied()
            })
            .collect()
    }
}

/// The stage itself: an iterator over the ring's samples (whole frames),
/// pulling the decoder as it goes. It ends where the decoder does, short of
/// what the resampler still owes: [`Stage::finish`] plays that out, once the
/// end is known to be the end — not a read cut short for a seek.
pub struct Stage<S> {
    input: Input<S>,
    /// The first frames have gone out as the track's [`Head`] (or never
    /// will).
    head_taken: bool,
    /// The ring's format.
    channels: usize,
    rate: u32,
    /// None while the decoder is at the ring's rate.
    converter: Option<Box<Converter>>,
    /// Samples waiting to be handed out, and how far that has come.
    out: Vec<f32>,
    at: usize,
    end: usize,
    /// The decoder has nothing more until a seek.
    drained: bool,
    /// The decoder moved on to another rate; the converter plays out first.
    switching: bool,
    /// No resampler could be built for the decoder's rate (it cannot happen
    /// with rates rodio accepts): the source ends.
    failed: bool,
}

impl<S: Source> Stage<S> {
    /// `source` at `rate`, in the channels it starts with.
    pub fn new(source: S, rate: SampleRate) -> Self {
        let intake = Intake::new(source);
        let channels = intake.frame.len();
        let input = Input {
            intake,
            kept: None,
            kept_max: 0,
            taken: 0,
            head: None,
            head_len: 0,
            history: Vec::new(),
            history_rate: 0,
            history_at: 0,
            history_len: 0,
        };
        let mut stage = Self {
            input,
            head_taken: false,
            channels,
            rate: rate.get(),
            converter: None,
            out: Vec::new(),
            at: 0,
            end: 0,
            drained: false,
            switching: false,
            failed: false,
        };
        stage.adopt();
        stage
    }

    pub fn channels(&self) -> ChannelCount {
        ChannelCount::new(self.channels as u16).expect("a source has channels")
    }

    /// The rate the decoder came to that cannot be resampled (one no music
    /// has, see [`plan`]): the stage ended there, and stays ended.
    pub fn refused(&self) -> Option<u32> {
        self.failed.then_some(self.input.intake.from_rate)
    }

    /// A converter for the decoder's rate as it is now, or none; and the
    /// track's end remembered anew from here, for that rate.
    fn adopt(&mut self) {
        let from = self.input.intake.from_rate;
        self.switching = false;
        self.failed = false;
        self.input.restart_history(history_frames(from, self.rate));
        if from == self.rate {
            self.converter = None;
            self.out.resize(self.channels, 0.0);
            return;
        }
        let reuse = self
            .converter
            .as_ref()
            .is_some_and(|c| c.from == from && c.to == self.rate);
        if reuse {
            if let Some(converter) = &mut self.converter {
                converter.reset();
            }
        } else {
            // On the decoder thread: the allocation hurts nobody.
            self.converter = Converter::new(from, self.rate, self.channels).map(Box::new);
            self.failed = self.converter.is_none();
        }
        let block_out = self.converter.as_ref().map_or(1, |c| c.block_out);
        self.out.resize(block_out * self.channels, 0.0);
    }

    /// Hand out nothing of what was there: after a seek, a new rate.
    fn clear(&mut self) {
        self.at = 0;
        self.end = 0;
        self.drained = false;
    }

    /// Convert to `rate` from now on (another output). The source is
    /// positioned anew right after.
    pub fn retarget(&mut self, rate: SampleRate) {
        if rate.get() != self.rate {
            self.rate = rate.get();
            self.adopt();
        }
        self.clear();
    }

    /// The decoder has really ended: what the resampler still owes is handed
    /// out next. Returns whether there is any.
    pub fn finish(&mut self) -> bool {
        self.converter.as_mut().is_some_and(|c| c.finish())
    }

    /// Position the decoder at `pos`; the resampler starts afresh there.
    pub fn try_seek(&mut self, pos: std::time::Duration) -> Result<(), SeekError> {
        self.input.kept = None;
        self.input.intake.source.try_seek(pos)?;
        self.input.intake.refresh();
        // The same converter, reset, unless the rate is another one there.
        self.adopt();
        self.clear();
        Ok(())
    }

    /// Keep what the decoder gives from here, its start: for the track before
    /// to play out into ([`Self::take_head`]), and — when it is resampled and
    /// what a ring of `ring_frames` frames takes is few enough samples — to
    /// convert the track once more without the decoder
    /// ([`Self::restart_after`]).
    pub fn keep_from_start(&mut self, ring_frames: usize) {
        let from = self.input.intake.from_rate;
        if plan(from, self.rate).is_none() {
            return;
        }
        let head = (HEAD_SECONDS * f64::from(from)).ceil() as usize;
        let frames = match &self.converter {
            // All that fills the ring, and what the resampler holds back: the
            // delay, a block of output waiting, the block being filled — and
            // one more for good measure.
            Some(c) => {
                let ring = converted_len(ring_frames as u64, self.rate, from) as usize;
                head.max(ring + 4 * c.block_in)
            }
            None => head,
        };
        let frames = if frames * self.channels > MAX_KEPT_SAMPLES {
            head
        } else {
            frames
        };
        self.input.kept = Some(Vec::with_capacity(frames * self.channels));
        self.input.kept_max = frames * self.channels;
        self.input.taken = 0;
        self.input.head = None;
        self.input.head_len = head * self.channels;
    }

    /// Every frame the decoder gave is kept (see [`Self::restart_after`]).
    pub fn replayable(&self) -> bool {
        self.input.kept.is_some()
    }

    /// Keep nothing more: the decoder gave up a read, and goes on from
    /// elsewhere.
    pub fn forget_kept(&mut self) {
        self.input.kept = None;
    }

    /// The track's first frames, [`HEAD_SECONDS`] of them (all, of a
    /// shorter track), once they are in; once. None also when they were not
    /// kept.
    pub fn take_head(&mut self) -> Option<Head> {
        if self.head_taken {
            return None;
        }
        let frames = match (self.input.head.take(), &self.input.kept) {
            (Some(head), _) => head,
            // A shorter track, all of it.
            (None, Some(kept)) if self.drained => kept.clone(),
            (None, Some(_)) => return None,
            // Its start is gone: never.
            (None, None) => {
                self.head_taken = true;
                return None;
            }
        };
        self.head_taken = true;
        Some(Head {
            rate: self.input.intake.from_rate,
            channels: self.channels,
            frames,
        })
    }

    /// The decoder's audio is resampled.
    pub fn resampling(&self) -> bool {
        self.converter.is_some()
    }

    /// The track ends into `head`, the start of the track after (see
    /// [`Converter::continue_into`]). Before [`Self::finish`].
    pub fn continue_into(&mut self, head: &Head) {
        if let Some(converter) = &mut self.converter {
            converter.continue_into(head, &self.input.history());
        }
    }

    /// The track's end, for the track after to start from.
    pub fn tail(&self) -> Tail {
        let (end_num, end_den) = self.converter.as_ref().map_or((0, 1), |c| c.end_fraction());
        Tail {
            rate: self.input.history_rate,
            to: self.rate,
            channels: self.channels,
            frames: self.input.history(),
            end_num,
            end_den,
        }
    }

    /// Start the track afresh from its first frame, as the gapless successor
    /// of the track `tail` is the end of (see [`Converter::couple_after`]):
    /// from what was kept of it, the decoder going on after that, or else
    /// from the decoder positioned at 0. Returns whether it starts from
    /// `tail` (the rates allow it), rather than on its own.
    pub fn restart_after(&mut self, tail: Option<&Tail>) -> Result<bool, SeekError> {
        if self.input.kept.is_some() {
            self.input.taken = 0;
        } else {
            self.input
                .intake
                .source
                .try_seek(std::time::Duration::ZERO)?;
            self.input.intake.refresh();
        }
        self.adopt();
        self.clear();
        let Some(converter) = &mut self.converter else {
            return Ok(false);
        };
        let Some(tail) = tail else {
            return Ok(false);
        };
        converter.couple_after(tail, self.input.kept.as_deref().unwrap_or_default());
        Ok(!converter.prelude.is_empty())
    }

    /// Get the next samples ready. False when there are none (yet).
    fn refill(&mut self) -> bool {
        loop {
            if self.failed {
                return false;
            }
            if self.drained {
                // A converter that was told the end plays it out (a block may
                // go to the delay entirely); nothing else comes before a seek.
                let owes = |c: &Converter| c.finishing() && !c.done();
                if !self.converter.as_deref().is_some_and(owes) {
                    return false;
                }
                if self.run_block() {
                    return true;
                }
                continue;
            }
            let Some(converter) = &mut self.converter else {
                // At the ring's rate: frame by frame, untouched.
                match self.input.pull(self.rate) {
                    Pull::Frame => {
                        self.out[..self.channels].copy_from_slice(&self.input.intake.frame);
                        self.at = 0;
                        self.end = self.channels;
                        return true;
                    }
                    Pull::End => {
                        self.drained = true;
                        return false;
                    }
                    Pull::Rate => {
                        self.adopt();
                        continue;
                    }
                }
            };
            if converter.done() {
                if self.switching {
                    self.adopt();
                    continue;
                }
                return false;
            }
            if !converter.finishing() {
                while !converter.full() {
                    match self.input.pull(converter.from) {
                        Pull::Frame => converter.push(&self.input.intake.frame),
                        Pull::End => {
                            self.drained = true;
                            return false;
                        }
                        Pull::Rate => {
                            // Played out at the old rate before the new one.
                            self.switching = true;
                            converter.finish();
                            break;
                        }
                    }
                }
                if converter.done() {
                    continue;
                }
            }
            if self.run_block() {
                return true;
            }
        }
    }

    /// Resample a block; true when it gave anything to hand out.
    fn run_block(&mut self) -> bool {
        let Some(converter) = &mut self.converter else {
            return false;
        };
        let frames = converter.run(&mut self.out);
        self.at = frames.start * self.channels;
        self.end = frames.end * self.channels;
        self.at < self.end
    }
}

impl<S: Source> Iterator for Stage<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.at == self.end && !self.refill() {
            return None;
        }
        let sample = self.out[self.at];
        self.at += 1;
        Some(sample)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use rustfft::num_complex::Complex;
    use std::f64::consts::PI;

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    fn buffer(channels: u16, hz: u32, samples: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(ChannelCount::new(channels).unwrap(), rate(hz), samples)
    }

    /// All a stage hands out for `source`, played out to the end, as the
    /// decoder thread gets it.
    pub(in crate::player::source) fn convert_all<S: Source>(source: S, to: u32) -> Vec<f32> {
        let mut stage = Stage::new(source, rate(to));
        let mut out: Vec<f32> = stage.by_ref().collect();
        if stage.finish() {
            out.extend(stage.by_ref());
        }
        assert!(!stage.finish(), "played out once");
        out
    }

    /// `frames` frames of a sine at `hz`, sampled at `at`, in every channel.
    pub(in crate::player::source) fn sine(
        hz: f64,
        at: u32,
        frames: usize,
        channels: usize,
        level: f64,
    ) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let s = (level * (2.0 * PI * hz * n as f64 / f64::from(at)).sin()) as f32;
                std::iter::repeat_n(s, channels)
            })
            .collect()
    }

    /// Deterministic noise in [-0.5, 0.5).
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    }

    /// The level of each channel-0 frequency bin of `samples` (`channels`
    /// interleaved), in dB below the strongest, over `len` frames from the
    /// middle: (bin width in Hz, levels). The window is a squared
    /// Blackman-Harris, whose own leakage lies far below what is measured.
    fn spectrum(samples: &[f32], channels: usize, at: u32, len: usize) -> (f64, Vec<f64>) {
        let frames = samples.len() / channels;
        let from = (frames - len) / 2;
        let mut bins: Vec<Complex<f64>> = (0..len)
            .map(|n| {
                let x = 2.0 * PI * n as f64 / len as f64;
                let window = 0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos()
                    - 0.01168 * (3.0 * x).cos();
                let sample = f64::from(samples[(from + n) * channels]);
                Complex::new(sample * window * window, 0.0)
            })
            .collect();
        rustfft::FftPlanner::new()
            .plan_fft_forward(len)
            .process(&mut bins);
        let power: Vec<f64> = bins[..len / 2].iter().map(|b| b.norm_sqr()).collect();
        let peak = power.iter().cloned().fold(0.0, f64::max);
        let levels = power
            .iter()
            .map(|p| 10.0 * (p / peak).max(1e-30).log10())
            .collect();
        (f64::from(at) / len as f64, levels)
    }

    /// The strongest component between `low` and `high` Hz, in dB below the
    /// strongest of all.
    fn strongest(spectrum: &(f64, Vec<f64>), low: f64, high: f64) -> f64 {
        let (width, levels) = spectrum;
        levels
            .iter()
            .enumerate()
            .filter(|(bin, _)| (low..high).contains(&(*bin as f64 * width)))
            .map(|(_, level)| *level)
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// The level of a sine in channel 0 of `samples`, from its RMS over the
    /// middle half, in dB against `level`.
    fn level_db(samples: &[f32], channels: usize, level: f64) -> f64 {
        let frames = samples.len() / channels;
        let middle = frames / 4..frames * 3 / 4;
        let count = middle.len() as f64;
        let power: f64 = middle
            .map(|f| f64::from(samples[f * channels]).powi(2))
            .sum();
        20.0 * ((power / count).sqrt() * std::f64::consts::SQRT_2 / level).log10()
    }

    #[test]
    fn audio_at_the_output_rate_passes_bit_for_bit() {
        let input = noise(48_000 * 2 + 2, 7);
        let out = convert_all(buffer(2, 48_000, input.clone()), 48_000);
        assert_eq!(out, input, "untouched, not a sample more or less");
        // No delay either: from the very first sample.
        let mut stage = Stage::new(buffer(2, 48_000, input.clone()), rate(48_000));
        assert_eq!(stage.next(), Some(input[0]));
    }

    #[test]
    fn a_track_comes_out_exactly_as_long_as_it_is_at_the_new_rate() {
        for (channels, from, to) in [
            (2, 44_100, 48_000),
            (2, 48_000, 44_100),
            (2, 96_000, 48_000),
            (1, 22_050, 48_000),
            (6, 44_100, 96_000),
        ] {
            for frames in [0, 1, 146, 147, 1_000, 1_177, 4_096, 44_100, 123_457] {
                let input = noise(frames * channels, frames as u32);
                let out = convert_all(buffer(channels as u16, from, input), to);
                let expected = converted_len(frames as u64, from, to) as usize;
                assert_eq!(
                    out.len(),
                    expected * channels,
                    "{frames} frames of {channels} channels from {from} to {to} Hz"
                );
            }
        }
        assert_eq!(converted_len(44_100, 44_100, 48_000), 48_000);
        assert_eq!(converted_len(147, 44_100, 48_000), 160);
        // To the nearest frame: 1.09, 15.24 and 2.5 (halves up).
        assert_eq!(converted_len(1, 44_100, 48_000), 1);
        assert_eq!(converted_len(14, 44_100, 48_000), 15);
        assert_eq!(converted_len(5, 48_000, 24_000), 3);
    }

    #[test]
    fn the_delay_is_cut_off_to_the_sample() {
        // A sine sampled at 44.1 kHz and resampled must be the same sine
        // sampled at 48 kHz, in phase: off by one output sample, a 1 kHz sine
        // would be off by 0.13.
        let hz = 1_000.0;
        let input = sine(hz, 44_100, 44_100, 2, 0.5);
        let out = convert_all(buffer(2, 44_100, input), 48_000);
        let expected = sine(hz, 48_000, 48_000, 2, 0.5);
        assert_eq!(out.len(), expected.len());
        // The edges are held frames, not the sine going on: left out.
        let worst = (2_000 * 2..46_000 * 2)
            .map(|i| (out[i] - expected[i]).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn a_tone_keeps_its_level_and_nothing_folds_back() {
        let len = 32_768;
        for hz in [1_000.0, 10_000.0, 20_000.0] {
            let input = sine(hz, 44_100, 48_000, 1, 0.5);
            let out = convert_all(buffer(1, 44_100, input), 48_000);
            let level = level_db(&out, 1, 0.5);
            assert!(level.abs() < 0.01, "{hz} Hz: {level} dB");
            let spectrum = spectrum(&out, 1, 48_000, len);
            // Everything else in the audible band is at the noise floor of
            // single-precision floats.
            let rest = strongest(&spectrum, 20.0, hz - 200.0).max(strongest(
                &spectrum,
                hz + 200.0,
                24_000.0,
            ));
            assert!(rest < -120.0, "{hz} Hz: something at {rest} dB");
        }
        // A 21 kHz tone: rodio's linear interpolation folds its images back
        // to 17.1 and 19.2 kHz; nothing of it here.
        let input = sine(21_000.0, 44_100, 48_000, 1, 0.5);
        let out = convert_all(buffer(1, 44_100, input.clone()), 48_000);
        let ours = spectrum(&out, 1, 48_000, len);
        let alias = strongest(&ours, 20.0, 20_800.0);
        assert!(alias < -120.0, "aliases at {alias} dB");
        // Its image at 23.1 kHz (above the old Nyquist) is filtered too.
        let image = strongest(&ours, 21_200.0, 24_000.0);
        assert!(image < -120.0, "image at {image} dB");
        let linear: Vec<f32> = rodio::source::UniformSourceIterator::new(
            buffer(1, 44_100, input),
            ChannelCount::new(1).unwrap(),
            rate(48_000),
        )
        .collect();
        let theirs = spectrum(&linear, 1, 48_000, len);
        let alias = strongest(&theirs, 20.0, 20_800.0);
        assert!(alias > -60.0, "the test would not see an alias: {alias} dB");
    }

    #[test]
    fn a_gapless_boundary_between_continuous_tracks_does_not_click() {
        // One continuous piece split into two tracks, each resampled on its
        // own, against the piece resampled as a whole. Split on the grid of
        // both rates (a multiple of 147 frames), though not on a block's:
        // the second track then starts on a sample of the whole, and only
        // the boundary can differ.
        let piece: Vec<f32> = (0..44_100)
            .flat_map(|n| {
                let t = n as f64 / 44_100.0;
                let s = 0.5 * (2.0 * PI * 80.0 * t).sin()
                    + 0.2 * (2.0 * PI * 1_234.0 * t).sin()
                    + 0.05 * (2.0 * PI * 6_000.0 * t).sin();
                [s as f32, -s as f32]
            })
            .collect();
        let split = 137 * 147;
        let (a, b) = piece.split_at(split * 2);
        let whole = convert_all(buffer(2, 44_100, piece.clone()), 48_000);
        let joined = [
            convert_all(buffer(2, 44_100, a.to_vec()), 48_000),
            convert_all(buffer(2, 44_100, b.to_vec()), 48_000),
        ]
        .concat();
        assert_eq!(joined.len(), whole.len(), "no frame more or less");
        let boundary = 137 * 160;
        let error = |out: &[f32], frames: std::ops::Range<usize>| {
            frames
                .flat_map(|f| [2 * f, 2 * f + 1])
                .map(|i| (out[i] - whole[i]).abs())
                .fold(0.0, f32::max)
        };
        // Away from the boundary: the same, to the rounding of floats.
        let away =
            error(&joined, 2_000..boundary - 1_000).max(error(&joined, boundary + 1_000..46_000));
        assert!(away < 1e-4, "{away} away from the boundary");
        // At the boundary the tracks miss each other's first and last
        // samples; held edges keep that to a kink.
        let near = error(&joined, boundary - 1_000..boundary + 1_000);
        assert!(near < 0.002, "a step of {near} at the boundary");
        // Silence there instead, as rubato pads a clip of its own: a dip to
        // about half the level, a click.
        let zero_padded = |part: &[f32]| {
            let frames = part.len() / 2;
            let (block_in, _) = block_sizes(44_100, 48_000);
            let mut fft = Fft::<f32>::new_custom(
                44_100,
                48_000,
                block_in,
                1,
                2,
                WindowFunction::BlackmanHarris2,
                FixedSync::Both,
            )
            .unwrap();
            let input = InterleavedSlice::new(part, 2, frames).unwrap();
            fft.process_all(&input, frames, None).unwrap().take_data()
        };
        let silent_edges = [zero_padded(a), zero_padded(b)].concat();
        let dip = error(&silent_edges, boundary - 1_000..boundary + 1_000);
        assert!(dip > 10.0 * near, "silent edges: {dip}, held: {near}");
    }

    #[test]
    fn a_gapless_boundary_between_two_samples_shifts_by_less_than_half_of_one() {
        // As above, split where the boundary falls almost half-way between
        // two output samples (20 122 frames are 21 901.497 at 48 kHz): the
        // second track comes out 0.497 samples early, all of it — what that
        // shift is of the signal, and no more, at the boundary or after it.
        let piece: Vec<f32> = (0..44_100)
            .flat_map(|n| {
                let t = n as f64 / 44_100.0;
                let s = 0.5 * (2.0 * PI * 80.0 * t).sin()
                    + 0.2 * (2.0 * PI * 1_234.0 * t).sin()
                    + 0.05 * (2.0 * PI * 6_000.0 * t).sin();
                [s as f32, -s as f32]
            })
            .collect();
        let steepest = 2.0 * PI * (0.5 * 80.0 + 0.2 * 1_234.0 + 0.05 * 6_000.0);
        let split = 20_122;
        let boundary = converted_len(split as u64, 44_100, 48_000) as usize;
        let early = split as f64 * 48_000.0 / 44_100.0 - boundary as f64;
        assert!((early - 0.497).abs() < 0.001, "{early}");
        let (a, b) = piece.split_at(split * 2);
        let whole = convert_all(buffer(2, 44_100, piece.clone()), 48_000);
        let joined = [
            convert_all(buffer(2, 44_100, a.to_vec()), 48_000),
            convert_all(buffer(2, 44_100, b.to_vec()), 48_000),
        ]
        .concat();
        assert_eq!(joined.len(), whole.len(), "no frame more or less");
        let error = |frames: std::ops::Range<usize>| {
            frames
                .flat_map(|f| [2 * f, 2 * f + 1])
                .map(|i| f64::from((joined[i] - whole[i]).abs()))
                .fold(0.0, f64::max)
        };
        assert!(error(2_000..boundary - 1_000) < 1e-4, "before the boundary");
        let shift = early / 48_000.0 * steepest;
        let after = error(boundary - 1_000..46_000);
        assert!(
            after < shift + 0.002,
            "{after} where the shift makes {shift}"
        );
    }

    /// Consecutive tracks converted as the decoder threads do at gapless
    /// hand-overs (`feed::FeedHandle::couple`): each stage kept its start
    /// and went through a first pass at its open; at the end of one, its
    /// resampler plays out into the next one's start, and the next one
    /// starts anew from the end of the one before. All each one hands out.
    pub(in crate::player::source) fn convert_coupled<S: Source>(
        parts: Vec<S>,
        to: u32,
    ) -> Vec<Vec<f32>> {
        let mut stages: Vec<Stage<S>> = parts
            .into_iter()
            .map(|part| {
                let mut stage = Stage::new(part, rate(to));
                stage.keep_from_start(72_000);
                stage
            })
            .collect();
        let heads: Vec<Option<Head>> = stages
            .iter_mut()
            .map(|stage| loop {
                if let Some(head) = stage.take_head() {
                    break Some(head);
                }
                if stage.next().is_none() {
                    break stage.take_head();
                }
            })
            .collect();
        let mut tail: Option<Tail> = None;
        let mut outs = Vec::new();
        for (i, stage) in stages.iter_mut().enumerate() {
            stage.restart_after(tail.as_ref()).unwrap();
            let mut out: Vec<f32> = stage.by_ref().collect();
            if let Some(Some(head)) = heads.get(i + 1) {
                stage.continue_into(head);
            }
            tail = Some(stage.tail());
            if stage.finish() {
                out.extend(stage.by_ref());
            }
            outs.push(out);
        }
        outs
    }

    /// The test signal of the measurement that found the ringing: one
    /// continuous tone, at `t` seconds.
    pub(in crate::player::source) fn tone(t: f64) -> f64 {
        0.35 * (2.0 * PI * 440.0 * t).sin() + 0.06 * (2.0 * PI * 3_000.0 * t).sin()
    }

    /// `frames` frames of [`tone`] from `start` seconds on, at `hz`.
    pub(in crate::player::source) fn tone_part(
        channels: u16,
        hz: u32,
        start: f64,
        frames: usize,
    ) -> SamplesBuffer {
        let samples = (0..frames)
            .flat_map(|n| {
                let s = tone(start + n as f64 / f64::from(hz)) as f32;
                std::iter::repeat_n(s, usize::from(channels))
            })
            .collect();
        buffer(channels, hz, samples)
    }

    /// The worst difference from [`tone`] at 48 kHz in channel 0 of each of
    /// `outs`, put one after the other, within `reach` frames of where one
    /// meets the next, in dB of full scale; and away from all boundaries.
    fn boundary_residuals(outs: &[(usize, Vec<f32>)], reach: usize) -> (Vec<f64>, f64) {
        let mut joined = Vec::new();
        let mut boundaries = Vec::new();
        for (channels, out) in outs {
            joined.extend(out.iter().step_by(*channels).copied());
            boundaries.push(joined.len());
        }
        boundaries.pop();
        residuals(&joined, &boundaries, reach)
    }

    /// The worst difference of `joined` (one channel at 48 kHz) from
    /// [`tone`] within `reach` frames of each of `boundaries`, in dB of full
    /// scale; and away from them and the two ends.
    pub(in crate::player::source) fn residuals(
        joined: &[f32],
        boundaries: &[usize],
        reach: usize,
    ) -> (Vec<f64>, f64) {
        let error = |frames: std::ops::Range<usize>| {
            frames
                .map(|m| (f64::from(joined[m]) - tone(m as f64 / 48_000.0)).abs())
                .fold(1e-12, f64::max)
        };
        let db = |e: f64| 20.0 * e.log10();
        let at = boundaries
            .iter()
            .map(|b| db(error(b - reach..b + reach)))
            .collect();
        // Away from the boundaries and the two true ends.
        let mut away = 0.0f64;
        let mut from = reach;
        for b in boundaries.iter().chain([&(joined.len())]) {
            away = away.max(error(from..b - reach));
            from = b + reach;
        }
        (at, db(away))
    }

    #[test]
    fn frames_across_a_boundary_between_two_rates_are_interpolated_closely() {
        // A sine that goes on from 48 kHz before the boundary to 44.1 kHz
        // after it, looked at on either grid around the boundary and away.
        let wave = |hz: f64, rate: u32, frames: std::ops::Range<i64>| -> Vec<f32> {
            frames
                .map(|n| (2.0 * PI * hz * n as f64 / f64::from(rate)).sin() as f32)
                .collect()
        };
        let worst = |hz: f64, times: &[f64]| {
            let before = wave(hz, 48_000, -256..0);
            let after = wave(hz, 44_100, 0..256);
            let seam = Seam {
                before: Side {
                    frames: &before,
                    channels: 1,
                    rate: 48_000,
                },
                after: Side {
                    frames: &after,
                    channels: 1,
                    rate: 44_100,
                },
            };
            let out = seam.frames_at(times.iter().copied(), 1);
            out.iter()
                .zip(times)
                .map(|(s, t)| (f64::from(*s) - (2.0 * PI * hz * t).sin()).abs())
                .fold(0.0, f64::max)
        };
        let near: Vec<f64> = (-200..200).map(|i| i as f64 / 1e7).collect();
        let away: Vec<f64> = (0..200).map(|i| -0.003 + i as f64 / 1.3e6).collect();
        for (hz, bound) in [(3_000.0, 1e-6), (10_000.0, 1e-2), (15_000.0, 1.0)] {
            let (at, off) = (worst(hz, &near), worst(hz, &away));
            eprintln!("{hz} Hz: within {at:.1e} at the boundary, {off:.1e} away");
            assert!(at < bound && off < bound, "{hz} Hz: {at}, {off}");
        }
        // On a frame: that frame, in the channels asked for.
        let before = noise(64 * 2, 5);
        let seam = Seam {
            before: Side {
                frames: &before,
                channels: 2,
                rate: 48_000,
            },
            after: Side {
                frames: &[],
                channels: 1,
                rate: 44_100,
            },
        };
        let out = seam.frames_at([-47.0 / 48_000.0].into_iter(), 2);
        assert_eq!(out, [before[34], before[35]]);
        // Nothing beyond what there is: the outermost frame held.
        let out = seam.frames_at([0.5, -1.0].into_iter(), 1);
        assert_eq!(out, [before[126], before[0]]);
    }

    #[test]
    fn coupled_tracks_at_one_rate_come_out_as_one_piece() {
        // Split on the grid of both rates (137 · 147 frames), off it by
        // almost half an output sample (20 122 frames are 21 901.497 at 48
        // kHz) and by some other fraction: each part must start exactly
        // where the one before left off, wherever that falls.
        let piece: Vec<f32> = (0..3 * 44_100)
            .flat_map(|n| {
                let t = n as f64 / 44_100.0;
                let s = 0.5 * (2.0 * PI * 80.0 * t).sin()
                    + 0.2 * (2.0 * PI * 1_234.0 * t).sin()
                    + 0.05 * (2.0 * PI * 6_000.0 * t).sin();
                [s as f32, -s as f32]
            })
            .collect();
        let splits = [0, 137 * 147, 137 * 147 + 20_122, 80_001, 3 * 44_100];
        let parts: Vec<SamplesBuffer> = splits
            .windows(2)
            .map(|w| buffer(2, 44_100, piece[w[0] * 2..w[1] * 2].to_vec()))
            .collect();
        let whole = convert_all(buffer(2, 44_100, piece.clone()), 48_000);
        let outs = convert_coupled(parts, 48_000);
        // Each as long as its share of the whole: from the frame after the
        // last of the one before up to its own end, to the nearest frame.
        for (out, w) in outs.iter().zip(splits.windows(2)) {
            let share = converted_len(w[1] as u64, 44_100, 48_000)
                - converted_len(w[0] as u64, 44_100, 48_000);
            assert_eq!(out.len(), share as usize * 2, "split {w:?}");
        }
        let joined = outs.concat();
        assert_eq!(joined.len(), whole.len(), "no frame more or less");
        // Everywhere, at the boundaries too: the same, to the rounding of
        // floats.
        let worst = joined
            .iter()
            .zip(&whole)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-5, "{worst} off the piece");
        // Not so on their own: the kink and the shift of half a sample.
        let apart: Vec<f32> = splits
            .windows(2)
            .flat_map(|w| {
                convert_all(
                    buffer(2, 44_100, piece[w[0] * 2..w[1] * 2].to_vec()),
                    48_000,
                )
            })
            .collect();
        let worst_apart = apart
            .iter()
            .zip(&whole)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(
            worst_apart > 100.0 * worst,
            "{worst_apart} apart, {worst} coupled"
        );
    }

    /// The same the other way round: 48 kHz tracks (Opus is always 48 kHz)
    /// on a 44.1 kHz output, converted down.
    #[test]
    fn coupled_tracks_converted_down_come_out_as_one_piece() {
        let piece: Vec<f32> = (0..3 * 48_000)
            .flat_map(|n| {
                let t = n as f64 / 48_000.0;
                let s = 0.5 * (2.0 * PI * 80.0 * t).sin() + 0.2 * (2.0 * PI * 1_234.0 * t).sin();
                [s as f32, -s as f32]
            })
            .collect();
        // On the grid of both rates (147 · 160 · 2 frames) and off it.
        let splits = [0, 47_040, 47_040 + 20_122, 3 * 48_000];
        let parts: Vec<SamplesBuffer> = splits
            .windows(2)
            .map(|w| buffer(2, 48_000, piece[w[0] * 2..w[1] * 2].to_vec()))
            .collect();
        let whole = convert_all(buffer(2, 48_000, piece.clone()), 44_100);
        let outs = convert_coupled(parts, 44_100);
        for (out, w) in outs.iter().zip(splits.windows(2)) {
            let share = converted_len(w[1] as u64, 48_000, 44_100)
                - converted_len(w[0] as u64, 48_000, 44_100);
            assert_eq!(out.len(), share as usize * 2, "split {w:?}");
        }
        let joined = outs.concat();
        assert_eq!(joined.len(), whole.len(), "no frame more or less");
        let worst = joined
            .iter()
            .zip(&whole)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-5, "{worst} off the piece");
    }

    #[test]
    fn coupled_tracks_ring_no_more_at_their_boundaries() {
        // The measurement that found it: one tone over four parts — 44.1 kHz
        // stereo twice, 48 kHz stereo (passed untouched), 44.1 kHz mono — on
        // a 48 kHz output. A second of each here, on the grid of both rates
        // as eight seconds are.
        let parts = || {
            vec![
                tone_part(2, 44_100, 0.0, 44_100),
                tone_part(2, 44_100, 1.0, 44_100),
                tone_part(2, 48_000, 2.0, 48_000),
                tone_part(1, 44_100, 3.0, 44_100),
            ]
        };
        let channels = [2, 2, 2, 1];
        let apart: Vec<(usize, Vec<f32>)> = parts()
            .into_iter()
            .zip(channels)
            .map(|(part, ch)| (ch, convert_all(part, 48_000)))
            .collect();
        let coupled: Vec<(usize, Vec<f32>)> = channels
            .into_iter()
            .zip(convert_coupled(parts(), 48_000))
            .collect();
        for ((_, a), (_, c)) in apart.iter().zip(&coupled) {
            assert_eq!(a.len(), c.len(), "as long as before");
        }
        let (before, away_before) = boundary_residuals(&apart, 2_000);
        let (after, away) = boundary_residuals(&coupled, 2_000);
        eprintln!(
            "boundary residual, dBFS: before {before:.1?} (away {away_before:.1}), \
             coupled {after:.1?} (away {away:.1})"
        );
        // What was measured, give or take: some -45 dB at every boundary.
        assert!(before.iter().all(|db| *db > -60.0), "{before:?}");
        // Far below that now, at the boundaries as away from them.
        assert!(after.iter().all(|db| *db < -100.0), "{after:?}");
        assert!(away < -100.0, "{away}");
    }

    #[test]
    fn coupled_tracks_by_sinc_stay_as_long_and_no_worse() {
        // Rates with too little in common for the FFT: the edges are the
        // tracks' own frames, but there is no grid to meet. The second track
        // starts up to a sample early or late, as on its own (half of it the
        // boundary's, half the sinc resampler's own start), and that step is
        // most of what differs at the boundary.
        let hz = 65_537;
        let wave = |t: f64| 0.4 * (2.0 * PI * 997.0 * t).sin();
        let signal = |n: usize| wave(n as f64 / f64::from(hz)) as f32;
        let part = |frames: std::ops::Range<usize>| buffer(1, hz, frames.map(signal).collect());
        let (split, end) = (40_000, 2 * hz as usize);
        let coupled = convert_coupled(vec![part(0..split), part(split..end)], 48_000);
        let apart = [
            convert_all(part(0..split), 48_000),
            convert_all(part(split..end), 48_000),
        ];
        for (c, a) in coupled.iter().zip(&apart) {
            assert_eq!(c.len(), a.len(), "as long as on its own");
        }
        let boundary = apart[0].len();
        let worst = |out: Vec<f32>| {
            (boundary - 500..boundary + 500)
                .map(|m| (f64::from(out[m]) - wave(m as f64 / 48_000.0)).abs())
                .fold(0.0, f64::max)
        };
        let (coupled, apart) = (worst(coupled.concat()), worst(apart.concat()));
        // A sample of a 997 Hz tone at this level is 0.052.
        assert!(coupled < 0.052, "{coupled}");
        assert!(coupled <= apart + 1e-3, "{coupled}, apart {apart}");
    }

    #[test]
    fn coupled_tracks_with_treble_ring_no_more_than_held_edges() {
        // Music has some treble, which the interpolation between two rates
        // gets less exactly than a low tone: still better than a held edge.
        let signal = |t: f64| {
            0.3 * (2.0 * PI * 220.0 * t).sin()
                + 0.1 * (2.0 * PI * 1_700.0 * t).sin()
                + 0.03 * (2.0 * PI * 9_000.0 * t).sin()
                + 0.02 * (2.0 * PI * 15_300.0 * t).sin()
        };
        let part = |channels: u16, hz: u32, start: f64, frames: usize| {
            let samples = (0..frames)
                .flat_map(|n| {
                    let s = signal(start + n as f64 / f64::from(hz)) as f32;
                    std::iter::repeat_n(s, usize::from(channels))
                })
                .collect();
            buffer(channels, hz, samples)
        };
        let parts = || {
            vec![
                part(2, 44_100, 0.0, 44_100),
                part(2, 44_100, 1.0, 44_100),
                part(2, 48_000, 2.0, 48_000),
                part(1, 44_100, 3.0, 44_100),
                part(2, 96_000, 4.0, 96_000),
            ]
        };
        let channels = [2, 2, 2, 1, 2];
        let measure = |outs: Vec<Vec<f32>>| {
            let mut joined = Vec::new();
            let mut boundaries = Vec::new();
            for (out, ch) in outs.iter().zip(channels) {
                joined.extend(out.iter().step_by(ch).copied());
                boundaries.push(joined.len());
            }
            boundaries.pop();
            boundaries
                .iter()
                .map(|b| {
                    let worst = (b - 2_000..b + 2_000)
                        .map(|m| (f64::from(joined[m]) - signal(m as f64 / 48_000.0)).abs())
                        .fold(1e-12, f64::max);
                    20.0 * worst.log10()
                })
                .collect::<Vec<f64>>()
        };
        let before = measure(
            parts()
                .into_iter()
                .map(|p| convert_all(p, 48_000))
                .collect(),
        );
        let after = measure(convert_coupled(parts(), 48_000));
        eprintln!("with treble, dBFS: before {before:.1?}, coupled {after:.1?}");
        // At one rate exact; between two, a good deal better still.
        assert!(after[0] < -100.0, "{after:?}");
        for (b, a) in before.iter().zip(&after) {
            assert!(a + 10.0 < *b, "{before:?} {after:?}");
        }
    }

    #[test]
    fn mono_and_surround_keep_their_channels() {
        // Each channel resampled as it is, none mixed into another.
        let mono = sine(440.0, 22_050, 22_050, 1, 0.5);
        let out = convert_all(buffer(1, 22_050, mono), 48_000);
        assert_eq!(out.len(), 48_000);
        let surround: Vec<f32> = (0..9_600)
            .flat_map(|_| [0.1, 0.2, 0.3, 0.4, 0.5, 0.6])
            .collect();
        let out = convert_all(buffer(6, 96_000, surround), 48_000);
        assert_eq!(out.len(), 4_800 * 6);
        // A constant stays that constant, held edges and all.
        for (i, sample) in out.iter().enumerate() {
            let expected = 0.1 * (i % 6 + 1) as f32;
            assert!((sample - expected).abs() < 1e-4, "{i}: {sample}");
        }
    }

    #[test]
    fn a_seek_starts_afresh_at_the_target() {
        let input = sine(1_000.0, 44_100, 88_200, 2, 0.5);
        let mut stage = Stage::new(buffer(2, 44_100, input.clone()), rate(48_000));
        let before: Vec<f32> = stage.by_ref().take(10_000).collect();
        assert_eq!(before.len(), 10_000);
        stage
            .try_seek(std::time::Duration::from_millis(500))
            .unwrap();
        let after: Vec<f32> = stage.by_ref().collect();
        assert!(stage.finish());
        let rest: Vec<f32> = stage.by_ref().collect();
        // Exactly what the track from there resamples to on its own.
        let expected = convert_all(buffer(2, 44_100, input[22_050 * 2..].to_vec()), 48_000);
        assert_eq!([after, rest].concat(), expected);
    }

    /// A decoder whose second half comes at another rate and channel count,
    /// as a chained Ogg stream's may.
    struct Chained {
        parts: Vec<SamplesBuffer>,
    }

    impl Iterator for Chained {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            let part = self.parts.first_mut()?;
            let sample = part.next();
            // Gone once it is through, so that the format read at the end of
            // its span is the next part's, as in rodio's queue.
            if part.size_hint().0 == 0 {
                self.parts.remove(0);
            }
            sample
        }
    }

    impl Source for Chained {
        fn current_span_len(&self) -> Option<usize> {
            self.parts.first().map(|part| part.size_hint().0)
        }

        fn channels(&self) -> ChannelCount {
            self.parts
                .first()
                .map_or(ChannelCount::new(1).unwrap(), |part| part.channels())
        }

        fn sample_rate(&self) -> SampleRate {
            self.parts
                .first()
                .map_or(rate(48_000), |part| part.sample_rate())
        }

        fn total_duration(&self) -> Option<std::time::Duration> {
            None
        }

        fn try_seek(&mut self, _: std::time::Duration) -> Result<(), SeekError> {
            Err(SeekError::NotSupported {
                underlying_source: "a test decoder",
            })
        }
    }

    #[test]
    fn a_new_format_mid_stream_gets_a_resampler_of_its_own() {
        let first = noise(10_000 * 2, 1);
        let second = noise(7_000, 2);
        let chained = Chained {
            parts: vec![
                buffer(2, 44_100, first.clone()),
                buffer(1, 32_000, second.clone()),
            ],
        };
        let out = convert_all(chained, 48_000);
        // Each part converted on its own, the mono part to both sides.
        let mut expected = convert_all(buffer(2, 44_100, first), 48_000);
        let upmixed: Vec<f32> = second.iter().flat_map(|s| [*s, *s]).collect();
        expected.extend(convert_all(buffer(2, 32_000, upmixed), 48_000));
        assert_eq!(out, expected);
        // Into a stream at the output rate, it passes untouched again.
        let third = noise(5_000 * 2, 3);
        let chained = Chained {
            parts: vec![
                buffer(2, 44_100, first_part()),
                buffer(2, 48_000, third.clone()),
            ],
        };
        let out = convert_all(chained, 48_000);
        let head = converted_len(4_410, 44_100, 48_000) as usize * 2;
        assert_eq!(out.len(), head + third.len());
        assert_eq!(out[head..], third[..]);
    }

    fn first_part() -> Vec<f32> {
        noise(4_410 * 2, 4)
    }

    /// How much faster than real time the stage converts a track: the CPU
    /// it costs a decoder thread. Not a gate (it measures this machine and
    /// this build); run with `cargo test --lib resample::tests::cost --
    /// --ignored --nocapture`, in `--release` for what users run.
    #[test]
    #[ignore]
    fn cost() {
        let seconds = 60;
        for (channels, from, to) in [
            (2, 44_100, 48_000),
            (2, 48_000, 44_100),
            (2, 96_000, 48_000),
            (2, 48_000, 48_000),
            // By sinc.
            (2, 65_537, 48_000),
            (2, 44_101, 192_000),
        ] {
            let input = noise(from as usize * seconds * channels, 9);
            let plan = plan(from, to);
            let source = buffer(channels as u16, from, input);
            let started = std::time::Instant::now();
            let out = convert_all(source, to);
            let took = started.elapsed();
            assert_eq!(out.len(), to as usize * seconds * channels);
            let factor = seconds as f64 / took.as_secs_f64();
            eprintln!(
                "{channels} ch, {from} -> {to} Hz ({plan:?}): {seconds} s in {took:?}, {factor:.0}x real time, {:.2} % of a core",
                100.0 / factor
            );
        }
    }

    /// The sinc resampler, which takes over from the FFT where the rates have
    /// too little in common for a block of bounded size.
    fn convert_by_sinc(input: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
        let mut converter = Converter::with_plan(from, to, channels, Plan::Sinc).unwrap();
        let mut out = vec![0.0; converter.block_out * channels];
        let mut result = Vec::new();
        for frame in input.chunks(channels) {
            converter.push(frame);
            if converter.full() {
                let frames = converter.run(&mut out);
                result.extend_from_slice(&out[frames.start * channels..frames.end * channels]);
            }
        }
        converter.finish();
        while !converter.done() {
            let frames = converter.run(&mut out);
            result.extend_from_slice(&out[frames.start * channels..frames.end * channels]);
        }
        result
    }

    #[test]
    fn rates_with_little_in_common_are_resampled_by_sinc_in_bounded_memory() {
        // Just under the FFT's limit (a prime rate: blocks of 131 042 in),
        // and just over it.
        assert!(matches!(
            plan(65_521, 48_000),
            Some(Plan::Fft {
                block_in: 131_042,
                ..
            })
        ));
        assert_eq!(plan(65_537, 48_000), Some(Plan::Sinc));
        assert_eq!(plan(44_101, 192_000), Some(Plan::Sinc));
        // The sinc resampler: a chunk of 1 024 in, so little out.
        let converter = Converter::with_plan(65_537, 48_000, 2, Plan::Sinc).unwrap();
        assert!(converter.block_out < 2_048, "{}", converter.block_out);
        // As long as it should be, in phase, through the stage: down and up.
        let hz = 1_000.0;
        for (from, to) in [(65_537, 48_000), (44_101, 192_000)] {
            let input = sine(hz, from, from as usize, 2, 0.5);
            let out = convert_all(buffer(2, from, input.clone()), to);
            assert_eq!(out.len(), to as usize * 2);
            let expected = sine(hz, to, to as usize, 2, 0.5);
            let edge = to as usize / 20;
            let worst = (edge * 2..(to as usize - edge) * 2)
                .map(|i| f64::from((out[i] - expected[i]).abs()))
                .fold(0.0, f64::max);
            // Within half a sample of the true start (one off would be
            // twice that).
            let half_a_sample = 0.5 * 2.0 * PI * hz * 0.5 / f64::from(to);
            assert!(worst < half_a_sample, "{from} to {to} Hz: {worst}");
            assert_eq!(out, convert_by_sinc(&input, 2, from, to));
            let level = level_db(&out, 2, 0.5);
            assert!(level.abs() < 0.01, "{from} to {to} Hz: {level} dB");
        }
        // And a high tone keeps clear of the rest of the band: no alias down,
        // no image up (from 24.1 kHz on, at 192 kHz).
        for (from, to) in [(65_537, 48_000), (44_101, 192_000)] {
            let input = sine(20_000.0, from, from as usize, 1, 0.5);
            let out = convert_all(buffer(1, from, input), to);
            let spectrum = spectrum(&out, 1, to, 32_768);
            let nyquist = f64::from(to) / 2.0;
            let rest =
                strongest(&spectrum, 20.0, 19_800.0).max(strongest(&spectrum, 20_200.0, nyquist));
            assert!(rest < -100.0, "{from} to {to} Hz: something at {rest} dB");
        }
    }

    #[test]
    fn a_rate_no_music_has_is_not_resampled_and_nothing_is_allocated_for_it() {
        // 1 Hz into 48 kHz would take FFT blocks of 49 million frames, a
        // broken header's 0xDEADBEEF Hz one of 7.5 billion: neither is tried.
        assert_eq!(block_sizes(1, 48_000), (1_024, 49_152_000));
        assert_eq!(plan(1, 48_000), None);
        assert_eq!(plan(0xDEAD_BEEF, 48_000), None);
        assert_eq!(plan(48_000, 0xDEAD_BEEF), None);
        assert!(!convertible(1, 48_000));
        assert!(convertible(48_000, 48_000));
        assert!(convertible(44_100, 48_000));
        for hz in [1, 3_999, 768_001, 0xDEAD_BEEF] {
            // The stage ends at once, cleanly, without a sample.
            let out = convert_all(buffer(2, hz, vec![0.25; 2_000]), 48_000);
            assert!(out.is_empty(), "{hz} Hz: {} samples", out.len());
        }
    }

    #[test]
    fn a_sample_that_is_not_a_number_is_silence_to_the_resampler() {
        // Not a number spread over two FFT blocks would be some 50 ms of
        // nothing but NaN; as silence it is one sample of a click at most.
        let clean = sine(1_000.0, 44_100, 10_000, 2, 0.5);
        let mut broken = clean.clone();
        let mut silenced = clean.clone();
        for (at, bad) in [
            (4_001, f32::NAN),
            (7_000, f32::INFINITY),
            (7_003, f32::NEG_INFINITY),
        ] {
            broken[at] = bad;
            silenced[at] = 0.0;
        }
        let out = convert_all(buffer(2, 44_100, broken), 48_000);
        assert!(out.iter().all(|s| s.is_finite()));
        assert_eq!(out, convert_all(buffer(2, 44_100, silenced), 48_000));
    }

    #[test]
    fn block_sizes_hold_an_even_number_of_the_smallest_blocks() {
        assert_eq!(block_sizes(44_100, 48_000), (1_176, 1_280));
        assert_eq!(block_sizes(48_000, 44_100), (1_280, 1_176));
        assert_eq!(block_sizes(96_000, 48_000), (2_048, 1_024));
        assert_eq!(block_sizes(22_050, 48_000), (1_176, 2_560));
        for (from, to) in [(8_000, 48_000), (44_100, 192_000), (11_025, 44_100)] {
            let (block_in, block_out) = block_sizes(from, to);
            assert_eq!(block_in % 2, 0);
            assert_eq!(block_out % 2, 0);
            assert!(block_in.min(block_out) >= BLOCK_FRAMES);
            assert_eq!(
                block_in as u64 * u64::from(to),
                block_out as u64 * u64::from(from)
            );
        }
    }
}
