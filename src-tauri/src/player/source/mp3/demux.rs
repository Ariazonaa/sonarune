//! An MPEG audio demuxer that remembers where its frames start. MP3 has no
//! index of its own, and Symphonia's reader (0.5.5) cannot be put at a frame
//! from outside: to seek back it reads every frame header again from the
//! start of the file — in a mix of two hours, a hundred megabytes and more
//! before the source makes a sound again. This one reads the same frames the
//! same way, and notes every [`SPACING`] samples the byte a frame starts at.
//! A seek into what was read before jumps to the nearest note and walks the
//! few frames from there; one further on walks from the last note, so it
//! waits for the download up to the target as before, and notes what it
//! passes. The index grows as the track plays, and costs about 40 bytes a
//! second of audio.
//!
//! What it keeps from Symphonia: the header frame is read by Symphonia's own
//! reader, so the encoder delay and padding (LAME, FFmpeg) and the length are
//! exactly what it reads there, and every frame is trimmed by them as its
//! reader trims it — gapless playback is unchanged. Frames are decoded by its
//! decoder. Where it differs: a header frame that counts no frames gives no
//! length, and a length Symphonia only estimated (from the first frames of a
//! file without a header frame) cuts nothing off the end; a seek starts
//! decoding early enough that the first frame played after it comes out as
//! it does in playback straight through, where Symphonia's seek can leave the
//! first one or two frames short of their bit reservoir.

use super::Frame;
use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::util::trim_packet;
use symphonia::core::formats::{FormatOptions, FormatReader, Packet};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadBytes, SeekBuffered};
use symphonia::default::formats::MpaReader;

/// A frame is noted when it starts this many samples (per channel) or more
/// after the last one noted: every 16 frames of MPEG-1 layer III, 0.4 s at
/// 44.1 kHz.
const SPACING: u64 = 16 * 1152;
/// A seek walks from a note at least this many samples before its target,
/// so that the frames the target's bit reservoir reaches back into are among
/// those it walked (see [`Walked`]): [`MAX_BEGIN`] bytes of main data, which
/// at the lowest bitrate of MPEG-2 (8 kbit/s at 24 kHz, 3 bytes a frame) are
/// 85 frames of 576 samples.
const LEAD: u64 = 64 * 1152;
/// The furthest a frame's main data can begin before it: 9 bits of bytes.
const MAX_BEGIN: u64 = 511;
/// Samples before a seek's target that have to be decoded right for it to
/// come out right: two granules of layer III (one frame of MPEG-1, two of
/// MPEG-2), more than the synthesis filter's history in layers I and II.
const HISTORY: u64 = 1152;
/// What a seek reads of a frame it walks over: enough to tell a header
/// frame (the id and the flags after the side information, or `VBRI` and
/// its version) and where its main data begins.
const WALK_BYTES: usize = 4 + 32 + 8;

/// A frame noted: where its header starts, and its time stamp (samples per
/// channel from the first frame, the encoder delay included).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Point {
    pos: u64,
    ts: u64,
}

/// Where the frames read so far start, every [`SPACING`] samples or so, in
/// order. The first frame is always in it.
#[derive(Debug)]
struct FrameIndex {
    points: Vec<Point>,
}

impl FrameIndex {
    fn new(first: u64) -> Self {
        Self {
            points: vec![Point { pos: first, ts: 0 }],
        }
    }

    /// A frame was read at `pos` with time stamp `ts`. Every frame is read
    /// in order from a frame noted before, so the time stamp is right.
    fn note(&mut self, pos: u64, ts: u64) {
        let last = self.last();
        if ts >= last.ts + SPACING {
            self.points.push(Point { pos, ts });
        }
    }

    /// The last frame noted that starts at or before `ts`.
    fn before(&self, ts: u64) -> Point {
        let after = self.points.partition_point(|point| point.ts <= ts);
        self.points[after.saturating_sub(1)]
    }

    fn last(&self) -> Point {
        self.points[self.points.len() - 1]
    }
}

/// A frame walked over on the way to a seek's target.
#[derive(Debug, Clone, Copy, Default)]
struct Walked {
    at: Point,
    /// Bytes of main data in it (the frame without its header, CRC and side
    /// information): what the bit reservoir of the frames after it can draw
    /// on.
    main: u64,
    /// How far back into the frames before it its own main data begins.
    begin: u64,
}

/// What the reader is up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Reading,
    /// At the end of the stream.
    Ended,
    /// A read failed (the download broke off, or the decoder thread cut it
    /// short for a seek). Where the stream stands is not known any more: only
    /// a seek goes on from here.
    Broken,
}

/// The bytes of the stream, as Symphonia reads them. Shared, so that the
/// reader can be had back when Symphonia's reader turns the stream down
/// (see [`IndexedMp3::new`]); only the decoder thread ever locks it.
struct Bytes<R> {
    inner: Arc<Mutex<R>>,
    len: u64,
}

