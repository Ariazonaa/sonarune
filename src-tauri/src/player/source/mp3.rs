//! Whether an MP3's bytes say where its time is, and how a seek finds its
//! way in one whose bytes do not. Symphonia seeks an MP3 one of two ways.
//! Accurately: it reads every frame from the start (backwards) or from the
//! current one up to the target — on the decoder thread, the source silent
//! meanwhile, waiting for the download up to there. Coarsely: it jumps to the
//! byte the target's share of the file points at and takes the time from that
//! same share. The jump is right only at a constant bitrate. In a
//! variable-bitrate file the assumed time is off by however much the bitrate
//! before the landing point differs from the file's average — seconds, in a
//! track with a quiet intro or fade-out — and it stays off: the position and
//! the lyrics run that far from the audio, and the gapless trim, which goes
//! by those times, cuts the end of the track or plays the encoder's padding.
//! So only a file that is constant is seeked coarsely (by Symphonia). Every
//! other one is read by a demuxer of our own that notes where its frames
//! start as it goes ([`demux`]): a seek back jumps to a frame it has seen
//! instead of reading the file again from its start.

mod demux;

pub(super) use demux::IndexedMp3;

use std::io::{self, Read, Seek, SeekFrom};

/// How far past the tags in front the first frame may start.
const SYNC_WINDOW: usize = 64 * 1024;
/// A file without a header frame counts as constant when this many frames
/// in a row keep one bitrate (Symphonia estimates such a file's length from
/// as many).
const FRAMES_TO_COMPARE: usize = 16;
/// How much is read at a time.
const STEP: usize = 4 * 1024;

/// What the first frames of an MPEG audio stream say about seeking in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bitrate {
    /// A constant bitrate that holds to the end of the file, so its byte
    /// share is its time: Symphonia seeks it coarsely.
    Constant,
    /// Anything else that has frames: read and seeked by [`IndexedMp3`].
    /// `start` is where the first frame (a header frame, if there is one)
    /// begins; `counted`, whether a header frame is there to count the
    /// stream's frames, so that Symphonia does not estimate them.
    Variable { start: u64, counted: bool },
    /// No MPEG audio recognised, or too short to tell — and too short to
    /// seek in anyway.
    Unknown,
}

/// What `reader`, an MPEG audio stream of `len` bytes, is:
///
/// - constant: an `Info` frame (what LAME and FFmpeg write into a
///   constant-bitrate file) whose byte count, if it has one, covers the rest
///   of the file; or no header frame at all, and the first frames keep one
///   bitrate;
/// - variable: a `Xing` or `VBRI` frame, an `Info` frame that does not count
///   to the end, or no header frame and frames of more than one bitrate.
///
/// Reads only from the start, in order: on a download that is how the bytes
/// arrive. Leaves the reader at the start.
pub(super) fn bitrate<R: Read + Seek>(reader: &mut R, len: u64) -> Bitrate {
    let verdict = check(reader, len).unwrap_or(Bitrate::Unknown);
    // The decoder reads from where this leaves the reader.
    match reader.seek(SeekFrom::Start(0)) {
        Ok(_) => verdict,
        Err(_) => Bitrate::Unknown,
    }
}

fn check<R: Read + Seek>(reader: &mut R, len: u64) -> io::Result<Bitrate> {
    reader.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    // Where `buf` starts in the stream.
    let mut base = 0u64;
    // ID3v2 tags in front of the audio, read through rather than skipped (a
    // jump ahead would point a download elsewhere).
    for _ in 0..4 {
        if !fill(reader, &mut buf, 10)? {
            return Ok(Bitrate::Unknown);
        }
        let Some(tag) = id3v2_len(&buf) else { break };
        if let Some(rest) = tag.checked_sub(buf.len() as u64) {
            if io::copy(&mut reader.by_ref().take(rest), &mut io::sink())? != rest {
                return Ok(Bitrate::Unknown);
            }
            buf.clear();
        } else {
            buf.drain(..tag as usize);
        }
        base += tag;
    }

    // The first frame: a header whose frame is followed by another like it.
    let mut at = 0;
    let first = loop {
        if at > SYNC_WINDOW || !fill(reader, &mut buf, at + 4)? {
            return Ok(Bitrate::Unknown);
        }
        if let Some(frame) = Frame::parse(word(&buf, at)) {
            let next = at + frame.len;
            if fill(reader, &mut buf, next + 4)?
                && Frame::parse(word(&buf, next)).is_some_and(|other| other.like(&frame))
            {
                break frame;
            }
        }
        at += 1;
    };
    let start = base + at as u64;
    let variable = |counted| Ok(Bitrate::Variable { start, counted });

    if first.layer3 {
        if !fill(reader, &mut buf, at + first.len)? {
            return Ok(Bitrate::Unknown);
        }
        let frame = &buf[at..at + first.len];
        let id = |offset: usize| frame.get(offset..offset + 4);
        // Where Symphonia looks for them, so the verdict matches what it saw.
        let tag = first.side_info_len() + 4;
        if id(36) == Some(b"VBRI".as_slice()) || id(tag) == Some(b"Xing".as_slice()) {
            return variable(true);
        }
        if id(tag) == Some(b"Info".as_slice()) {
            let rest = len.saturating_sub(start);
            if info_covers_the_rest(&frame[tag + 4..], rest, first.len) {
                return Ok(Bitrate::Constant);
            }
            return variable(true);
        }
    }

    // No header frame: the bitrate of the first frames decides.
    let mut frame = first;
    for _ in 1..FRAMES_TO_COMPARE {
        at += frame.len;
        if !fill(reader, &mut buf, at + 4)? {
            return Ok(Bitrate::Unknown);
        }
        match Frame::parse(word(&buf, at)) {
            Some(next) if next.like(&first) && next.bitrate == first.bitrate => frame = next,
            _ => return variable(false),
        }
    }
    Ok(Bitrate::Constant)
}

