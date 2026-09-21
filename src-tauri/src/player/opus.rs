//! Ogg-Opus playback source for rodio. Symphonia has no Opus decoder, but
//! Jellyfin streams Opus in Ogg containers; this demuxes with the pure-Rust
//! `ogg` crate and decodes with libopus (`opus` crate). Self-contained: no
//! project-internal imports.

use ogg::reading::OggReadError;
use ogg::PacketReader;
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Opus always decodes at 48 kHz regardless of the original input rate.
const SAMPLE_RATE: u32 = 48_000;
const SAMPLE_RATE_NZ: NonZeroU32 = match NonZeroU32::new(SAMPLE_RATE) {
    Some(v) => v,
    None => unreachable!(),
};
/// RFC 7845 section 4.4: decode and discard at least 80 ms before a seek
/// target so the decoder state converges.
const SEEK_PREROLL: u64 = 3_840;
/// Largest Opus frame: 120 ms at 48 kHz, per channel.
const MAX_FRAME: usize = 5_760;
/// Bytes the Ogg layer may consume while assembling one packet.
///
/// A legitimate Opus packet is at most ~1.3 kB per frame; even a pathological
/// page chain stays far below this. The `ogg` crate glues continued pages
/// together with no limit of its own, so a server whose packet never ends
/// would otherwise buffer the entire stream in RAM until the process dies.
const MAX_PACKET_BYTES: u64 = 4 * 1024 * 1024;
/// Byte budget for the two header packets. OpusTags carries embedded cover art
/// as a base64 METADATA_BLOCK_PICTURE, which easily runs to several MB — far
/// past any audio packet. Still bounded, for the reason above.
const MAX_HEADER_PACKET_BYTES: u64 = 64 * 1024 * 1024;
/// Bound for the OpusHead output gain (dB), which is applied to every sample.
const OUTPUT_GAIN_LIMIT_DB: f32 = 24.0;

/// Bytes the demuxer consumed since the last completed packet, and how many it
/// may. Shared: the reader counts, the source resets it whenever a packet does
/// come out and picks the limit for the next one.
struct PacketBudget {
    used: AtomicU64,
    limit: AtomicU64,
}

impl PacketBudget {
    fn new(limit: u64) -> Self {
        Self {
            used: AtomicU64::new(0),
            limit: AtomicU64::new(limit),
        }
    }

    /// Start counting the next packet, which may take `limit` bytes.
    fn reset(&self, limit: u64) {
        self.limit.store(limit, Ordering::Relaxed);
        self.used.store(0, Ordering::Relaxed);
    }

    /// Count `n` consumed bytes; false once they exceed the limit.
    fn consume(&self, n: u64) -> bool {
        let total = self.used.fetch_add(n, Ordering::Relaxed).saturating_add(n);
        total <= self.limit.load(Ordering::Relaxed)
    }
}