impl<R> Bytes<R> {
    fn inner(&self) -> MutexGuard<'_, R> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<R: Read> Read for Bytes<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner().read(buf)
    }
}

impl<R: Seek> Seek for Bytes<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner().seek(pos)
    }
}

impl<R: Read + Seek + Send + Sync> MediaSource for Bytes<R> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// An MPEG audio stream with a known length, decoded by Symphonia and
/// seeked through a frame index of its own (see the module docs).
pub struct IndexedMp3 {
    stream: MediaSourceStream,
    decoder: Box<dyn Decoder>,
    /// The encoder delay, and the length after it, in samples per channel:
    /// what Symphonia read from the header frame. No length when it only
    /// estimated one.
    delay: u64,
    length: Option<u64>,
    channels: ChannelCount,
    rate: SampleRate,
    total_duration: Option<Duration>,
    index: FrameIndex,
    /// The time stamp of the next frame read.
    next_ts: u64,
    /// The frame read last, header included.
    frame: Vec<u8>,
    /// Decoded audio, interleaved, and how much of it was handed out.
    buffer: Option<SampleBuffer<f32>>,
    offset: usize,
    /// Samples per channel still to throw away after a seek: from the frame
    /// decoding starts at up to the target.
    skip: u64,
    state: State,
    /// The frames a seek walked over last that the next frame's bit
    /// reservoir may reach back into, oldest first.
    recent: VecDeque<Walked>,
}

