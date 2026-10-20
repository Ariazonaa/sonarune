//! The bytes under a playing track: one HTTP download into a temp file
//! (stream-download), plus the failure handling that library leaves to its
//! stream type. Decoders read the download on the audio callback, where a
//! read of bytes that have not arrived waits for them, so everything here is
//! about never leaving such a read waiting for good:
//!
//! - a body that fails ends right there. stream-download only logs a failed
//!   chunk and polls again, and neither an HTTP/2 stream reset nor reqwest's
//!   expired read timeout ever stop failing, so the download would spin
//!   forever and the waiting read with it. One that sends nothing for
//!   [`Patience::silence`] is let go of too;
//! - the rest of its range is then asked for again — from where it broke,
//!   with backoff, for as long as there is buffered audio to play. Left to
//!   stream-download, the download would go on with the first gap in the file
//!   instead, which can lie far below the decoder: one waiting for the broken
//!   range would wait until everything up to it is in. The download gives up
//!   (and the read fails) only once a decoder has waited [`Patience::stall`]
//!   for it, or the outage outlasts [`Patience::outage`] while nobody waits;
//! - a live transcode is never asked for again: the server would start it
//!   over, and stream-download would append that at the current offset. A
//!   silent one is held on to until a decoder has waited too long;
//! - the player can stop a download at any time, which also wakes a decoder
//!   waiting on it;
//! - the decoder reads only what is on disk ([`StreamReader`]).

use super::{DownloadState, FileSlot, Storage};
use crate::error::{AppError, AppResult};
use bytes::Bytes;
use futures_core::Stream;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use stream_download::source::{SourceStream, StreamOutcome};
use stream_download::{Settings, StreamDownload};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// What a track starts with before its decoder reads anything: about 13 s of
/// a 320 kbit/s MP3, 4 s of CD-quality FLAC.
const HEAD_START: u64 = 512 * 1024;
/// How long one attempt at a missing range may wait for the server's answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// A read waiting for bytes looks again this often even without news: for a
/// download that ended without a word (a write error in stream-download).
const READ_POLL: Duration = Duration::from_millis(100);
/// A download writing this close below a read's position is taken to be on
/// its way there; one further off is pointed at it with a seek.
const STEER_WINDOW: u64 = 256 * 1024;
/// stream-download's own silence timeout, as good as off. The stream watches
/// for silence itself ([`Patience::silence`]) and keeps at its range through
/// an outage, which the library would break into every few seconds — with a
/// warning in the log each time — to call `reconnect`.
const LIBRARY_SILENCE: Duration = Duration::from_secs(24 * 60 * 60);

/// How long the stream holds out, at each point where it could give up.
#[derive(Debug, Clone, Copy)]
pub(super) struct Patience {
    /// A decoder waiting this long for bytes that do not come makes the
    /// download give up: its read then fails rather than holding the audio
    /// callback any longer.
    pub stall: Duration,
    /// With nobody waiting (paused, or still playing what is buffered), an
    /// outage this long makes it give up all the same.
    pub outage: Duration,
    /// How long the head start waits for the next chunk before it lets the
    /// decoder start with what is there.
    pub head_start: Duration,
    /// Between attempts at a missing range: the first pause, doubled up to
    /// the second.
    pub retry: (Duration, Duration),
    /// A body that sends nothing for this long is taken for broken off: the
    /// rest of its range is asked for on a new connection. A live transcode
    /// keeps its own (see the module docs).
    pub silence: Duration,
}

impl Patience {
    pub const PLAYBACK: Self = Self {
        stall: Duration::from_secs(20),
        outage: Duration::from_secs(10 * 60),
        head_start: Duration::from_secs(3),
        retry: (Duration::from_millis(500), Duration::from_secs(5)),
        silence: Duration::from_secs(5),
    };
}

/// A download, under way.
pub(super) struct Opened {
    pub reader: StreamReader,
    /// The server sent MPEG audio (an MP3), see `open_track_source`.
    pub mpeg: bool,
}

/// Request `url` and start downloading it into `storage` (whose slot gets
/// the reader's own handle on the file); `stop` aborts the open, and later
/// the download, at any point. Usually followed by [`head_start`].
pub(super) async fn open(
    client: reqwest::Client,
    url: reqwest::Url,
    (storage, reading): (Storage, FileSlot),
    settings: Settings<ServerStream>,
    stop: CancellationToken,
    state: Arc<DownloadState>,
    patience: Patience,
) -> AppResult<Opened> {
    let watch = Arc::new(ReadWatch::default());
    let origin = Arc::new(Origin {
        client,
        url,
        stop: stop.clone(),
        state: state.clone(),
        watch: watch.clone(),
        patience,
        outage: Mutex::new(None),
    });
    let stream = ServerStream::open(origin).await?;
    let mpeg = stream.mpeg;
    let len = stream.content_length;
    let settings = settings.retry_timeout(LIBRARY_SILENCE);
    let download = StreamDownload::from_stream(stream, storage, settings)
        .await
        .map_err(|e| AppError::Audio(format!("stream buffer failed: {e}")))?;
    let file = reading.lock().unwrap().take();
    if len.is_some() && file.is_none() {
        return Err(AppError::Audio("stream buffer failed: no temp file".into()));
    }
    Ok(Opened {
        reader: StreamReader {
            task_done: download.cancellation_token(),
            inner: download,
            // A live stream is read through stream-download (see the type).
            file: file.filter(|_| len.is_some()),
            file_pos: 0,
            len,
            pos: 0,
            state,
            stop,
            steered: None,
            watch,
            calls: 0,
        },
        mpeg,
    })
}

/// stream-download's own prefetch is off: it would run again after every
/// seek, and a reader waiting for the seek is woken only once it completes —
/// for a target near the end of the file, after everything up to it has been
/// downloaded (`ServerStream::poll_next` explains the rest). So the head
/// start is waited for here: until [`HEAD_START`] bytes from the start are in
/// (or all of a shorter stream), the download ended or broke off, or nothing
/// arrived for `patience`. From then on the decoder waits for what it reads.
pub(super) async fn head_start(
    state: &DownloadState,
    stop: &CancellationToken,
    patience: Duration,
) -> AppResult<()> {
    loop {
        if state.has_head_start(HEAD_START) {
            return Ok(());
        }
        tokio::select! {
            () = state.changed.notified() => {}
            () = stop.cancelled() => return Err(stopped()),
            () = tokio::time::sleep(patience) => return Ok(()),
        }
    }
}