/// Reader wrapper that fails once too many bytes have been consumed without
/// the demuxer completing a packet (see [`PacketBudget`]).
struct Limited<R> {
    inner: R,
    budget: Arc<PacketBudget>,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if !self.budget.consume(n as u64) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ogg packet exceeds the size limit",
            ));
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for Limited<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.budget.used.store(0, Ordering::Relaxed);
        self.inner.seek(pos)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpusProbeError {
    #[error("not an Ogg stream: {0}")]
    Ogg(#[from] OggReadError),
    #[error("stream contains no packets")]
    Empty,
    #[error("first packet is not an OpusHead")]
    NotOpus,
    #[error("malformed Opus headers: {0}")]
    Malformed(&'static str),
    #[error("unsupported layout: mapping family {family}, {channels} channels")]
    UnsupportedLayout { family: u8, channels: u8 },
    #[error("libopus init failed: {0}")]
    Opus(#[from] opus::Error),
}

/// Fields of an RFC 7845 identification header we act on.
struct OpusHead {
    channels: u8,
    pre_skip: u16,
    /// Q7.8 dB output gain, already converted to a linear factor.
    gain: f32,
}

fn parse_opus_head(data: &[u8]) -> Result<OpusHead, OpusProbeError> {
    if data.len() < 19 {
        return Err(OpusProbeError::Malformed("OpusHead too short"));
    }
    // RFC 7845 section 5.1: versions 0..=15 share this layout.
    if data[8] > 15 {
        return Err(OpusProbeError::Malformed("incompatible OpusHead version"));
    }
    let channels = data[9];
    let mapping_family = data[18];
    // Mapping family 0 is plain mono/stereo; anything else needs the
    // multistream decoder, which we do not support.
    if mapping_family != 0 || !(1..=2).contains(&channels) {
        return Err(OpusProbeError::UnsupportedLayout {
            family: mapping_family,
            channels,
        });
    }
    let pre_skip = u16::from_le_bytes([data[10], data[11]]);
    let gain_q78 = i16::from_le_bytes([data[16], data[17]]);
    // Q7.8 dB spans ±128 dB, i.e. a linear factor up to ~2.5 million. That is
    // multiplied into every sample, so a corrupt or hostile header would mean
    // instant full-scale noise. Real files sit within a few dB of zero.
    let gain_db = (f32::from(gain_q78) / 256.0).clamp(-OUTPUT_GAIN_LIMIT_DB, OUTPUT_GAIN_LIMIT_DB);
    Ok(OpusHead {
        channels,
        pre_skip,
        gain: 10f32.powf(gain_db / 20.0),
    })
}

/// Probe `reader` for an Ogg-Opus stream. On success return a playable source;
/// on failure give the (rewound) reader back so the caller can try Symphonia.
///
/// `seekable`: whether the underlying byte stream has a known end and random
/// access. A live transcode wrapped in StreamDownload claims Seek but would
/// BLOCK on reads at not-yet-downloaded offsets — and rodio runs try_seek on
/// the audio callback thread, so a bisection over such a stream stalls all
/// audio. Callers must pass false when the content length is unknown.
pub fn probe<R>(reader: R, seekable: bool) -> Result<OggOpusSource<R>, (R, OpusProbeError)>
where
    R: Read + Seek + Send + Sync + 'static,
{
    let budget = Arc::new(PacketBudget::new(MAX_HEADER_PACKET_BYTES));
    let mut packets = PacketReader::new(Limited {
        inner: reader,
        budget: budget.clone(),
    });
    match probe_headers(&mut packets, &budget) {
        Ok((serial, head, decoder)) => {
            let mut source = OggOpusSource {
                packets,
                budget,
                decoder,
                serial,
                channels: NonZeroU16::new(u16::from(head.channels))
                    .expect("probe_headers guarantees 1-2 channels"),
                pre_skip: head.pre_skip,
                gain: head.gain,
                gp: 0,
                emit_from: u64::from(head.pre_skip),
                pcm: VecDeque::new(),
                scratch: vec![0.0; MAX_FRAME * usize::from(head.channels)],
                ended: false,
                seekable,
            };
            // Decode ahead: a player that starts paused never pulls a sample,
            // and an empty buffer would read as an unknown span length.
            source.refill();
            Ok(source)
        }
        Err(e) => {
            let mut reader = packets.into_inner().inner;
            if let Err(se) = reader.seek(SeekFrom::Start(0)) {
                tracing::warn!("failed to rewind reader after failed opus probe: {se}");
            }
            Err((reader, e))
        }
    }
}

fn probe_headers<R: Read + Seek>(
    packets: &mut PacketReader<R>,
    budget: &PacketBudget,
) -> Result<(u32, OpusHead, opus::Decoder), OpusProbeError> {
    budget.reset(MAX_HEADER_PACKET_BYTES);
    let first = packets.read_packet()?.ok_or(OpusProbeError::Empty)?;
    if !first.data.starts_with(b"OpusHead") {
        return Err(OpusProbeError::NotOpus);
    }
    let head = parse_opus_head(&first.data)?;
    let serial = first.stream_serial();
    budget.reset(MAX_HEADER_PACKET_BYTES);
    let tags = packets
        .read_packet()?
        .ok_or(OpusProbeError::Malformed("missing OpusTags"))?;
    if tags.stream_serial() != serial || !tags.data.starts_with(b"OpusTags") {
        return Err(OpusProbeError::Malformed("second packet is not OpusTags"));
    }
    let channels = if head.channels == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    };
    let decoder = opus::Decoder::new(SAMPLE_RATE, channels)?;
    Ok((serial, head, decoder))
}

/// Streaming Ogg-Opus source: decodes packet by packet into a small buffer.
/// Interleaved f32 at 48 kHz; implements `Iterator` + `rodio::Source`.
pub struct OggOpusSource<R: Read + Seek> {
    packets: PacketReader<Limited<R>>,
    /// Shared with the reader; see [`PacketBudget`].
    budget: Arc<PacketBudget>,
    decoder: opus::Decoder,
    serial: u32,
    channels: NonZeroU16,
    pre_skip: u16,
    gain: f32,
    /// Granules (48 kHz frames, pre-skip included) decoded so far.
    gp: u64,
    /// First granule allowed out: pre-skip at start, target after a seek.
    emit_from: u64,
    pcm: VecDeque<f32>,
    scratch: Vec<f32>,
    ended: bool,
    seekable: bool,
}

impl<R: Read + Seek> std::fmt::Debug for OggOpusSource<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OggOpusSource")
            .field("serial", &self.serial)
            .field("channels", &self.channels)
            .field("pre_skip", &self.pre_skip)
            .field("gp", &self.gp)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> OggOpusSource<R> {
    /// Decode one audio packet and queue its emittable samples. Granule
    /// bookkeeping decides both the head discard (pre-skip / seek target)
    /// and the final-page end trim (RFC 7845 section 4.5).
    fn process_packet(
        &mut self,
        data: &[u8],
        last_in_page: bool,
        last_in_stream: bool,
        absgp: u64,
    ) {
        let ch = usize::from(self.channels.get());
        let frames = match self.decoder.decode_float(data, &mut self.scratch, false) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("opus decode failed, ending stream: {e}");
                self.ended = true;
                return;
            }
        };
        let start = self.emit_from.saturating_sub(self.gp).min(frames as u64) as usize;
        let end = if last_in_stream {
            // The final page's granule position is the true end of the audio.
            absgp.saturating_sub(self.gp).min(frames as u64) as usize
        } else {
            frames
        };
        if start < end {
            let gain = self.gain;
            self.pcm
                .extend(self.scratch[start * ch..end * ch].iter().map(|s| s * gain));
        }
        // Page granule positions are authoritative; resync on page ends.
        self.gp = if last_in_page && absgp != u64::MAX {
            absgp
        } else {
            // A hostile page can put the granule position right below MAX.
            self.gp.saturating_add(frames as u64)
        };
        if last_in_stream {
            self.ended = true;
        }
    }

    fn refill(&mut self) {
        while self.pcm.is_empty() && !self.ended {
            // A completed packet resets the per-packet byte budget. Only audio
            // is read here: the probe and `try_seek` get past the headers.
            self.budget.reset(MAX_PACKET_BYTES);
            let packet = match self.packets.read_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.ended = true;
                    return;
                }
                Err(e) => {
                    tracing::warn!("ogg read failed, ending stream: {e}");
                    self.ended = true;
                    return;
                }
            };
            // Header pages carry granule position 0 (RFC 7845 section 4);
            // this also skips OpusHead/OpusTags re-read after a seek to 0.
            // Zero-length packets are legal Ogg framing but would be decoded
            // as packet loss (libopus PLC injects synthetic audio) — skip.
            if packet.stream_serial() != self.serial
                || packet.absgp_page() == 0
                || packet.data.is_empty()
            {
                continue;
            }
            self.process_packet(
                &packet.data,
                packet.last_in_page(),
                packet.last_in_stream(),
                packet.absgp_page(),
            );
        }
    }
}