impl IndexedMp3 {
    /// The stream `reader` of `len` bytes, whose first frame starts at
    /// `start` (see `super::Bitrate::Variable`); `counted`: a header frame
    /// counts its frames. When Symphonia does not take it after all: the
    /// error, and the reader back (somewhere in the stream).
    pub fn new<R>(
        reader: R,
        len: u64,
        start: u64,
        counted: bool,
    ) -> Result<Self, (Error, Option<R>)>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let reader = Arc::new(Mutex::new(reader));
        Self::open(reader.clone(), len, start, counted).map_err(|e| {
            // Whatever else held the reader went with the error.
            let reader = Arc::into_inner(reader)
                .map(|reader| reader.into_inner().unwrap_or_else(PoisonError::into_inner));
            (e, reader)
        })
    }

    fn open<R>(reader: Arc<Mutex<R>>, len: u64, start: u64, counted: bool) -> Result<Self, Error>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mut stream =
            MediaSourceStream::new(Box::new(Bytes { inner: reader, len }), Default::default());
        stream.seek(SeekFrom::Start(start))?;
        // Symphonia's reader takes in the header frame — or, without one,
        // estimates the length — and stops in front of the first audio frame.
        let options = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let reader = MpaReader::try_new(stream, &options)?;
        let params = reader
            .tracks()
            .first()
            .map(|track| track.codec_params.clone())
            .ok_or(Error::Unsupported("no MPEG audio track"))?;
        let stream = Box::new(reader).into_inner();
        let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default())?;
        let rate = params
            .sample_rate
            .and_then(SampleRate::new)
            .ok_or(Error::Unsupported("no sample rate"))?;
        let channels = params
            .channels
            .and_then(|channels| u16::try_from(channels.count()).ok())
            .and_then(ChannelCount::new)
            .ok_or(Error::Unsupported("no channels"))?;
        // Not from an estimate: nothing goes by it (the player has the
        // server's duration), and it would only be wrong.
        let total_duration = params
            .n_frames
            .filter(|&frames| counted && frames > 0)
            .map(|frames| duration(frames, rate));
        let first = stream.pos();
        Ok(Self {
            stream,
            decoder,
            delay: u64::from(params.delay.unwrap_or(0)),
            length: params.n_frames.filter(|_| counted),
            channels,
            rate,
            total_duration,
            index: FrameIndex::new(first),
            next_ts: 0,
            frame: Vec::new(),
            buffer: None,
            offset: 0,
            skip: 0,
            state: State::Reading,
            recent: VecDeque::new(),
        })
    }

    /// The next audio frame into `self.frame` — whole, or its first
    /// [`WALK_BYTES`] when `whole` is false (the rest is passed over): where
    /// its header starts and what it says. A header frame in the middle of
    /// the stream is passed over as Symphonia's reader passes over it. None
    /// at the end.
    fn read_frame(&mut self, whole: bool) -> io::Result<Option<(u64, Frame)>> {
        let at_end = |e: io::Error| match e.kind() {
            io::ErrorKind::UnexpectedEof => Ok(None),
            _ => Err(e),
        };
        loop {
            let (pos, word, frame) = match sync(&mut self.stream) {
                Ok(found) => found,
                Err(e) => return at_end(e),
            };
            self.frame.clear();
            self.frame.extend_from_slice(&word.to_be_bytes());
            let read = if whole {
                frame.len
            } else {
                frame.len.min(WALK_BYTES)
            };
            self.frame.resize(read, 0);
            let mut result = ReadBytes::read_buf_exact(&mut self.stream, &mut self.frame[4..]);
            if result.is_ok() && read < frame.len {
                result = self.stream.ignore_bytes((frame.len - read) as u64);
            }
            if let Err(e) = result {
                return at_end(e);
            }
            if !is_header_frame(&self.frame, &frame) {
                return Ok(Some((pos, frame)));
            }
        }
    }

    /// The next frame as a packet, trimmed as Symphonia's reader trims it.
    fn next_packet(&mut self) -> Option<Packet> {
        if self.state != State::Reading {
            return None;
        }
        match self.read_frame(true) {
            Ok(Some((pos, frame))) => {
                self.index.note(pos, self.next_ts);
                let ts = self.next_ts;
                self.next_ts += frame.samples;
                let mut packet = Packet::new_from_slice(0, ts, frame.samples, &self.frame);
                trim_packet(&mut packet, self.delay as u32, self.length);
                Some(packet)
            }
            Ok(None) => {
                self.state = State::Ended;
                None
            }
            Err(_) => {
                self.state = State::Broken;
                None
            }
        }
    }

    /// Decode on until there is audio to hand out. False at the end.
    fn refill(&mut self) -> bool {
        let channels = usize::from(self.channels.get());
        loop {
            let Some(packet) = self.next_packet() else {
                return false;
            };
            // A frame left out still takes its time: one before a seek's
            // target is no less to skip.
            let lost = packet.dur();
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                // A frame that cannot be decoded is left out, as rodio
                // leaves it out; so is one that runs out of bits.
                Err(Error::DecodeError(_) | Error::IoError(_)) => {
                    self.skip = self.skip.saturating_sub(lost);
                    continue;
                }
                Err(_) => {
                    self.state = State::Broken;
                    return false;
                }
            };
            let frames = decoded.frames() as u64;
            // Trimmed away whole (the delay, the padding).
            if frames == 0 {
                continue;
            }
            // Of another channel count than the stream began with.
            if decoded.spec().channels.count() != channels {
                self.skip = self.skip.saturating_sub(lost);
                continue;
            }
            let skipped = self.skip.min(frames);
            self.skip -= skipped;
            if skipped == frames {
                continue;
            }
            let fits = self
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.capacity() >= decoded.capacity() * channels);
            if !fits {
                self.buffer = Some(SampleBuffer::new(
                    decoded.capacity() as u64,
                    *decoded.spec(),
                ));
            }
            let Some(buffer) = self.buffer.as_mut() else {
                return false;
            };
            buffer.copy_interleaved_ref(decoded);
            self.offset = skipped as usize * channels;
            return true;
        }
    }

    /// Put the stream at `pos`, within what it holds in memory if it can.
    fn reposition(&mut self, pos: u64) -> io::Result<()> {
        let here = self.stream.pos();
        let behind = self.stream.read_buffer_len() as u64;
        let ahead = self.stream.unread_buffer_len() as u64;
        if pos + behind >= here && pos <= here + ahead && self.stream.seek_buffered(pos) == pos {
            return Ok(());
        }
        self.stream.seek(SeekFrom::Start(pos)).map(drop)
    }

    /// Put the reader at the frame decoding has to start at for the frame
    /// with time stamp `required` to come out right: walk there from the
    /// last frame noted well before it. The frames walked, or None at the
    /// end of the stream.
    fn walk_to(&mut self, required: u64) -> io::Result<Option<usize>> {
        let from = self.index.before(required.saturating_sub(LEAD));
        self.reposition(from.pos)?;
        self.next_ts = from.ts;
        self.state = State::Reading;
        self.recent.clear();
        let mut walked = 0usize;
        // How many frames before the target have to come out right.
        let mut back;
        loop {
            let Some((pos, frame)) = self.read_frame(false)? else {
                return Ok(None);
            };
            self.index.note(pos, self.next_ts);
            self.recent.push_back(Walked {
                at: Point {
                    pos,
                    ts: self.next_ts,
                },
                main: main_data_len(&frame) as u64,
                begin: main_data_begin(&self.frame, &frame),
            });
            walked += 1;
            back = HISTORY.div_ceil(frame.samples) as usize;
            // Kept: what the frame `back` before the newest can reach back
            // into.
            while self.recent.len() > back + 1 {
                let before = self.recent.len() - 1 - back;
                let held: u64 = self.recent.range(1..before).map(|w| w.main).sum();
                if held < MAX_BEGIN {
                    break;
                }
                self.recent.pop_front();
            }
            if self.next_ts + frame.samples > required {
                break;
            }
            self.next_ts += frame.samples;
        }
        // The target frame's first granule overlaps the last one before it,
        // and the synthesis filter runs on the samples of that one, which
        // overlaps the one before: those two granules need all of their
        // main data. Start where that begins (what comes after begins later).
        let needed = self.recent.len().saturating_sub(1 + back);
        let mut start = needed;
        let mut have = 0;
        while have < self.recent[needed].begin && start > 0 {
            start -= 1;
            have += self.recent[start].main;
        }
        let at = self.recent[start].at;
        self.reposition(at.pos)?;
        self.next_ts = at.ts;
        Ok(Some(walked))
    }
}