fn stopped() -> AppError {
    AppError::Audio("stream open cancelled".into())
}

/// Which read or seek of the download the decoder is in, if any. That is how
/// the download tells a decoder waiting for missing bytes from one that is
/// busy playing what is already there.
#[derive(Debug, Default)]
pub(super) struct ReadWatch {
    /// The call number shifted left by one, the low bit set while inside it.
    /// Written by the reader — the audio callback, so one relaxed store per
    /// call boundary and nothing else.
    state: AtomicU64,
    /// The call the download saw the decoder in, and since when. Only ever
    /// touched by the download.
    seen: Mutex<Option<(u64, Instant)>>,
}

impl ReadWatch {
    fn enter(&self, call: u64) {
        self.state.store(call << 1 | 1, Ordering::Relaxed);
    }

    fn leave(&self, call: u64) {
        self.state.store(call << 1, Ordering::Relaxed);
    }

    /// How long the decoder has been inside one call, as far as observed.
    /// Coarse — it is only sampled when the stream decides whether to hold
    /// out.
    fn waited(&self, now: Instant) -> Duration {
        let state = self.state.load(Ordering::Relaxed);
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if state & 1 == 0 {
            *seen = None;
            return Duration::ZERO;
        }
        match *seen {
            Some((call, since)) if call == state => now.saturating_duration_since(since),
            _ => {
                *seen = Some((state, now));
                Duration::ZERO
            }
        }
    }
}

/// The decoder's side of a download. A stream with a length — direct play,
/// which seeks — is read from the temp file through a handle of its own, and
/// only where it is on disk. stream-download's own reader is wrong there in
/// three ways:
///
/// - it lets a waiting read go once the download's write position passes the
///   one the read waits for, wherever the download writes. After a seek that
///   can be another part of the file, and the read then gets the hole in
///   between: zeros;
/// - its downloaded ranges are half-open, so a read or seek at the end of the
///   file waits for the whole file, however little of it is missing;
/// - once the download fails, every read fails, also of what is on disk.
///
/// A live stream (no length, never seeked) goes through stream-download's
/// reader, which reads it in order and so gets none of that wrong.
pub struct StreamReader {
    inner: StreamDownload<Storage>,
    /// The reader's handle on the temp file (a stream with a length), and
    /// where it is.
    file: Option<std::fs::File>,
    file_pos: u64,
    len: Option<u64>,
    pos: u64,
    state: Arc<DownloadState>,
    stop: CancellationToken,
    /// Cancelled once the download task has ended, whichever way.
    task_done: CancellationToken,
    /// The position the download was last pointed at (see `steer`).
    steered: Option<u64>,
    watch: Arc<ReadWatch>,
    calls: u64,
}

impl StreamReader {
    /// The stream's length, when the server announced one (direct play).
    pub fn content_length(&self) -> Option<u64> {
        self.len
    }

    fn enter(&mut self) {
        self.calls += 1;
        self.watch.enter(self.calls);
    }

    fn leave(&self) {
        self.watch.leave(self.calls);
    }

    /// Read into `buf` what is on disk at `pos` — once something is: the
    /// download is pointed at it if it is busy elsewhere, and waited for.
    fn read_on_disk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = buf.len() as u64;
        loop {
            let on_disk = self.state.on_disk_from(self.pos, want, None);
            if on_disk > 0 {
                let Some(file) = self.file.as_mut() else {
                    return Err(io::Error::other("no temp file to read"));
                };
                if self.file_pos != self.pos {
                    self.file_pos = file.seek(SeekFrom::Start(self.pos))?;
                }
                let n = file.read(&mut buf[..on_disk as usize])?;
                self.file_pos += n as u64;
                return Ok(n);
            }
            if self.stop.is_cancelled() || self.task_done.is_cancelled() || self.state.is_broken() {
                return Err(io::Error::other("stream failed to download"));
            }
            self.steer()?;
            self.state.on_disk_from(self.pos, want, Some(READ_POLL));
        }
    }

    /// stream-download fetches what is missing in order from its write
    /// position, or from a seek of its own. Point it at a read it is not on
    /// its way to (once per position).
    fn steer(&mut self) -> io::Result<()> {
        let head = self.state.head.load(Ordering::Relaxed);
        let on_its_way = head <= self.pos && self.pos - head < STEER_WINDOW;
        if on_its_way || self.steered == Some(self.pos) {
            return Ok(());
        }
        self.steered = Some(self.pos);
        // Its own position is not used for reading here; the seek is only
        // what starts the range request.
        self.inner.seek(SeekFrom::Start(self.pos)).map(drop)
    }
}

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.enter();
        let result = match self.len {
            // At or past the end: nothing to read, nothing to wait for.
            Some(len) if self.pos >= len => Ok(0),
            Some(len) => {
                let want = (buf.len() as u64).min(len - self.pos) as usize;
                self.read_on_disk(&mut buf[..want])
            }
            None => self.inner.read(buf),
        };
        self.leave();
        let n = result?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for StreamReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(target) => Some(target),
            SeekFrom::Current(offset) => self.pos.checked_add_signed(offset),
            SeekFrom::End(offset) => {
                let Some(len) = self.len else {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "a live stream has no end to seek from",
                    ));
                };
                len.checked_add_signed(offset)
            }
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        // A stream with a length: only the position moves; reading there
        // fetches what is missing. A live stream is stream-download's — and
        // `stream_position()` must not make it seek.
        if self.len.is_none() && target != self.pos {
            self.enter();
            let result = self.inner.seek(SeekFrom::Start(target));
            self.leave();
            result?;
        }
        self.pos = target;
        Ok(target)
    }
}

type Body = Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send + Sync + Unpin>;

/// A request for a range in flight, see [`ServerStream::ask`]. The mutex
/// only makes it `Sync`, as stream-download's stream type has to be; it is
/// never locked.
type Request = Mutex<Pin<Box<dyn Future<Output = io::Result<Body>> + Send>>>;