impl<R: Read + Seek> Iterator for OggOpusSource<R> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pcm.is_empty() {
            self.refill();
        }
        let sample = self.pcm.pop_front();
        // Keep the next packet decoded, so `current_span_len` never has to
        // answer "unknown" in the middle of the stream.
        if self.pcm.is_empty() {
            self.refill();
        }
        sample
    }
}

impl<R: Read + Seek> rodio::Source for OggOpusSource<R> {
    fn current_span_len(&self) -> Option<usize> {
        // Parameters never change mid-stream, but rodio's queue hands this
        // value to its format converter: `None` there keeps this track's
        // 48 kHz layout for everything later appended to the same player (a
        // 44.1 kHz FLAC after an Opus track would play sped up). Report the
        // decoded whole frames; `probe`, `try_seek` and `next` refill, so the
        // buffer is only empty at the end.
        match (self.pcm.len(), self.ended) {
            (0, true) => Some(0),
            (0, false) => None,
            (buffered, _) => Some(buffered),
        }
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        SAMPLE_RATE_NZ
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        use rodio::source::SeekError;

        // Live transcodes must not be bisected: StreamDownload's Seek would
        // BLOCK (not fail) on undownloaded offsets, and rodio runs this on
        // the audio callback thread. NotSupported leaves the source intact.
        if !self.seekable {
            return Err(SeekError::NotSupported {
                underlying_source: std::any::type_name::<Self>(),
            });
        }

        // Remember where we were so a failed bisection can restore playback
        // instead of leaving the ogg reader mid-stream at a random offset.
        let restore_pos = self
            .packets
            .get_mut()
            .stream_position()
            .map_err(|e| SeekError::Other(Arc::new(e)))?;

        // Granule positions count 48 kHz samples including pre-skip.
        let target = u64::from(self.pre_skip) + (pos.as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
        let goal = target.saturating_sub(SEEK_PREROLL);
        match self.packets.seek_absgp(Some(self.serial), goal) {
            Ok(true) => {}
            Ok(false) => {
                // Target lies past the last page: saturate to end-of-stream.
                self.pcm.clear();
                self.ended = true;
                return Ok(());
            }
            Err(e) => {
                if let Err(re) = self.packets.seek_bytes(SeekFrom::Start(restore_pos)) {
                    tracing::warn!("could not restore position after failed seek: {re}");
                    self.ended = true;
                }
                return Err(SeekError::Other(Arc::new(e)));
            }
        }

        self.pcm.clear();
        self.ended = false;
        self.emit_from = target;
        if let Err(e) = self.decoder.reset_state() {
            tracing::warn!("opus decoder reset failed after seek: {e}");
        }

        // The landing page starts at least SEEK_PREROLL before the target but
        // its start granule is unknown until a page-end granule position is
        // seen. Buffer packets up to the first page end (which dates the end
        // of the last buffered packet), then decode them: the pre-target part
        // is the pre-roll, discarded by `emit_from`.
        let mut pending = Vec::new();
        let mut total_frames: u64 = 0;
        // Goal 0 sends seek_absgp back to the first page of the stream: the
        // header packets come first again, OpusTags with its cover art.
        let mut limit = if goal == 0 {
            MAX_HEADER_PACKET_BYTES
        } else {
            MAX_PACKET_BYTES
        };
        let anchor = loop {
            self.budget.reset(limit);
            let packet = match self.packets.read_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.ended = true;
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("ogg read failed after seek, ending stream: {e}");
                    self.ended = true;
                    return Ok(());
                }
            };
            if packet.stream_serial() != self.serial
                || packet.absgp_page() == 0
                || packet.data.is_empty()
            {
                continue;
            }
            limit = MAX_PACKET_BYTES;
            total_frames += self.decoder.get_nb_samples(&packet.data).unwrap_or(0) as u64;
            let ends_page = packet.last_in_page();
            let absgp = packet.absgp_page();
            pending.push(packet);
            if ends_page {
                break absgp;
            }
        };
        self.gp = anchor.saturating_sub(total_frames);
        for packet in pending {
            if self.ended {
                break;
            }
            self.process_packet(
                &packet.data,
                packet.last_in_page(),
                packet.last_in_stream(),
                packet.absgp_page(),
            );
        }
        // The buffered packets may have been pre-roll only; a paused player
        // would otherwise report an unknown span length until resumed.
        self.refill();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ogg::{PacketWriteEndInfo, PacketWriter};
    use rodio::Source;
    use std::io::Cursor;