/// An `Info` frame's fields after its id (flags, then the counts they name)
/// against `rest`, the bytes from the frame's start to the end of the file.
/// Trailing tags or junk beyond what it counts would shift the byte share; a
/// frame or two (whether the count includes the header frame) and an ID3v1
/// tag are within reach, as is a thousandth of the file.
fn info_covers_the_rest(fields: &[u8], rest: u64, frame_len: usize) -> bool {
    let read = |at: usize| {
        fields
            .get(at..at + 4)
            .map(|bytes| u64::from(u32::from_be_bytes(bytes.try_into().unwrap())))
    };
    let Some(flags) = read(0) else { return false };
    if flags & 2 == 0 {
        // No byte count to go by.
        return true;
    }
    let Some(bytes) = read(if flags & 1 != 0 { 8 } else { 4 }) else {
        return false;
    };
    rest.abs_diff(bytes) <= (bytes / 1000).max(2 * frame_len as u64)
}

/// `buf` holds `upto` bytes or more, reading on as needed. False at the end
/// of the stream.
fn fill(reader: &mut impl Read, buf: &mut Vec<u8>, upto: usize) -> io::Result<bool> {
    if buf.len() < upto {
        let target = upto.max(buf.len() + STEP);
        reader
            .by_ref()
            .take((target - buf.len()) as u64)
            .read_to_end(buf)?;
    }
    Ok(buf.len() >= upto)
}

fn word(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(buf[at..at + 4].try_into().unwrap())
}