/// What asking the server for the stream takes. Shared by the stream and its
/// request in flight, which runs on between the stream's polls.
struct Origin {
    client: reqwest::Client,
    url: reqwest::Url,
    stop: CancellationToken,
    state: Arc<DownloadState>,
    watch: Arc<ReadWatch>,
    patience: Patience,
    /// Since when the server has not delivered — it failed, fell silent or
    /// turned an attempt down — until data flows again.
    outage: Mutex<Option<Instant>>,
}

/// One attempt at a range that did not work out.
enum Failure {
    Stopped,
    /// The server said no (a 4xx): asking again would not change its mind.
    Refused(io::Error),
    /// Worth another try: no answer, a dropped connection, a server error.
    Transient(io::Error),
}

impl Origin {
    fn outage(&self) -> MutexGuard<'_, Option<Instant>> {
        self.outage.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn in_outage(&self) -> bool {
        self.outage().is_some()
    }

    /// The server stopped delivering, for `why`: an outage begins, unless
    /// one is under way.
    fn interrupted(&self, why: &dyn fmt::Display) {
        let mut outage = self.outage();
        if outage.is_none() {
            tracing::warn!("stream interrupted ({why})");
            *outage = Some(Instant::now());
        }
    }

    /// Data flows again.
    fn recovered(&self) {
        if let Some(since) = self.outage().take() {
            tracing::info!(
                "stream recovered after {:.1} s",
                since.elapsed().as_secs_f32()
            );
        }
    }

    /// Whether to give up: a decoder has waited too long for the missing
    /// bytes, or the outage has lasted too long even for nobody waiting.
    fn out_of_patience(&self) -> bool {
        let now = Instant::now();
        self.watch.waited(now) >= self.patience.stall
            || self
                .outage()
                .is_some_and(|since| now.saturating_duration_since(since) >= self.patience.outage)
    }

    /// Ask for `start..end` (or `start..` to the end), and keep at it.
    async fn fetch(&self, start: u64, end: Option<u64>) -> io::Result<Body> {
        let (first, most) = self.patience.retry;
        let mut pause = first;
        loop {
            // Also before the first attempt of an outage: a server that keeps
            // answering and then sending nothing comes back here every time.
            if self.in_outage() && self.out_of_patience() {
                tracing::warn!("stream given up: no data");
                self.state.broke_off();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
            }
            let error = match self.request(start, end).await {
                Ok(body) => return Ok(body),
                Err(Failure::Stopped) => return Err(interrupted()),
                Err(Failure::Refused(error)) => {
                    tracing::warn!("stream refused by the server: {error}");
                    self.state.broke_off();
                    return Err(error);
                }
                Err(Failure::Transient(error)) => error,
            };
            if !self.in_outage() {
                self.interrupted(&format_args!("{error}; asking for the rest again"));
            } else if self.out_of_patience() {
                tracing::warn!("stream given up: {error}");
                self.state.broke_off();
                return Err(error);
            }
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = self.stop.cancelled() => return Err(interrupted()),
            }
            pause = (pause * 2).min(most);
        }
    }

    async fn request(&self, start: u64, end: Option<u64>) -> Result<Body, Failure> {
        let range = match end {
            // HTTP ranges are inclusive.
            Some(end) => format!("bytes={start}-{}", end.saturating_sub(1)),
            None => format!("bytes={start}-"),
        };
        let send = self
            .client
            .get(self.url.clone())
            .header(reqwest::header::RANGE, range)
            .send();
        let response = tokio::select! {
            response = tokio::time::timeout(REQUEST_TIMEOUT, send) => response,
            () = self.stop.cancelled() => return Err(Failure::Stopped),
        };
        let response = match response {
            Err(_) => {
                return Err(Failure::Transient(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no answer from the server",
                )))
            }
            Ok(Err(e)) => return Err(Failure::Transient(io::Error::other(e.without_url()))),
            Ok(Ok(response)) => response,
        };
        let status = response.status();
        // A server that ignores the range sends the file from its start,
        // which is what was asked for only when the range starts there.
        if status == reqwest::StatusCode::PARTIAL_CONTENT
            || (status == reqwest::StatusCode::OK && start == 0)
        {
            return Ok(Box::new(response.bytes_stream()));
        }
        let error = io::Error::other(format!("server returned {status}"));
        if status.is_server_error()
            || status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            Err(Failure::Transient(error))
        } else {
            Err(Failure::Refused(error))
        }
    }
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "download stopped")
}

/// The HTTP side of a download, as stream-download sees it (see the module
/// docs for what it does differently from the library's own `HttpStream`).
pub(super) struct ServerStream {
    origin: Arc<Origin>,
    /// The response body being read; `None` while it is asked for, and once
    /// the range or the stream ended.
    body: Option<Body>,
    /// Asking for a range: after a seek, or for the rest of one whose body
    /// broke off. Polled with the stream.
    request: Option<Request>,
    /// The rest of a chunk handed over split, see `poll_next`.
    rest: Option<Bytes>,
    /// Split the next chunk: a new range starts with it.
    split_next: bool,
    /// Where in the stream the next byte from the server goes, and where the
    /// range being fetched ends (`None`: with the stream).
    at: u64,
    end: Option<u64>,
    content_length: Option<u64>,
    /// Missing ranges can be asked for again: the length is known and the
    /// server takes range requests (a live transcode does neither).
    ranges: bool,
    mpeg: bool,
    /// `origin.stop`, as a future polled with the body, so a stop wakes the
    /// task.
    stopped: Pin<Box<WaitForCancellationFutureOwned>>,
    /// Runs out once the body has sent nothing for [`Patience::silence`].
    silence: Pin<Box<tokio::time::Sleep>>,
}