/// The time `frames` samples per channel take at `rate`.
fn duration(frames: u64, rate: SampleRate) -> Duration {
    let nanos = u128::from(frames) * 1_000_000_000 / u128::from(rate.get());
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// The sample (per channel) `time` falls on at `rate`.
fn timestamp(time: Duration, rate: SampleRate) -> u64 {
    let samples = time.as_nanos() * u128::from(rate.get()) / 1_000_000_000;
    u64::try_from(samples).unwrap_or(u64::MAX)
}

/// The next frame header, found as Symphonia finds one: the first word that
/// passes its quick check, then its full one — a word that fails that is
/// left behind whole. Where it starts, the word, and what it says.
fn sync(stream: &mut MediaSourceStream) -> io::Result<(u64, u32, Frame)> {
    loop {
        let mut word = 0u32;
        loop {
            while word & 0xffe0_0000 != 0xffe0_0000 {
                word = word << 8 | u32::from(stream.read_u8()?);
            }
            // Version, layer, bitrate and sample rate not reserved.
            if word >> 19 & 3 != 1
                && word >> 17 & 3 != 0
                && word >> 12 & 0xf != 0xf
                && word >> 10 & 3 != 3
            {
                break;
            }
            word = word << 8 | u32::from(stream.read_u8()?);
        }
        if let Some(frame) = Frame::parse(word) {
            return Ok((stream.pos() - 4, word, frame));
        }
    }
}

/// Whether `frame`, whose first bytes (at least [`WALK_BYTES`] of it, or
/// all) are `data`, is a `Xing`, `Info` or `VBRI` header frame by the checks
/// Symphonia's reader makes before it passes one over.
fn is_header_frame(data: &[u8], frame: &Frame) -> bool {
    if !frame.layer3 {
        return false;
    }
    let side = 4 + frame.side_info_len();
    let zeroed = |upto: usize| data.get(4..upto).is_some_and(|b| b.iter().all(|&b| b == 0));
    // Xing or Info, after zeroed side information: a header frame when the
    // fields its flags name fit into the frame.
    if frame.len >= side + 8
        && matches!(data.get(side..side + 4), Some(b"Xing" | b"Info"))
        && zeroed(side)
    {
        let Some(flags) = data.get(side + 4..side + 8) else {
            return false;
        };
        let flags = u32::from_be_bytes(flags.try_into().unwrap());
        let fields = [(1, 4), (2, 4), (4, 100), (8, 4)]
            .iter()
            .filter(|(flag, _)| flags & flag != 0)
            .map(|(_, len)| len)
            .sum::<usize>();
        return frame.len >= side + 8 + fields;
    }
    // VBRI, always 32 bytes after the header, version 1.
    frame.len >= 62
        && data.get(36..40) == Some(b"VBRI".as_slice())
        && zeroed(36)
        && data.get(40..42) == Some([0, 1].as_slice())
}

/// Bytes of main data in a frame of layer III (the rest has none).
fn main_data_len(frame: &Frame) -> usize {
    if !frame.layer3 {
        return 0;
    }
    let crc = if frame.crc { 2 } else { 0 };
    frame.len.saturating_sub(4 + crc + frame.side_info_len())
}

/// How far back from a layer III frame `data` its main data begins, in bytes
/// of the frames before it.
fn main_data_begin(data: &[u8], frame: &Frame) -> u64 {
    if !frame.layer3 {
        return 0;
    }
    let at = if frame.crc { 6 } else { 4 };
    match data.get(at..at + 2) {
        Some(&[high, low]) if frame.mpeg1 => u64::from(u16::from_be_bytes([high, low]) >> 7),
        Some(&[high, _]) => u64::from(high),
        _ => 0,
    }
}

impl Iterator for IndexedMp3 {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(buffer) = &self.buffer {
                if let Some(&sample) = buffer.samples().get(self.offset) {
                    self.offset += 1;
                    return Some(sample);
                }
            }
            if !self.refill() {
                return None;
            }
        }
    }
}