    const PRE_SKIP: u16 = 312;
    const FRAME: usize = 960; // 20 ms at 48 kHz
    const SECONDS: usize = 2;

    fn opus_head(channels: u8, pre_skip: u16, gain_q78: i16) -> Vec<u8> {
        let mut h = Vec::with_capacity(19);
        h.extend_from_slice(b"OpusHead");
        h.push(1); // version
        h.push(channels);
        h.extend_from_slice(&pre_skip.to_le_bytes());
        h.extend_from_slice(&48_000u32.to_le_bytes()); // input sample rate
        h.extend_from_slice(&gain_q78.to_le_bytes());
        h.push(0); // mapping family
        h
    }

    fn opus_tags() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"OpusTags");
        t.extend_from_slice(&4u32.to_le_bytes());
        t.extend_from_slice(b"test");
        t.extend_from_slice(&0u32.to_le_bytes()); // comment count
        t
    }

    /// OpusTags with one comment of `len` payload bytes, the shape of an
    /// embedded cover (base64 METADATA_BLOCK_PICTURE).
    fn opus_tags_with_picture(len: usize) -> Vec<u8> {
        let key = b"METADATA_BLOCK_PICTURE=";
        let mut t = Vec::with_capacity(len + 64);
        t.extend_from_slice(b"OpusTags");
        t.extend_from_slice(&4u32.to_le_bytes());
        t.extend_from_slice(b"test");
        t.extend_from_slice(&1u32.to_le_bytes()); // comment count
        t.extend_from_slice(&((key.len() + len) as u32).to_le_bytes());
        t.extend_from_slice(key);
        t.resize(t.len() + len, b'A');
        t
    }

    /// 2 s stereo 440 Hz sine, encoded with libopus and muxed in-memory.
    fn make_ogg_opus() -> Vec<u8> {
        make_ogg_opus_with_tags(opus_tags())
    }

    fn make_ogg_opus_with_tags(tags: Vec<u8>) -> Vec<u8> {
        let frames = SECONDS * 48_000;
        let mut pcm = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = 0.8 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin();
            pcm.push(s);
            pcm.push(s);
        }

        let mut enc = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)
            .expect("encoder");
        let serial = 0x4a53_4f50;
        let mut writer = PacketWriter::new(Cursor::new(Vec::new()));
        writer
            .write_packet(
                opus_head(2, PRE_SKIP, 0),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .unwrap();
        writer
            .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();

        let packets = frames / FRAME;
        for i in 0..packets {
            let chunk = &pcm[i * FRAME * 2..(i + 1) * FRAME * 2];
            let data = enc.encode_vec_float(chunk, 4000).expect("encode");
            let end = if i + 1 == packets {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            writer
                .write_packet(data, serial, end, ((i + 1) * FRAME) as u64)
                .unwrap();
        }
        writer.into_inner().into_inner()
    }

    #[test]
    fn roundtrip_decodes_sine() {
        let ogg = make_ogg_opus();
        let src = probe(Cursor::new(ogg), true).expect("probe should accept ogg-opus");
        assert_eq!(src.channels().get(), 2);
        assert_eq!(src.sample_rate().get(), 48_000);
        assert!(src.total_duration().is_none());

        let samples: Vec<f32> = src.collect();
        // Container declares pre-skip, so those frames must be discarded.
        let expected = (SECONDS * 48_000 - PRE_SKIP as usize) * 2;
        let tolerance = expected / 20;
        assert!(
            samples.len().abs_diff(expected) <= tolerance,
            "decoded {} samples, expected ~{expected}",
            samples.len()
        );

        let rms = (samples
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        // 0.8 amplitude sine has RMS ~0.57; allow for codec artifacts.
        assert!((0.35..0.75).contains(&rms), "implausible RMS {rms}");
    }

    #[test]
    fn span_len_is_known_until_the_end() {
        let mut src = probe(Cursor::new(make_ogg_opus()), true).expect("probe");
        // Known before the first pull (a paused player never pulls) and right
        // after a seek.
        assert!(matches!(src.current_span_len(), Some(n) if n > 0));
        src.try_seek(Duration::from_millis(500)).expect("seek");
        assert!(matches!(src.current_span_len(), Some(n) if n > 0));
        assert!(src.next().is_some());
        let mut samples = 1usize;
        loop {
            match src.current_span_len() {
                Some(0) => break,
                // Remaining samples may start mid-frame; the span must end
                // on a frame boundary.
                Some(n) => assert!(
                    (samples + n).is_multiple_of(2),
                    "span of {n} ends mid-frame"
                ),
                None => panic!("span length unknown after {samples} samples"),
            }
            assert!(src.next().is_some(), "span promised more samples");
            samples += 1;
        }
        assert!(
            src.next().is_none(),
            "Some(0) only at the end of the stream"
        );
        assert!(samples > 48_000, "decoded only {samples} samples");
    }

    #[test]
    fn seek_leaves_about_one_second() {
        let ogg = make_ogg_opus();
        let mut src = probe(Cursor::new(ogg), true).expect("probe");
        src.try_seek(Duration::from_secs(1)).expect("seek");

        let remaining: Vec<f32> = src.collect();
        let expected = 48_000 * 2; // 1 s stereo
        let tolerance = expected / 10;
        assert!(
            remaining.len().abs_diff(expected) <= tolerance,
            "got {} samples after seek, expected ~{expected}",
            remaining.len()
        );
    }

    #[test]
    fn probe_rejects_garbage_and_rewinds() {
        let junk: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let (mut reader, _err) =
            probe(Cursor::new(junk.clone()), true).expect_err("junk must fail");
        assert_eq!(reader.stream_position().unwrap(), 0);
        let mut back = Vec::new();
        reader.read_to_end(&mut back).unwrap();
        assert_eq!(back, junk);
    }

    #[test]
    fn probe_rejects_non_opus_ogg() {
        // Valid Ogg stream whose first packet is a Vorbis-style header.
        let mut writer = PacketWriter::new(Cursor::new(Vec::new()));
        let mut vorbis = b"\x01vorbis".to_vec();
        vorbis.extend_from_slice(&[0u8; 23]);
        writer
            .write_packet(vorbis, 99, PacketWriteEndInfo::EndStream, 0)
            .unwrap();
        let bytes = writer.into_inner().into_inner();

        let (mut reader, err) = probe(Cursor::new(bytes), true).expect_err("vorbis must fail");
        assert!(matches!(err, OpusProbeError::NotOpus));
        assert_eq!(reader.stream_position().unwrap(), 0);
    }

    #[test]
    fn a_large_embedded_cover_still_plays() {
        // Issue #10: a cover of a few MB in OpusTags exceeded the per-packet
        // budget, so the probe failed and the file did not play at all.
        let tags = opus_tags_with_picture(5 * 1024 * 1024);
        assert!(tags.len() as u64 > MAX_PACKET_BYTES);
        let mut src = probe(Cursor::new(make_ogg_opus_with_tags(tags)), true)
            .expect("probe should accept a large OpusTags packet");
        assert_eq!(
            src.budget.limit.load(Ordering::Relaxed),
            MAX_PACKET_BYTES,
            "audio packets keep the tight budget"
        );

        // Seeking to the start reads the header pages again.
        src.try_seek(Duration::ZERO).expect("seek to the start");
        assert_eq!(src.budget.limit.load(Ordering::Relaxed), MAX_PACKET_BYTES);
        let samples: Vec<f32> = src.collect();
        let expected = (SECONDS * 48_000 - PRE_SKIP as usize) * 2;
        assert!(
            samples.len().abs_diff(expected) <= expected / 20,
            "decoded {} samples after seeking to the start, expected ~{expected}",
            samples.len()
        );
    }
}