impl ServerStream {
    async fn open(origin: Arc<Origin>) -> AppResult<Self> {
        let response = tokio::select! {
            response = origin.client.get(origin.url.clone()).send() => response.map_err(|e| {
                AppError::Audio(format!("stream open failed: {}", e.without_url()))
            })?,
            () = origin.stop.cancelled() => return Err(stopped()),
        };
        let status = response.status();
        if !status.is_success() {
            return Err(AppError::Audio(format!(
                "stream open failed: server returned {status}"
            )));
        }
        let content_length = response.content_length();
        let header = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_ascii_lowercase)
        };
        let ranges = content_length.is_some()
            && header(reqwest::header::ACCEPT_RANGES).as_deref() != Some("none");
        let mpeg = header(reqwest::header::CONTENT_TYPE).is_some_and(|kind| {
            ["audio/mpeg", "audio/mp3", "audio/mpeg3", "audio/x-mpeg"]
                .iter()
                .any(|mpeg| kind.starts_with(mpeg))
        });
        let stopped = Box::pin(origin.stop.clone().cancelled_owned());
        let silence = Box::pin(tokio::time::sleep(origin.patience.silence));
        Ok(Self {
            body: Some(Box::new(response.bytes_stream())),
            request: None,
            rest: None,
            split_next: false,
            at: 0,
            end: content_length,
            content_length,
            ranges,
            mpeg,
            stopped,
            silence,
            origin,
        })
    }

    /// Ask for `start..end` (or `start..` to the end). The request runs, and
    /// retries, as the stream is polled: stream-download meanwhile still
    /// takes seeks — and its own cancellation.
    fn ask(&mut self, start: u64, end: Option<u64>) {
        self.body = None;
        let origin = self.origin.clone();
        self.request = Some(Mutex::new(Box::pin(async move {
            origin.fetch(start, end).await
        })));
    }

    /// The body just delivered, or just began: the silence starts over.
    fn rearm(&mut self) {
        let deadline = tokio::time::Instant::now() + self.origin.patience.silence;
        self.silence.as_mut().reset(deadline);
    }

    /// The body failed or fell silent: ask for the rest of its range again,
    /// where it broke. False when there is nothing to ask for — the range is
    /// all there (stream-download goes on with the next gap), or nothing can
    /// be asked for again (a live transcode, which ends here).
    fn broke(&mut self, why: &dyn fmt::Display) -> bool {
        self.body = None;
        if self.ranges && self.end.is_some_and(|end| self.at >= end) {
            return false;
        }
        self.origin.interrupted(why);
        if !self.ranges {
            self.origin.state.broke_off();
            return false;
        }
        self.ask(self.at, self.end);
        true
    }

    /// The body has sent nothing for a while. False: the stream ends here.
    fn went_silent(&mut self) -> bool {
        if self.ranges {
            // Drop the silent connection; the rest comes on a new one.
            return self.broke(&"no data");
        }
        // A live transcode: stay on this connection, which may still recover
        // (reqwest's read timeout ends one that does not), but give up once a
        // decoder has waited too long.
        self.origin.interrupted(&"live stream stalled");
        if self.origin.out_of_patience() {
            tracing::warn!("live stream given up");
            self.body = None;
            self.origin.state.broke_off();
            return false;
        }
        self.rearm();
        true
    }
}

impl Stream for ServerStream {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // Stopped: end the body. stream-download then winds the download up
        // — it asks for a missing range, which `seek_range` refuses — and
        // wakes a reader waiting on it.
        if this.origin.stop.is_cancelled() || this.stopped.as_mut().poll(cx).is_ready() {
            this.body = None;
            this.request = None;
            this.rest = None;
            this.origin.state.wake();
            return Poll::Ready(None);
        }
        if let Some(rest) = this.rest.take() {
            this.at += rest.len() as u64;
            return Poll::Ready(Some(Ok(rest)));
        }
        loop {
            if let Some(request) = this.request.as_mut() {
                let request = request.get_mut().unwrap_or_else(PoisonError::into_inner);
                let Poll::Ready(answer) = request.as_mut().poll(cx) else {
                    return Poll::Pending;
                };
                this.request = None;
                // No body: given up (the download state says so) or stopped.
                let Ok(body) = answer else {
                    return Poll::Ready(None);
                };
                this.body = Some(body);
                this.rearm();
            }
            let Some(body) = this.body.as_mut() else {
                return Poll::Ready(None);
            };
            let empty = match Pin::new(body).poll_next(cx) {
                Poll::Ready(Some(Ok(mut chunk))) if !chunk.is_empty() => {
                    this.origin.recovered();
                    this.rearm();
                    // After a seek stream-download treats the first chunk as
                    // a prefetch (of zero bytes here, see `head_start`), and a
                    // prefetch never wakes a waiting reader: only the chunks
                    // after it do. A range that arrives in one piece — the end
                    // of a file, typically — would leave the reader waiting
                    // until something else fills the file up to its target.
                    // Its first byte goes on its own, so the rest wakes the
                    // reader.
                    if std::mem::take(&mut this.split_next) && chunk.len() > 1 {
                        this.rest = Some(chunk.split_off(1));
                    }
                    this.at += chunk.len() as u64;
                    return Poll::Ready(Some(Ok(chunk)));
                }
                Poll::Ready(Some(Err(e))) => {
                    if !this.broke(&e.without_url()) {
                        return Poll::Ready(None);
                    }
                    continue;
                }
                // The range is complete.
                Poll::Ready(None) => {
                    this.body = None;
                    return Poll::Ready(None);
                }
                // stream-download cannot take an empty chunk at every
                // position (it looks up the range ending at the write
                // position): come back for the next one.
                Poll::Ready(Some(Ok(_))) => true,
                Poll::Pending => false,
            };
            if this.silence.as_mut().poll(cx).is_pending() {
                if empty {
                    cx.waker().wake_by_ref();
                }
                return Poll::Pending;
            }
            if !this.went_silent() {
                return Poll::Ready(None);
            }
        }
    }
}

impl SourceStream for ServerStream {
    /// Never created by stream-download: `open` builds it.
    type Params = Infallible;
    type StreamCreationError = Infallible;