impl Source for IndexedMp3 {
    /// Its format does not change: the decoder refuses a frame of another
    /// sample rate or channel count than the first, and it is left out.
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
        self.total_duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let begun = Instant::now();
        let mut desired = timestamp(pos, self.rate);
        // Past the end is the end, as with rodio.
        if let Some(length) = self.length {
            desired = desired.min(length);
        }
        let reach = self.index.last().ts.saturating_sub(self.delay);
        let walked = match self.walk_to(desired.saturating_add(self.delay)) {
            Ok(walked) => walked,
            Err(e) => {
                self.state = State::Broken;
                return Err(SeekError::Other(Arc::new(e)));
            }
        };
        self.decoder.reset();
        self.buffer = None;
        self.offset = 0;
        match walked {
            Some(walked) => {
                self.skip = desired.saturating_sub(self.next_ts.saturating_sub(self.delay));
                tracing::debug!(
                    "MP3 seek to {:.1} s: {walked} frame(s) walked from {}, in {:?}",
                    pos.as_secs_f64(),
                    if desired > reach {
                        format!(
                            "the end of the index ({:.1} s)",
                            duration(reach, self.rate).as_secs_f64()
                        )
                    } else {
                        "the index".to_owned()
                    },
                    begun.elapsed()
                );
            }
            None => {
                self.state = State::Ended;
                tracing::debug!("MP3 seek to {:.1} s: past the end", pos.as_secs_f64());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{stream, FRAME, KBPS_128, KBPS_32, KBPS_320};
    use super::super::{bitrate, Bitrate};
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Short VBR MP3s from FFmpeg's libmp3lame, 3 s of a chirp with a quiet
    /// start, each with an ID3v2 tag and a Xing/LAME header frame:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i "aevalsrc='if(lt(t\,0.7)\,0.002\,0.5)*sin(2*PI*(200*t+300*t*t))|0.3*sin(2*PI*440*t)+0.2*gt(t\,1.5)*sin(2*PI*3000*t)':s=44100:d=3" -c:a libmp3lame -q:a 4 -map_metadata -1 chirp-vbr.mp3
    /// ```
    ///
    /// and the same with `-ac 1` (mono), `-ar 22050` (MPEG-2), and
    /// `-ac 1 -ar 24000 -q:a 6` (MPEG-2, mono).
    struct Fixture {
        name: &'static str,
        data: &'static [u8],
        rate: usize,
        channels: usize,
    }

    const FIXTURES: [Fixture; 4] = [
        Fixture {
            name: "44.1 kHz stereo",
            data: include_bytes!("testdata/chirp-vbr.mp3"),
            rate: 44_100,
            channels: 2,
        },
        Fixture {
            name: "44.1 kHz mono",
            data: include_bytes!("testdata/chirp-vbr-mono.mp3"),
            rate: 44_100,
            channels: 1,
        },
        Fixture {
            name: "MPEG-2, 22.05 kHz stereo",
            data: include_bytes!("testdata/chirp-vbr-22k.mp3"),
            rate: 22_050,
            channels: 2,
        },
        Fixture {
            name: "MPEG-2, 24 kHz mono",
            data: include_bytes!("testdata/chirp-vbr-24k-mono.mp3"),
            rate: 24_000,
            channels: 1,
        },
    ];

    impl Fixture {
        fn open(&self) -> IndexedMp3 {
            open(Cursor::new(self.data), self.data.len() as u64)
        }

        /// Its length in samples per channel.
        fn frames(&self) -> usize {
            3 * self.rate
        }
    }

    fn open<R: Read + Seek + Send + Sync + 'static>(mut reader: R, len: u64) -> IndexedMp3 {
        match bitrate(&mut reader, len) {
            Bitrate::Variable { start, counted } => {
                match IndexedMp3::new(reader, len, start, counted) {
                    Ok(decoder) => decoder,
                    Err((e, _)) => panic!("does not open: {e}"),
                }
            }
            other => panic!("not taken for variable: {other:?}"),
        }
    }

    /// rodio's decoder, as the player opens it for an MP3 it seeks
    /// accurately.
    fn rodio_decoder<R: Read + Seek + Send + Sync + 'static>(
        reader: R,
        len: u64,
    ) -> rodio::Decoder<R> {
        rodio::Decoder::builder()
            .with_data(reader)
            .with_byte_len(len)
            .with_gapless(true)
            .with_coarse_seek(false)
            .build()
            .expect("decodes")
    }

    fn bits(samples: impl IntoIterator<Item = f32>) -> Vec<u32> {
        samples.into_iter().map(f32::to_bits).collect()
    }

    #[test]
    fn it_plays_what_symphonia_plays_trimmed_to_the_track() {
        for fixture in &FIXTURES {
            let name = fixture.name;
            let decoder = fixture.open();
            assert_eq!(usize::from(decoder.channels().get()), fixture.channels);
            assert_eq!(decoder.sample_rate().get() as usize, fixture.rate);
            assert_eq!(
                decoder.total_duration(),
                Some(Duration::from_secs(3)),
                "{name}"
            );
            let ours = bits(decoder);
            let len = fixture.data.len() as u64;
            let theirs = bits(rodio_decoder(Cursor::new(fixture.data), len));
            // The encoder's delay and padding are gone: the track as encoded.
            assert_eq!(ours.len(), fixture.channels * fixture.frames(), "{name}");
            assert!(
                ours == theirs,
                "{name}: the same samples as rodio's decoder"
            );
        }
    }