/// The length of the ID3v2 tag `head` (its first ten bytes) starts, if it
/// is one.
fn id3v2_len(head: &[u8]) -> Option<u64> {
    let size = head.get(6..10)?;
    if &head[..3] != b"ID3"
        || head[3] == 0xff
        || head[4] == 0xff
        || size.iter().any(|b| b & 0x80 != 0)
    {
        return None;
    }
    let size = size.iter().fold(0u64, |n, b| n << 7 | u64::from(*b));
    let footer = if head[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// What a frame header says, as far as telling frames apart and reading
/// them goes.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// Version, layer and sample rate bits: the same in every frame.
    kind: u32,
    mono: bool,
    mpeg1: bool,
    layer3: bool,
    /// The bitrate index.
    bitrate: u32,
    /// The whole frame, header included.
    len: usize,
    /// A CRC follows the header.
    crc: bool,
    /// Samples per channel it holds.
    samples: u64,
}

impl Frame {
    /// The header `word`, if it is a valid one — by Symphonia's rules
    /// (free-format bitrates are not: it cannot play them).
    fn parse(word: u32) -> Option<Self> {
        // By layer: III, II, I.
        static KBPS_V1: [[u32; 15]; 3] = [
            [
                0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
            ],
            [
                0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
            ],
            [
                0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
            ],
        ];
        static KBPS_V2: [[u32; 15]; 3] = [
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
            [
                0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
            ],
        ];
        if word & 0xffe0_0000 != 0xffe0_0000 {
            return None;
        }
        let version = word >> 19 & 3;
        // 1 is layer III, 3 layer I.
        let layer = word >> 17 & 3;
        let bitrate = word >> 12 & 0xf;
        let rate = word >> 10 & 3;
        if version == 1 || layer == 0 || bitrate == 0 || bitrate == 0xf || rate == 3 {
            return None;
        }
        let mpeg1 = version == 3;
        let table = if mpeg1 { &KBPS_V1 } else { &KBPS_V2 };
        let bits_per_second = table[layer as usize - 1][bitrate as usize] * 1000;
        let mono = word >> 6 & 3 == 3;
        // Layer II leaves out some bitrates for mono, and others for two
        // channels.
        let kbps = bits_per_second / 1000;
        if layer == 2
            && if mono {
                matches!(kbps, 224 | 256 | 320 | 384)
            } else {
                matches!(kbps, 32 | 48 | 56 | 80)
            }
        {
            return None;
        }
        let sample_rate = [44_100, 48_000, 32_000][rate as usize]
            >> match version {
                3 => 0,
                2 => 1,
                _ => 2,
            };
        let padding = word >> 9 & 1;
        let len = match layer {
            3 => (12 * bits_per_second / sample_rate + padding) * 4,
            1 if !mpeg1 => 72 * bits_per_second / sample_rate + padding,
            _ => 144 * bits_per_second / sample_rate + padding,
        };
        Some(Self {
            kind: word & 0x001e_0c00,
            mono,
            mpeg1,
            layer3: layer == 1,
            bitrate,
            len: len as usize,
            crc: word >> 16 & 1 == 0,
            samples: match layer {
                3 => 384,
                2 => 1152,
                _ if mpeg1 => 1152,
                _ => 576,
            },
        })
    }

    /// The same kind of stream: what Symphonia checks before it trusts a
    /// header found by searching.
    fn like(&self, other: &Self) -> bool {
        self.kind == other.kind && self.mono == other.mono
    }

    fn side_info_len(&self) -> usize {
        match (self.mpeg1, self.mono) {
            (true, true) => 17,
            (true, false) => 32,
            (false, true) => 9,
            (false, false) => 17,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source as _;
    use std::io::Cursor;
    use std::time::Duration;

    /// Bitrate indices of MPEG-1 layer III.
    pub(super) const KBPS_32: u32 = 1;
    pub(super) const KBPS_128: u32 = 9;
    pub(super) const KBPS_320: u32 = 14;
    /// Samples per channel in a frame.
    pub(super) const FRAME: usize = 1152;

    /// A frame of silence: MPEG-1 layer III, 44.1 kHz, stereo, no CRC;
    /// zeroed side information, so no main data.
    fn frame(bitrate: u32) -> Vec<u8> {
        let word = 0xfffb_0000 | bitrate << 12;
        let mut frame = vec![0u8; Frame::parse(word).unwrap().len];
        frame[..4].copy_from_slice(&word.to_be_bytes());
        frame
    }

    /// The header frame of a stream (`Xing`, `Info` or `VBRI`) counting
    /// `frames` frames and `bytes` bytes.
    fn header_frame(id: &[u8; 4], frames: u32, bytes: u32) -> Vec<u8> {
        let mut frame = frame(KBPS_128);
        frame[36..40].copy_from_slice(id);
        if id == b"VBRI" {
            frame[40..42].copy_from_slice(&1u16.to_be_bytes());
            frame[46..50].copy_from_slice(&bytes.to_be_bytes());
            frame[50..54].copy_from_slice(&frames.to_be_bytes());
        } else {
            // Flags: frame count and byte count.
            frame[40..44].copy_from_slice(&3u32.to_be_bytes());
            frame[44..48].copy_from_slice(&frames.to_be_bytes());
            frame[48..52].copy_from_slice(&bytes.to_be_bytes());
        }
        frame
    }

    /// A stream: an optional header frame (with the right counts), then
    /// frames at these bitrates.
    pub(super) fn stream(header: Option<&[u8; 4]>, bitrates: &[u32]) -> Vec<u8> {
        let audio: Vec<u8> = bitrates
            .iter()
            .flat_map(|&bitrate| frame(bitrate))
            .collect();
        let Some(id) = header else { return audio };
        let len = frame(KBPS_128).len() + audio.len();
        let mut out = header_frame(id, bitrates.len() as u32, len as u32);
        out.extend(audio);
        out
    }

    /// An ID3v2.3 tag of `size` bytes after its header, full of bytes that
    /// look like frame headers.
    fn id3v2(size: usize) -> Vec<u8> {
        let mut tag = b"ID3\x03\x00\x00".to_vec();
        tag.extend((0..4).rev().map(|i| (size >> (7 * i) & 0x7f) as u8));
        tag.extend((0..size).map(|i| if i % 3 == 0 { 0xff } else { 0xfb }));
        tag
    }

    fn bitrate_of(data: Vec<u8>) -> Bitrate {
        let len = data.len() as u64;
        let mut reader = Cursor::new(data);
        let verdict = bitrate(&mut reader, len);
        assert_eq!(reader.position(), 0, "the reader is left at the start");
        verdict
    }

    fn verdict(data: Vec<u8>) -> bool {
        bitrate_of(data) == Bitrate::Constant
    }

    #[test]
    fn a_constant_bitrate_file_is_told_from_a_variable_one() {
        let cbr = vec![KBPS_128; 40];
        let vbr: Vec<u32> = (0..40)
            .map(|i| [KBPS_32, KBPS_128, KBPS_320][i % 3])
            .collect();
        assert!(verdict(stream(Some(b"Info"), &cbr)), "an Info frame");
        assert!(verdict(stream(None, &cbr)), "no header frame, one bitrate");
        assert!(!verdict(stream(Some(b"Xing"), &vbr)), "a Xing frame");
        assert!(!verdict(stream(Some(b"VBRI"), &vbr)), "a VBRI frame");
        assert!(
            !verdict(stream(None, &vbr)),
            "no header frame, bitrates vary"
        );
        // Too short to tell, and nothing to seek in anyway.
        assert_eq!(bitrate_of(stream(None, &cbr[..4])), Bitrate::Unknown);
        assert_eq!(
            bitrate_of(b"not an mp3 at all".repeat(100)),
            Bitrate::Unknown
        );
    }

    #[test]
    fn a_variable_stream_says_where_its_first_frame_is_and_whether_it_is_counted() {
        let vbr: Vec<u32> = (0..40).map(|i| [KBPS_32, KBPS_320][i % 2]).collect();
        let mut data = id3v2(500);
        let tag = data.len() as u64;
        data.extend(stream(Some(b"Xing"), &vbr));
        assert_eq!(
            bitrate_of(data),
            Bitrate::Variable {
                start: tag,
                counted: true
            }
        );
        // Junk in front of the first frame is skipped.
        let mut data = vec![0u8; 100];
        data.extend(stream(None, &vbr));
        assert_eq!(
            bitrate_of(data),
            Bitrate::Variable {
                start: 100,
                counted: false
            }
        );
        // An Info frame that does not count to the end.
        let mut data = stream(Some(b"Info"), &[KBPS_128; 400]);
        data.extend(vec![0u8; 50_000]);
        assert_eq!(
            bitrate_of(data),
            Bitrate::Variable {
                start: 0,
                counted: true
            }
        );
    }

    #[test]
    fn tags_in_front_are_read_past_whatever_they_hold() {
        // Shorter and longer than one read.
        for size in [500, 50_000] {
            let mut data = id3v2(size);
            data.extend(stream(Some(b"Info"), &[KBPS_128; 40]));
            assert!(verdict(data), "{size}");
            let mut data = id3v2(size);
            data.extend(stream(Some(b"Xing"), &[KBPS_32, KBPS_320].repeat(20)));
            assert!(!verdict(data), "{size}");
        }
    }

    #[test]
    fn an_info_frame_must_count_to_the_end_of_the_file() {
        let mut data = stream(Some(b"Info"), &[KBPS_128; 400]);
        // An ID3v1 tag at the end: within reach.
        data.extend([0u8; 128]);
        assert!(verdict(data.clone()));
        // A picture-sized tag at the end shifts the byte share.
        data.extend(vec![0u8; 50_000]);
        assert!(!verdict(data));
    }

    /// `count` samples per channel, as a duration at 44.1 kHz.
    fn at(count: usize) -> Duration {
        Duration::from_secs_f64(count as f64 / 44_100.0)
    }

    /// Decode `data` as the player does, seek to `to`, and count the frames
    /// (sample pairs) to the end.
    fn frames_after_seek(data: Vec<u8>, to: Duration) -> usize {
        let len = data.len() as u64;
        let mut decoder =
            super::super::symphonia_decoder(Cursor::new(data), Some(len), true).expect("decodes");
        decoder.try_seek(to).expect("seeks");
        decoder.count() / 2
    }

    #[test]
    fn a_seek_in_a_variable_bitrate_mp3_keeps_the_rest_of_the_track() {
        // A quiet half at 32 kbit/s, then a busy one at 320: by byte share,
        // the middle of the file lies far into the second half.
        let data = stream(Some(b"Xing"), &[[KBPS_32; 200], [KBPS_320; 200]].concat());
        let frames = frames_after_seek(data, at(200 * FRAME));
        assert!(
            frames.abs_diff(200 * FRAME) <= FRAME,
            "{frames} frames after the middle, not {}",
            200 * FRAME
        );
    }

    #[test]
    fn a_seek_in_a_constant_bitrate_mp3_lands_on_its_target() {
        let data = stream(Some(b"Info"), &[KBPS_128; 400]);
        assert!(verdict(data.clone()), "seeked coarsely");
        let frames = frames_after_seek(data, at(200 * FRAME));
        assert!(
            frames.abs_diff(200 * FRAME) <= 4 * FRAME,
            "{frames} frames after the middle, not {}",
            200 * FRAME
        );
    }
}