    async fn create(params: Self::Params) -> Result<Self, Self::StreamCreationError> {
        match params {}
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    async fn seek_range(&mut self, start: u64, end: Option<u64>) -> io::Result<()> {
        self.body = None;
        self.request = None;
        self.rest = None;
        if self.origin.stop.is_cancelled() {
            return Err(interrupted());
        }
        if self.origin.state.is_broken() {
            return Err(io::Error::other("stream given up"));
        }
        self.at = start;
        // stream-download ends the range where the first gap after its write
        // position ends. After a seek above a hole it is filling, past a range
        // on disk, that gap lies wholly below `start`: an end in front of the
        // start, which a server answers with the whole file. The range then
        // runs up to the next one on disk.
        self.end = end
            .filter(|&end| end > start)
            .or_else(|| self.origin.state.on_disk_after(start))
            .or(self.content_length);
        self.split_next = true;
        // Nothing left: the absent body ends at once.
        if Some(start) != self.content_length {
            self.ask(start, self.end);
        }
        Ok(())
    }

    /// Not called: `open` turns stream-download's silence timeout off. Taken
    /// for silence, should it come.
    async fn reconnect(&mut self, _position: u64) -> io::Result<()> {
        if self.body.is_some() {
            self.went_silent();
        }
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        self.ranges
    }

    /// A stop is the player letting go, not a failure: stream-download would
    /// log the rest it then cannot fetch as one.
    async fn on_finish(
        &mut self,
        result: io::Result<()>,
        _outcome: StreamOutcome,
    ) -> io::Result<()> {
        if self.origin.stop.is_cancelled() {
            Ok(())
        } else {
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use stream_download::storage::adaptive::AdaptiveStorageProvider;

    /// What the test server does with one request. The script is played in
    /// order; its last entry repeats.
    #[derive(Debug, Clone, Copy)]
    enum Answer {
        /// The requested range (or the whole file), with its length.
        Serve,
        /// That, but cut off after this many bytes.
        Cut(usize),
        /// That, but silent after this many bytes, connection kept open.
        Stall(usize),
        /// An error status.
        Status(u16),
        /// A live stream: no length, no ranges, silent after this many bytes.
        Live(usize),
        /// A range that starts at or after this offset; anything else gets
        /// its headers and then silence.
        TailFrom(usize),
        /// The requested range, announced this much longer than it is: the
        /// connection then closes as if broken off.
        ClaimsMore(usize),
    }

    struct Server {
        url: reqwest::Url,
        /// The `Range` header of every request, in order (`None`: none).
        requests: Arc<Mutex<Vec<Option<String>>>>,
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    /// A local HTTP/1.1 server for `body`, one request per connection.
    fn serve(body: Vec<u8>, script: Vec<Answer>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let url = format!("http://{}/stream", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        std::thread::spawn(move || {
            let mut silent: Vec<TcpStream> = Vec::new();
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => head.extend_from_slice(&buf[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let range = head
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("range: ")
                            .or(line.strip_prefix("Range: "))
                    })
                    .map(str::to_string);
                log.lock().unwrap().push(range.clone());
                let answer = script[n.min(script.len() - 1)];
                // Like Kestrel, a range that makes no sense (it ends before
                // it starts) is ignored: the whole file, 200.
                let wanted = range
                    .as_deref()
                    .and_then(|r| r.strip_prefix("bytes="))
                    .and_then(|r| r.split_once('-'))
                    .map(|(s, e)| {
                        let s: usize = s.parse().unwrap();
                        let e = if e.is_empty() {
                            body.len()
                        } else {
                            e.parse::<usize>().unwrap() + 1
                        };
                        (s, e.min(body.len()))
                    })
                    .filter(|(s, e)| s < e);
                let (start, end) = wanted.unwrap_or((0, body.len()));
                let part = &body[start..end];
                let status = if wanted.is_some() {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let send_claiming = |stream: &mut TcpStream, bytes: &[u8], len: usize| {
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nContent-Type: audio/flac\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(bytes);
                    let _ = stream.flush();
                };
                let send =
                    |stream: &mut TcpStream, bytes: &[u8]| send_claiming(stream, bytes, part.len());
                match answer {
                    Answer::Serve => send(&mut stream, part),
                    Answer::ClaimsMore(more) => {
                        send_claiming(&mut stream, part, part.len() + more);
                    }
                    Answer::Cut(at) => send(&mut stream, &part[..at.min(part.len())]),
                    Answer::Stall(at) => {
                        send(&mut stream, &part[..at.min(part.len())]);
                        silent.push(stream);
                    }
                    Answer::Status(code) => {
                        let _ = stream.write_all(
                            format!("HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .as_bytes(),
                        );
                    }
                    Answer::TailFrom(from) if start >= from => send(&mut stream, part),
                    Answer::TailFrom(_) => {
                        send(&mut stream, &[]);
                        silent.push(stream);
                    }
                    Answer::Live(at) => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nAccept-Ranges: none\r\nContent-Type: audio/ogg\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.write_all(&body[..at.min(body.len())]);
                        let _ = stream.flush();
                        silent.push(stream);
                    }
                }
            }
        });
        Server { url, requests }
    }

    impl Server {
        fn ranges(&self) -> Vec<Option<String>> {
            self.requests.lock().unwrap().clone()
        }
    }

    struct Download {
        reader: StreamReader,
        state: Arc<DownloadState>,
        stop: CancellationToken,
        runtime: tokio::runtime::Runtime,
    }

    const SHORT_SILENCE: Duration = Duration::from_millis(300);
    /// Longer than any test: a silent body stays silent.
    const ENDLESS_SILENCE: Duration = Duration::from_secs(600);

    const TEST_PATIENCE: Patience = Patience {
        stall: Duration::from_secs(3),
        outage: Duration::from_secs(60),
        head_start: Duration::from_millis(300),
        retry: (Duration::from_millis(50), Duration::from_millis(200)),
        silence: SHORT_SILENCE,
    };

    /// `TEST_PATIENCE`, but a decoder may wait as long as any test runs and
    /// a silent body stays silent: only what a test sets up makes things move.
    const ENDLESS_PATIENCE: Patience = Patience {
        stall: Duration::from_secs(600),
        silence: ENDLESS_SILENCE,
        ..TEST_PATIENCE
    };

    /// Open `server`'s stream as the player does.
    fn open_test(server: &Server, patience: Patience) -> Download {
        open_with(server, patience, Duration::from_secs(30), None)
    }

    /// That, with reqwest's read timeout, and `log` as the tracing
    /// subscriber of the threads the download runs on.
    fn open_with(
        server: &Server,
        patience: Patience,
        read_timeout: Duration,
        log: Option<tracing::Dispatch>,
    ) -> Download {
        let mut runtime = tokio::runtime::Builder::new_multi_thread();
        runtime.worker_threads(2).enable_all();
        if let Some(log) = log {
            runtime.on_thread_start(move || {
                // For the thread's whole life.
                std::mem::forget(tracing::dispatcher::set_default(&log));
            });
        }
        let runtime = runtime.build().unwrap();
        let state = Arc::new(DownloadState::default());
        let stop = CancellationToken::new();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(read_timeout)
            .build()
            .unwrap();
        // As the player stores streams, in the temp directory itself.
        let reading = FileSlot::default();
        let storage = AdaptiveStorageProvider::new(
            super::super::StreamFiles {
                dir: None,
                wave_file: None,
                reading: reading.clone(),
            },
            std::num::NonZeroUsize::new(64 * 1024 * 1024).unwrap(),
        );
        let progress = state.clone();
        let settings = Settings::default().prefetch_bytes(0).on_progress(
            move |stream: &ServerStream, progress_state, _| {
                progress.report(progress_state, stream.content_length());
            },
        );
        let opened = runtime
            .block_on(async {
                let opened = open(
                    client,
                    server.url.clone(),
                    (storage, reading),
                    settings,
                    stop.clone(),
                    state.clone(),
                    patience,
                )
                .await?;
                head_start(&state, &stop, patience.head_start).await?;
                Ok::<_, AppError>(opened)
            })
            .expect("open");
        Download {
            reader: opened.reader,
            state,
            stop,
            runtime,
        }
    }

    /// Run `op` on a thread of its own and fail the test if it takes longer
    /// than `limit` — a read that waits forever must not hang the suite.
    fn within<T: Send + 'static>(limit: Duration, op: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(op());
        });
        rx.recv_timeout(limit)
            .unwrap_or_else(|_| panic!("still waiting after {limit:?}"))
    }

    fn read_all(reader: &mut StreamReader) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        reader.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn a_dropped_connection_continues_where_it_broke() {
        let body = data(900_000);
        let server = serve(body.clone(), vec![Answer::Cut(300_000), Answer::Serve]);
        let Download {
            mut reader,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        let got = within(Duration::from_secs(20), move || {
            let got = read_all(&mut reader);
            drop(runtime);
            got
        })
        .expect("the whole file");
        assert!(got == body, "the file came back different");
        let ranges = server.ranges();
        assert_eq!(ranges[0], None);
        assert_eq!(ranges[1].as_deref(), Some("bytes=300000-899999"));
    }

    #[test]
    fn a_range_that_breaks_off_is_continued_not_left_for_the_first_gap() {
        // The download from the start stops early. A decoder seeks ahead,
        // and the range it gets breaks off twice: before its first byte, and
        // again after some. The server only ever serves from that seek on —
        // a download that went back to the gap below it would never get
        // there, and the decoder would wait for good.
        let body = data(2_000_000);
        let server = serve(
            body.clone(),
            vec![
                Answer::Stall(100_000),
                Answer::Cut(0),
                Answer::Cut(10_000),
                Answer::TailFrom(1_500_000),
            ],
        );
        let Download {
            mut reader,
            runtime,
            ..
        } = open_test(&server, ENDLESS_PATIENCE);
        let got = within(Duration::from_secs(10), move || {
            reader.seek(SeekFrom::Start(1_500_000)).unwrap();
            let mut got = vec![0u8; 20_000];
            let got = reader.read_exact(&mut got).map(|()| got);
            drop(runtime);
            got
        })
        .expect("the range after the seek");
        assert!(got == body[1_500_000..1_520_000], "it came back different");
        let ranges = server.ranges();
        assert_eq!(
            ranges[1..4],
            [
                Some("bytes=1500000-1999999".to_string()),
                Some("bytes=1500000-1999999".to_string()),
                Some("bytes=1510000-1999999".to_string()),
            ]
        );
    }

    #[test]
    fn a_range_that_breaks_after_its_last_byte_is_complete() {
        // Every range comes whole and then breaks off, as a server announcing
        // more than it sends would: nothing is missing, nothing to ask for.
        let body = data(2_000_000);
        let server = serve(
            body.clone(),
            vec![Answer::Stall(100_000), Answer::ClaimsMore(10)],
        );
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, ENDLESS_PATIENCE);
        let downloaded = state.clone();
        let got = within(Duration::from_secs(10), move || {
            reader.seek(SeekFrom::Start(1_500_000)).unwrap();
            reader.read_exact(&mut [0u8; 1_000]).unwrap();
            // A read below would point the download there at once.
            while !downloaded.is_complete() && !downloaded.is_broken() {
                std::thread::sleep(Duration::from_millis(10));
            }
            reader.seek(SeekFrom::Start(0)).unwrap();
            let got = read_all(&mut reader);
            drop(runtime);
            got
        })
        .expect("the whole file");
        assert!(got == body, "the file came back different");
        assert!(!state.is_broken());
        // The range after the seek, then the gap below it.
        assert_eq!(
            server.ranges()[1..],
            [
                Some("bytes=1500000-1999999".to_string()),
                Some("bytes=100000-1499999".to_string()),
            ]
        );
    }

    #[test]
    fn a_seek_above_a_hole_being_filled_asks_for_a_range_that_makes_sense() {
        // Pieces of the file on disk, the download filling the hole above
        // the start one, and a seek past the next piece up: stream-download
        // ends its request where the hole it is filling ends — in front of
        // where it starts.
        let body = data(2_000_000);
        let server = serve(
            body.clone(),
            vec![
                Answer::Stall(100_000),
                Answer::Stall(50_000),
                Answer::Stall(50_000),
                Answer::Serve,
            ],
        );
        let Download {
            mut reader,
            runtime,
            ..
        } = open_test(&server, ENDLESS_PATIENCE);
        let got = within(Duration::from_secs(10), move || {
            let read_at = |at: u64| {
                reader.seek(SeekFrom::Start(at)).unwrap();
                let mut got = vec![0u8; 1_000];
                reader.read_exact(&mut got).map(|()| got)
            };
            let got = [1_500_000, 500_000, 1_800_000].map(read_at);
            drop(runtime);
            got
        });
        for (got, at) in got.into_iter().zip([1_500_000, 500_000, 1_800_000]) {
            let got = got.unwrap_or_else(|e| panic!("reading at {at}: {e}"));
            assert!(got == body[at..at + 1_000], "{at} came back different");
        }
        let ranges = server.ranges();
        assert_eq!(ranges[2].as_deref(), Some("bytes=500000-1499999"));
        assert_eq!(ranges[3].as_deref(), Some("bytes=1800000-1999999"));
    }

    #[test]
    fn a_body_that_keeps_failing_ends_and_the_rest_is_asked_for() {
        // reqwest's read timeout, once expired, fails every later poll of the
        // body (an HTTP/2 stream reset does the same). Passed on as errors,
        // stream-download would poll that body forever.
        let body = data(900_000);
        let server = serve(body.clone(), vec![Answer::Stall(600_000), Answer::Serve]);
        let Download {
            mut reader,
            runtime,
            ..
        } = open_with(
            &server,
            Patience {
                silence: ENDLESS_SILENCE,
                ..TEST_PATIENCE
            },
            Duration::from_millis(400),
            None,
        );
        let got = within(Duration::from_secs(20), move || {
            let got = read_all(&mut reader);
            drop(runtime);
            got
        })
        .expect("the whole file");
        assert!(got == body, "the file came back different");
        assert_eq!(server.ranges()[1].as_deref(), Some("bytes=600000-899999"));
    }

    #[test]
    fn an_outage_is_ridden_out_while_there_is_audio_to_play() {
        let body = data(900_000);
        let server = serve(
            body.clone(),
            vec![
                Answer::Cut(600_000),
                Answer::Status(503),
                Answer::Status(503),
                Answer::Status(503),
                Answer::Serve,
            ],
        );
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        let got = within(Duration::from_secs(20), move || {
            let got = read_all(&mut reader);
            drop(runtime);
            got
        })
        .expect("buffered audio and the rest, no error");
        assert!(got == body, "the file came back different");
        assert!(!state.is_broken());
        assert!(state.is_complete());
    }

    #[test]
    fn a_decoder_waiting_through_an_outage_gets_an_error_not_a_hang() {
        let body = data(900_000);
        let server = serve(body, vec![Answer::Cut(100_000), Answer::Status(503)]);
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        let result = within(Duration::from_secs(20), move || {
            let result = read_all(&mut reader);
            drop(runtime);
            result
        });
        assert!(
            result.is_err(),
            "the read should fail once patience runs out"
        );
        assert!(state.is_broken());
    }

    #[test]
    fn a_server_that_answers_and_then_sends_nothing_is_given_up_too() {
        let body = data(900_000);
        let server = serve(body, vec![Answer::Cut(100_000), Answer::Stall(0)]);
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        let result = within(Duration::from_secs(20), move || {
            let result = read_all(&mut reader);
            drop(runtime);
            result
        });
        assert!(
            result.is_err(),
            "the read should fail once patience runs out"
        );
        assert!(state.is_broken());
    }

    #[test]
    fn what_is_on_disk_stays_readable_after_the_download_gave_up() {
        let body = data(900_000);
        let server = serve(
            body.clone(),
            vec![Answer::Cut(300_000), Answer::Status(503)],
        );
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        let (first, gave_up, again) = within(Duration::from_secs(20), move || {
            let mut first = vec![0u8; 300_000];
            reader.read_exact(&mut first).unwrap();
            // Past it: the download gives up while the read waits.
            let gave_up = reader.read(&mut [0u8; 1024]).is_err();
            // Back to the part on disk, as a seek in the decoder would.
            reader.seek(SeekFrom::Start(1_000)).unwrap();
            let mut again = vec![0u8; 1_000];
            let again = reader.read_exact(&mut again).map(|()| again);
            drop(runtime);
            (first, gave_up, again)
        });
        assert!(first == body[..300_000]);
        assert!(gave_up, "the read past the download failed");
        assert!(state.is_broken());
        assert_eq!(
            again.expect("what is on disk can still be read"),
            body[1_000..2_000]
        );
    }

    #[test]
    fn a_refusing_server_is_not_asked_again_and_again() {
        let body = data(900_000);
        let server = serve(body, vec![Answer::Cut(100_000), Answer::Status(404)]);
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        let result = within(Duration::from_secs(10), move || {
            let result = read_all(&mut reader);
            drop(runtime);
            result
        });
        assert!(result.is_err());
        assert!(state.is_broken());
        assert_eq!(server.ranges().len(), 2, "one refusal is the answer");
    }

    #[test]
    fn stopping_wakes_a_decoder_waiting_for_a_silent_server() {
        let body = data(900_000);
        let server = serve(body, vec![Answer::Stall(100_000)]);
        let Download {
            mut reader,
            stop,
            runtime,
            ..
        } = open_test(&server, ENDLESS_PATIENCE);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            stop.cancel();
        });
        let result = within(Duration::from_secs(10), move || {
            let result = read_all(&mut reader);
            drop(runtime);
            result
        });
        assert!(result.is_err(), "a stopped download fails the waiting read");
    }