    #[test]
    fn a_seek_lands_on_the_sample_playback_straight_through_reaches() {
        for fixture in &FIXTURES {
            let (name, channels) = (fixture.name, fixture.channels);
            let straight = bits(fixture.open());
            let mut decoder = fixture.open();
            // Ahead, back, back to the start, near the end, at it, past it —
            // and into the encoder delay, where the first frame is cut.
            for ms in [2000, 500, 1234, 0, 13, 2990, 3000, 9000, 1] {
                decoder.try_seek(Duration::from_millis(ms)).expect("seeks");
                let at = (ms as usize * fixture.rate / 1000).min(fixture.frames());
                let heard = bits(decoder.by_ref().take(4096 * channels));
                let expected =
                    &straight[channels * at..(channels * (at + 4096)).min(straight.len())];
                assert!(heard == expected, "{name}: after a seek to {ms} ms");
            }
            // And on to the end from there.
            decoder.try_seek(Duration::from_millis(1500)).unwrap();
            assert!(
                bits(decoder) == straight[channels * fixture.rate * 3 / 2..],
                "{name}: from 1.5 s to the end"
            );
        }
    }

    #[test]
    fn a_seek_far_past_the_end_is_the_end() {
        let mut decoder = FIXTURES[0].open();
        decoder.try_seek(Duration::MAX).unwrap();
        assert_eq!(decoder.next(), None);
        // Uncounted: no length to stop at.
        let data = stream(None, &[KBPS_32, KBPS_320].repeat(50));
        let len = data.len() as u64;
        let mut decoder = open(Cursor::new(data), len);
        assert_eq!(decoder.total_duration(), None, "no estimate");
        decoder.try_seek(Duration::MAX).unwrap();
        assert_eq!(decoder.next(), None);
        decoder.try_seek(Duration::ZERO).unwrap();
        assert_eq!(decoder.count(), 2 * 100 * FRAME, "nothing cut off the end");
    }

    #[test]
    fn a_stream_symphonia_turns_down_gives_its_reader_back() {
        // Nothing where the first frame was said to be.
        let data = FIXTURES[0].data;
        let len = data.len() as u64;
        match IndexedMp3::new(Cursor::new(data), len, len, true) {
            Err((_, Some(reader))) => assert_eq!(reader.get_ref().len(), data.len()),
            Err((e, None)) => panic!("the reader is lost: {e}"),
            Ok(_) => panic!("opened"),
        }
    }

    #[test]
    fn a_frame_lost_on_the_way_to_a_target_still_counts() {
        // A frame of another sample rate, which the decoder refuses, right
        // before the target: decoding starts at it (its main data begins at
        // its own header), and it is no audio, but it takes its time.
        let word = 0xfffb_0000 | KBPS_128 << 12 | 1 << 10;
        let mut bad = vec![0u8; Frame::parse(word).unwrap().len];
        bad[..4].copy_from_slice(&word.to_be_bytes());
        let good = stream(None, &[KBPS_32, KBPS_320].repeat(100));
        let half = good.len() / 2;
        let mut data = good[..half].to_vec();
        data.extend(&bad);
        data.extend(&good[half..]);
        let len = data.len() as u64;
        // Played straight through, the lost frame is left out.
        assert_eq!(
            open(Cursor::new(data.clone()), len).count(),
            2 * 200 * FRAME
        );
        // The frames before it alternate 104 and 1044 bytes.
        let lost = (0..)
            .scan(0, |at, i| {
                *at += [104, 1044][i % 2];
                Some(*at)
            })
            .position(|at| at == half)
            .unwrap()
            + 1;
        let mut decoder = open(Cursor::new(data), len);
        // Heard something first, as a seek in a playing track has.
        decoder.next();
        let at = Duration::from_nanos(((lost + 1) * FRAME) as u64 * 1_000_000_000 / 44_100 + 1);
        decoder.try_seek(at).unwrap();
        assert_eq!(decoder.count(), 2 * (201 - lost - 1) * FRAME);
    }

    /// What was read through a [`Counted`] reader, and from where.
    #[derive(Clone, Default)]
    struct Reads {
        bytes: Arc<AtomicU64>,
        lowest: Arc<AtomicU64>,
        /// A read from here on fails, as one cut short while it waits for
        /// the download does (0: none does).
        wall: Arc<AtomicU64>,
    }

    impl Reads {
        fn reset(&self) {
            self.bytes.store(0, Ordering::Relaxed);
            self.lowest.store(u64::MAX, Ordering::Relaxed);
        }

        fn bytes(&self) -> u64 {
            self.bytes.load(Ordering::Relaxed)
        }