    /// The errors logged on the threads this is the subscriber of.
    #[derive(Clone, Default)]
    struct ErrorLog(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorLog {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() == tracing::Level::ERROR {
                let mut line = event.metadata().target().to_string();
                event.record(
                    &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                        line.push_str(&format!(" {}={value:?}", field.name()));
                    },
                );
                self.0.lock().unwrap().push(line);
            }
        }
    }

    #[test]
    fn a_stopped_download_ends_without_an_error_in_the_log() {
        // Every skip of a track still downloading stops its download: that
        // is no failure, and the diagnostics ring is not the place for it.
        use tracing_subscriber::layer::SubscriberExt as _;
        let errors = ErrorLog::default();
        let log = tracing::Dispatch::new(tracing_subscriber::registry().with(errors.clone()));
        let body = data(900_000);
        let server = serve(body, vec![Answer::Stall(100_000)]);
        let Download {
            reader,
            stop,
            runtime,
            ..
        } = open_with(
            &server,
            ENDLESS_PATIENCE,
            Duration::from_secs(30),
            Some(log),
        );
        // The log does reach `errors` from where the download runs.
        let probe = runtime.spawn(async {
            tracing::error!(target: "probe", "probe");
        });
        runtime.block_on(probe).unwrap();
        stop.cancel();
        let task_done = reader.task_done.clone();
        within(Duration::from_secs(10), move || {
            runtime.block_on(task_done.cancelled());
            drop(runtime);
        });
        drop(reader);
        assert_eq!(*errors.0.lock().unwrap(), ["probe message=probe"]);
    }

    /// `seconds` of a stereo tone as Ogg-Opus at 256 kbit/s, a page every
    /// half second.
    fn ogg_opus(seconds: usize) -> Vec<u8> {
        use ogg::{PacketWriteEndInfo, PacketWriter};
        const FRAME: usize = 960;
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio).unwrap();
        encoder.set_bitrate(opus::Bitrate::Bits(256_000)).unwrap();
        let serial = 7;
        let mut writer = PacketWriter::new(std::io::Cursor::new(Vec::new()));
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 2]); // version, channels
        head.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]); // output gain, mapping family
        writer
            .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&[0; 8]); // no vendor, no comments
        writer
            .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        let packets = seconds * 48_000 / FRAME;
        let mut pcm = vec![0f32; FRAME * 2];
        for i in 0..packets {
            for (n, frame) in pcm.chunks_mut(2).enumerate() {
                let t = (i * FRAME + n) as f32 / 48_000.0;
                frame.fill(0.5 * (std::f32::consts::TAU * 440.0 * t).sin());
            }
            let data = encoder.encode_vec_float(&pcm, 4000).unwrap();
            let end = if i + 1 == packets {
                PacketWriteEndInfo::EndStream
            } else if (i + 1) % 25 == 0 {
                PacketWriteEndInfo::EndPage
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
    fn an_ogg_opus_file_is_positioned_without_downloading_all_of_it() {
        // Ogg's bisection reads the end of the file first. On a slow link,
        // every request here brings 400 kB and then nothing: the file is
        // never complete, so anything waiting for all of it waits for good.
        let body = ogg_opus(30);
        assert!(body.len() > 800_000, "the test file is too small to tell");
        let server = serve(body, vec![Answer::Stall(400_000)]);
        let Download {
            reader, runtime, ..
        } = open_test(&server, ENDLESS_PATIENCE);
        let samples = within(Duration::from_secs(20), move || {
            use rodio::Source as _;
            let mut source = crate::player::opus::probe(reader, true)
                .map_err(|(_, e)| e)
                .expect("an Ogg-Opus stream");
            source
                .try_seek(Duration::from_secs(20))
                .expect("positioned");
            let samples: Vec<f32> = source.by_ref().take(48_000 * 2).collect();
            drop(runtime);
            samples
        });
        assert_eq!(
            samples.len(),
            48_000 * 2,
            "a second of audio after the target"
        );
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        assert!(rms > 0.2, "the audio after the seek is silent ({rms})");
    }

    #[test]
    fn a_seek_near_the_end_does_not_wait_for_the_whole_file() {
        let body = data(2_000_000);
        // The download from the start stops after the head start, and the
        // server only ever serves the last few kB: waiting for anything more
        // waits for good.
        let server = serve(
            body.clone(),
            vec![
                Answer::Stall(600_000),
                Answer::TailFrom(body.len() - 10_000),
            ],
        );
        let Download {
            mut reader,
            runtime,
            ..
        } = open_test(&server, ENDLESS_PATIENCE);
        let len = body.len() as u64;
        let tail = within(Duration::from_secs(10), move || {
            // Ogg's bisection asks for the end first: answered on the spot.
            assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), len);
            assert_eq!(reader.read(&mut [0u8; 16]).unwrap(), 0);
            // Then a page close to it — a range small enough to arrive in
            // one piece, which stream-download alone would never wake on.
            reader.seek(SeekFrom::End(-2_000)).unwrap();
            let mut tail = vec![0u8; 2_000];
            reader.read_exact(&mut tail).unwrap();
            // And back to the start, which is on disk.
            reader.seek(SeekFrom::Start(10)).unwrap();
            let mut start = [0u8; 4];
            reader.read_exact(&mut start).unwrap();
            drop(runtime);
            (tail, start)
        });
        assert!(
            tail.0 == body[body.len() - 2_000..],
            "the tail came back different"
        );
        assert_eq!(tail.1, body[10..14]);
    }

    #[test]
    fn a_live_stream_is_never_asked_for_again() {
        let body = data(300_000);
        let server = serve(body.clone(), vec![Answer::Live(200_000), Answer::Serve]);
        let Download {
            mut reader,
            state,
            runtime,
            ..
        } = open_test(&server, TEST_PATIENCE);
        assert_eq!(reader.content_length(), None);
        let got = within(Duration::from_secs(20), move || {
            let got = read_all(&mut reader);
            drop(runtime);
            got
        })
        .expect("what arrived, then the end");
        assert!(got == body[..200_000], "the stream came back different");
        assert!(
            state.is_broken(),
            "a cut-off live stream counts as broken off"
        );
        assert_eq!(server.ranges(), vec![None], "nothing was asked for again");
    }

    #[test]
    fn waiting_is_measured_per_call() {
        let watch = ReadWatch::default();
        let t0 = Instant::now();
        assert_eq!(watch.waited(t0), Duration::ZERO);
        watch.enter(1);
        assert_eq!(watch.waited(t0), Duration::ZERO);
        let later = t0 + Duration::from_secs(5);
        assert_eq!(watch.waited(later), Duration::from_secs(5));
        // Another call started in between: the clock starts over.
        watch.leave(1);
        watch.enter(2);
        assert_eq!(watch.waited(later), Duration::ZERO);
        watch.leave(2);
        assert_eq!(watch.waited(later), Duration::ZERO);
        assert_eq!(*watch.seen.lock().unwrap(), None);
    }
}