        fn lowest(&self) -> u64 {
            self.lowest.load(Ordering::Relaxed)
        }
    }

    struct Counted {
        inner: Cursor<Vec<u8>>,
        reads: Reads,
    }

    impl Read for Counted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let at = self.inner.position();
            let wall = self.reads.wall.load(Ordering::Relaxed);
            if wall > 0 && at + buf.len() as u64 > wall {
                return Err(io::Error::other("read cut short for a seek"));
            }
            let n = self.inner.read(buf)?;
            if n > 0 {
                self.reads.bytes.fetch_add(n as u64, Ordering::Relaxed);
                self.reads.lowest.fetch_min(at, Ordering::Relaxed);
            }
            Ok(n)
        }
    }

    impl Seek for Counted {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// `frames` silent frames at varying bitrates (23 000 are ten minutes),
    /// opened with a counter on its reads; and how many frames it has.
    fn long_stream(frames: usize) -> (IndexedMp3, Reads, u64, usize) {
        let bitrates: Vec<u32> = (0..frames)
            .map(|i| [KBPS_32, KBPS_128, KBPS_320, KBPS_32][i % 4])
            .collect();
        let data = stream(Some(b"Xing"), &bitrates);
        let len = data.len() as u64;
        let reads = Reads::default();
        let reader = Counted {
            inner: Cursor::new(data),
            reads: reads.clone(),
        };
        (open(reader, len), reads, len, frames)
    }

    /// The start of frame `frame` (rounded up to the nanosecond, so that it
    /// does not fall on the sample before).
    fn at(frame: usize) -> Duration {
        Duration::from_nanos((frame as u64 * FRAME as u64 * 1_000_000_000).div_ceil(44_100))
    }

    #[test]
    fn a_seek_back_jumps_instead_of_reading_from_the_start() {
        let (mut decoder, reads, len, frames) = long_stream(23_000);

        // Ahead to near the end: read up to there, once.
        reads.reset();
        decoder.try_seek(at(frames - 100)).unwrap();
        assert!(reads.bytes() > len * 9 / 10);
        assert_eq!(decoder.by_ref().count(), 2 * 100 * FRAME);

        // Back to the middle: a few frames before it are read, nothing else.
        reads.reset();
        decoder.try_seek(at(frames / 2)).unwrap();
        assert!(reads.bytes() < 128 * 1024, "{} bytes read", reads.bytes());
        assert!(
            reads.lowest() + 128 * 1024 > len / 2,
            "read from {}, the middle is at {}",
            reads.lowest(),
            len / 2
        );
        assert_eq!(decoder.by_ref().count(), 2 * (frames - frames / 2) * FRAME);

        // Back to the start, and ahead again into what was read before.
        decoder.try_seek(Duration::ZERO).unwrap();
        reads.reset();
        decoder.try_seek(at(frames - 1000)).unwrap();
        assert!(reads.bytes() < 128 * 1024, "{} bytes read", reads.bytes());
        assert_eq!(decoder.count(), 2 * 1000 * FRAME);
    }

    #[test]
    fn a_seek_ahead_of_what_was_read_walks_on_from_there() {
        let (mut decoder, reads, len, frames) = long_stream(23_000);
        decoder.try_seek(at(frames / 4)).unwrap();
        // Ahead past the index: what lies before its end is not read again
        // (on a download, nothing is asked for there).
        reads.reset();
        decoder.try_seek(at(frames / 2)).unwrap();
        assert!(
            reads.lowest() + 64 * 1024 > len / 4,
            "read from {}",
            reads.lowest()
        );
        assert_eq!(decoder.count(), 2 * (frames - frames / 2) * FRAME);
    }

    #[test]
    fn a_read_cut_short_leaves_the_index_whole() {
        let (mut decoder, reads, len, frames) = long_stream(3_000);
        // What has not arrived yet: a seek there fails, and so does reading
        // on from there, until a seek works again.
        reads.wall.store(len / 2, Ordering::Relaxed);
        assert!(decoder.try_seek(at(frames * 3 / 4)).is_err());
        assert_eq!(decoder.by_ref().next(), None);
        // Played up to the wall, cut short in the middle of a frame.
        decoder.try_seek(at(frames / 4)).unwrap();
        assert!(decoder.by_ref().count() < 2 * frames * FRAME / 2);
        // Arrived: both from what was read and on past it, to the sample.
        reads.wall.store(0, Ordering::Relaxed);
        for frame in [frames / 3, frames * 3 / 4, 10, frames / 2 - 3] {
            decoder.try_seek(at(frame)).unwrap();
            assert_eq!(
                decoder.by_ref().count(),
                2 * (frames - frame) * FRAME,
                "after a seek to frame {frame}"
            );
        }
    }

    #[test]
    fn a_header_frame_in_the_middle_is_passed_over() {
        // Two streams one after the other, as a cut-together file may be:
        // the second's header frame holds no audio and takes no time.
        let mut data = stream(None, &[KBPS_32, KBPS_320].repeat(200));
        data.extend(stream(Some(b"Info"), &[KBPS_128; 400]));
        let len = data.len() as u64;
        let straight = open(Cursor::new(data.clone()), len).count();
        assert_eq!(straight, 2 * 800 * FRAME);
        let mut decoder = open(Cursor::new(data), len);
        decoder.try_seek(at(600)).unwrap();
        assert_eq!(decoder.by_ref().count(), 2 * 200 * FRAME);
        decoder.try_seek(at(100)).unwrap();
        assert_eq!(decoder.count(), 2 * 700 * FRAME);
    }

    #[test]
    fn the_index_keeps_one_frame_in_so_many() {
        let mut index = FrameIndex::new(100);
        let mut pos = 100;
        for frame in 0..1000u64 {
            index.note(pos, frame * 1152);
            pos += 417;
        }
        // Noting a frame again, or one before the last noted, changes
        // nothing.
        index.note(100 + 417 * 500, 500 * 1152);
        assert_eq!(index.points.len(), 1000 / 16 + 1);
        assert_eq!(index.before(0), Point { pos: 100, ts: 0 });
        assert_eq!(
            index.before(17 * 1152),
            Point {
                pos: 100 + 16 * 417,
                ts: 16 * 1152
            }
        );
        assert_eq!(index.before(u64::MAX).ts, 992 * 1152);
    }

    /// The seek times the TODO asks about: a two-hour VBR MP3 (LAME
    /// `-q:a 4`, a chirp, f = 100 + 0.5556·t Hz), to 100 min, back to 10,
    /// ahead to 100 again — rodio's accurate seek against the index. Needs
    /// ffmpeg with libmp3lame on the PATH; the file is made once in the temp
    /// directory:
    ///
    /// `cargo test --lib long_mix_seek_times -- --ignored --nocapture`
    #[test]
    #[ignore = "needs ffmpeg; makes a file of about 100 MB"]
    fn long_mix_seek_times() {
        let path = std::env::temp_dir().join("sonarune-test-chirp-noise-2h-vbr.mp3");
        if !path.exists() {
            let made = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-f", "lavfi", "-i"])
                // Noise under the chirp, for the bitrate of music.
                .arg("aevalsrc=0.4*sin(2*PI*(100*t+0.2778*t*t))+0.2*(random(0)-0.5):s=44100:d=7200")
                .args(["-ac", "2", "-c:a", "libmp3lame", "-q:a", "4"])
                .arg(&path)
                .status()
                .expect("ffmpeg runs");
            assert!(made.success(), "ffmpeg made the file");
        }
        let len = std::fs::metadata(&path).unwrap().len();
        let file = || std::fs::File::open(&path).unwrap();
        let minutes = |m: u64| Duration::from_secs(m * 60);
        fn timed(what: &str, seek: impl FnOnce()) {
            let begun = Instant::now();
            seek();
            println!("{what}: {:?}", begun.elapsed());
        }
        fn first(source: &mut dyn Iterator<Item = f32>) -> Vec<u32> {
            source.take(8192).map(f32::to_bits).collect()
        }

        println!("{:.1} MB", len as f64 / 1e6);
        let mut theirs = rodio_decoder(file(), len);
        timed("rodio, to 100 min", || {
            theirs.try_seek(minutes(100)).unwrap()
        });
        timed("rodio, back to 10 min", || {
            theirs.try_seek(minutes(10)).unwrap()
        });
        let theirs_at_10 = first(&mut theirs);
        timed("rodio, ahead to 100 min", || {
            theirs.try_seek(minutes(100)).unwrap()
        });
        timed("rodio, back to 99 min", || {
            theirs.try_seek(minutes(99)).unwrap()
        });

        let mut ours = open(file(), len);
        timed("index, to 100 min", || ours.try_seek(minutes(100)).unwrap());
        timed("index, back to 10 min", || {
            ours.try_seek(minutes(10)).unwrap()
        });
        let ours_at_10 = first(&mut ours);
        timed("index, ahead to 100 min", || {
            ours.try_seek(minutes(100)).unwrap()
        });
        timed("index, back to 99 min", || {
            ours.try_seek(minutes(99)).unwrap()
        });
        println!("index: {} points", ours.index.points.len());

        // Both land on the same audio. rodio's first frame or two after a
        // seek can be short of their bit reservoir, so they are compared
        // after those; and it counts the samples to skip in an f32, which at
        // 10 min is off by a few.
        let shift = (-16..=16)
            .find(|&shift: &i32| {
                let at = (4096 + 2 * shift) as usize;
                ours_at_10[4096..6144] == theirs_at_10[at..at + 2048]
            })
            .expect("the same audio");
        println!("rodio is {shift} frame(s) off");
    }
}
