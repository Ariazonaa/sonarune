//! The decoder thread of a source. A decoder reads the track's download, and
//! a read of bytes that have not arrived waits for them: for seconds on a slow
//! link, for as long as the stream's patience lasts on a broken one. So every
//! source decodes on a thread of its own into a lock-free ring a second and a
//! half ahead, and what the mixer pulls on the audio callback ([`RingSource`])
//! only ever reads that ring. When the decoder falls behind, that one source
//! plays silence for as long as it has to; everything else in the mix — the
//! other track of a crossfade — plays on.
//!
//! - A source ends once its decoder has reached the end and the ring is
//!   drained, never because the ring ran dry.
//! - Silence handed out while the ring is dry is no media time. rodio counts
//!   it all the same, so the player takes it off (see
//!   [`FeedHandle::silence`]).
//! - A seek is a request to the thread: the ring is emptied, and the source
//!   is silent until audio from the target arrives. A read the decoder is
//!   waiting in is cut short for it ([`Interrupt`], which the reader shares).
//! - A source can be given a ring of its own again, positioned anew, while the
//!   decoder and its download carry on ([`FeedHandle::reposition`]): moving to
//!   another output costs no new download.
//! - The thread ends once its source is dropped or its download let go.
//!
//! Between the decoder and the ring sits the conversion stage
//! (`resample.rs`): the ring holds the decoder's audio at the output's rate,
//! in the channels the decoder started with. A new ring for another output
//! may come at another rate; the stage is rebuilt for it.
//!
//! The ring counts samples on both sides: the decoder thread how many it put
//! in, the source how many it took out (played or thrown away). A seek is
//! numbered, and the thread answers it with the count it had reached when it
//! carried it out: what came before is from the old position.
//!
//! At a gapless hand-over the player couples the next track to the playing
//! one ([`FeedHandle::couple`], a `Link` between their threads): the
//! playing one's resampler plays out into the next one's first frames, and
//! the next one is converted once more, from what its thread kept of its
//! start, beginning on the playing one's last frames — once the playing
//! one's decoder has got to its end, a second and a half ahead of the
//! boundary — into a ring of its own. The next one's source takes that ring
//! at its first pull if it is ready by then, and else plays the ring it had,
//! which its thread fills on as if nothing had been coupled: a coupled
//! hand-over is never later than one that is not. A next track let go of
//! (a skip, a queue edit) leaves the playing one to end on its own.

use super::resample::{Head, Stage, Tail};
use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};
use rtrb::{Consumer, Producer, RingBuffer};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{fence, AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::Thread;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How much decoded audio a ring holds.
const RING_SECONDS: f64 = 1.5;
/// Nor more than this many samples (16 MiB): 1.5 s of 384 kHz in eight
/// channels would be twice as much.
const RING_MAX_SAMPLES: usize = 4 * 1024 * 1024;
/// Frames the decoder thread puts into the ring at a time. Small: a decoder
/// that has to wait for its download in the middle of a chunk holds back what
/// it decoded of it until then.
const CHUNK_FRAMES: usize = 256;
/// Frames of silence a source hands out at a time while it has nothing else;
/// it looks again after each.
const SILENCE_FRAMES: usize = 256;
/// How long a decoder thread with nothing to do sleeps at most: a full ring
/// (then no longer than half of what the ring holds lasts), a decoder at its
/// end or stuck. Whatever the player asks of it, and letting go of its
/// download, wakes it at once; it notices a dropped source by itself.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Most frames in a span of audio. Bounds the silence a seek near the end of
/// a track can owe (see `RingSource::owed`); rodio's mixer looks at the
/// format again at least every 32 768 samples anyway.
const MAX_AUDIO_FRAMES: usize = 4096;
/// Audio a source waits for before it plays again: after a seek, and after it
/// ran dry in the middle of a track — a link that barely keeps up would
/// otherwise stutter every few milliseconds.
const AFTER_SEEK: Duration = Duration::from_millis(100);
const AFTER_STALL: Duration = Duration::from_millis(500);
/// The audio after a stall fades in over this long instead of setting in at
/// full level.
const STALL_FADE: Duration = Duration::from_millis(5);
/// A resampled track whose decoder got to its end before a next track was
/// coupled to it holds the resampler's tail back for one, as long as its ring
/// has more than this left to play: the player appends the next track three
/// seconds ahead, the decoder is a second and a half ahead — except in a
/// track shorter than that, or behind a slow open.
const SUCCESSOR_MARGIN: Duration = Duration::from_millis(500);
/// Less spare time than this is not waited for.
const SUCCESSOR_MIN_WAIT: Duration = Duration::from_millis(20);

/// A seek request packs its number above its target in µs.
const EPOCH_SHIFT: u32 = 44;
const EPOCH_MASK: u64 = (1 << (64 - EPOCH_SHIFT)) - 1;
const MICROS_MASK: u64 = (1 << EPOCH_SHIFT) - 1;
/// `View::end_at` while the decoder has not reached the end.
const NO_END: u64 = u64::MAX;
/// `View::seek_failed` while no seek failed.
const NO_FAILURE: u64 = u64::MAX;

fn pack(epoch: u64, at: Duration) -> u64 {
    let micros = u64::try_from(at.as_micros())
        .unwrap_or(MICROS_MASK)
        .min(MICROS_MASK);
    (epoch & EPOCH_MASK) << EPOCH_SHIFT | micros
}

fn unpack(word: u64) -> (u64, Duration) {
    (
        word >> EPOCH_SHIFT,
        Duration::from_micros(word & MICROS_MASK),
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the decoder thread is wanted elsewhere: a seek or a new ring was
/// asked for since it last looked. A read that waits for the download gives
/// up while it is (see the reader), so the thread gets to it at once.
///
/// Counted rather than flagged. A request is stored first and announced
/// after, and the thread takes every request announced before it looks; one
/// announced later stays pending until the next look. A flag cleared by the
/// thread would be set again by a request it had already carried out, and
/// stay set. A count can still be raised a moment late — by a source
/// preempted between storing and announcing — and cut one wait short that
/// needed no cutting: the thread then puts the decoder back where it was
/// (see `Feed::resync`).
#[derive(Debug, Default)]
pub struct Interrupt {
    wanted: AtomicU64,
    served: AtomicU64,
}

impl Interrupt {
    /// A request was stored: announce it.
    pub(super) fn raise(&self) {
        self.wanted.fetch_add(1, Ordering::SeqCst);
    }

    /// The thread is about to look at what was asked for.
    pub(super) fn settle(&self) {
        self.served
            .store(self.wanted.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// Something was asked for that the thread has not looked at yet.
    pub fn pending(&self) -> bool {
        self.wanted.load(Ordering::SeqCst) != self.served.load(Ordering::SeqCst)
    }
}

/// One ring's side of the hand-over between a source and its decoder thread:
/// what the source asked for, and what the decoder did about it.
#[derive(Debug)]
struct View {
    /// The seek the source asked for last: its number and target ([`pack`]).
    request: AtomicU64,
    /// The seek the decoder carried out last, and how many samples it had
    /// put into the ring by then: the target's audio comes after those.
    applied: AtomicU64,
    applied_at: AtomicU64,
    /// How many samples the ring gets in all, once the decoder has reached
    /// the end after that seek ([`NO_END`] until then).
    end_at: AtomicU64,
    /// Silence the source handed out since its last seek, in samples.
    silence: AtomicU64,
    /// How often the source ran dry.
    stalls: AtomicU32,
    /// A seek the decoder could not carry out ([`pack`]ed with its number),
    /// or [`NO_FAILURE`]. Only the one asked for last counts.
    seek_failed: AtomicU64,
    /// Samples the decoder put into the ring in all.
    pushed: AtomicU64,
    /// A coupled successor's ring: which ring its source plays, chosen at
    /// its first pull ([`CHOSEN_OPEN`] until then).
    chosen: AtomicU8,
    /// Tells this view (and the source of its ring) from every other one:
    /// what an offer is made for (see [`Shared::offer_for`]).
    id: u64,
    /// The ring's rate (the output's) and how many samples it holds.
    rate: SampleRate,
    capacity: usize,
}

/// [`View::chosen`]: not yet; this ring; the one the source had before.
const CHOSEN_OPEN: u8 = 0;
const CHOSEN_THIS: u8 = 1;
const CHOSEN_BEFORE: u8 = 2;

/// Where the views' ids come from: 0 is none.
static VIEW_IDS: AtomicU64 = AtomicU64::new(1);

impl View {
    /// A view whose source asks for seek `epoch` to `at`, and whose decoder
    /// has carried out `applied`, for a ring of `capacity` samples at `rate`.
    fn new(epoch: u64, at: Duration, applied: u64, rate: SampleRate, capacity: usize) -> Self {
        Self {
            request: AtomicU64::new(pack(epoch, at)),
            applied: AtomicU64::new(applied),
            applied_at: AtomicU64::new(0),
            end_at: AtomicU64::new(NO_END),
            silence: AtomicU64::new(0),
            stalls: AtomicU32::new(0),
            seek_failed: AtomicU64::new(NO_FAILURE),
            pushed: AtomicU64::new(0),
            chosen: AtomicU8::new(CHOSEN_OPEN),
            id: VIEW_IDS.fetch_add(1, Ordering::Relaxed),
            rate,
            capacity,
        }
    }

    fn frames(&self, duration: Duration) -> usize {
        (duration.as_secs_f64() * f64::from(self.rate.get())) as usize
    }

    /// `samples` samples of `channels` channels at the ring's rate.
    fn duration(&self, samples: u64, channels: ChannelCount) -> Duration {
        let per_second = f64::from(self.rate.get()) * f64::from(channels.get());
        Duration::from_secs_f64(samples as f64 / per_second)
    }
}

/// What a decoder thread, its source and the player share.
struct Shared {
    /// The ring's channel count, for the source's whole life. Its rate is
    /// the output's, and comes with each ring (see [`View`]).
    channels: ChannelCount,
    total_duration: Option<Duration>,
    /// The stream can be repositioned: it has a length (not a live
    /// transcode).
    seekable: bool,
    /// The view the decoder serves, as the player sees it.
    view: Mutex<Arc<View>>,
    handover: Mutex<Handover>,
    thread: OnceLock<Thread>,
    /// Cuts a read short that the decoder waits in (see the reader): the
    /// source asked for a seek, or for a new ring.
    interrupt: Arc<Interrupt>,
    /// The decoder crashed.
    failed: AtomicBool,
    /// The decoder came to a rate no music has (a broken stream), which the
    /// stage refused; 0 while it has not.
    refused: AtomicU32,
    /// The decoder could not carry out a seek. Positioning it again is left
    /// to a fresh open.
    seek_trouble: AtomicBool,
    /// The track's first frames, once the thread has them (only of a source
    /// started at the top): what the track before plays out into.
    head: OnceLock<Arc<Head>>,
    /// Every frame the decoder gave since the start is still at hand: the
    /// track can be converted once more without it, as a gapless successor.
    replayable: AtomicBool,
    succession: Mutex<Succession>,
    /// The ring of a coupled start, offered to the source at its first pull
    /// ([`FeedHandle::couple`]): a [`Box<Offer>`] or null. Taken with one
    /// swap, so the audio callback needs no lock for it.
    offer: AtomicPtr<Offer>,
    /// The id of the view whose source the offer is for ([`View::id`]), 0
    /// for none: a source on another ring (one repositioned since) does not
    /// take it.
    offer_for: AtomicU64,
    /// A coupled start was offered (once per source at most).
    offered: AtomicBool,
    /// A source of this thread has been pulled: it plays, and takes no
    /// offer any more.
    pulled: AtomicBool,
    /// The view of the ring offered last: how far it got, which ring the
    /// source chose.
    offered_view: Mutex<Option<Arc<View>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.withdraw_offer();
    }
}

impl Shared {
    /// Take back an offer no source has taken: whether there was one. Not on
    /// the audio callback — it frees the ring.
    fn withdraw_offer(&self) -> bool {
        self.offer_for.store(0, Ordering::SeqCst);
        let offer = self.offer.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if offer.is_null() {
            return false;
        }
        // SAFETY: only ever a pointer from `Box::into_raw`, swapped out once.
        drop(unsafe { Box::from_raw(offer) });
        true
    }

    /// The offer made for the source of view `id`, if there is one: once.
    /// No lock, no allocation; a source of another view leaves it alone.
    fn take_offer(&self, id: u64) -> Option<Box<Offer>> {
        if self.offer_for.load(Ordering::SeqCst) != id {
            return None;
        }
        let offer = self.offer.swap(std::ptr::null_mut(), Ordering::SeqCst);
        // SAFETY: only ever a pointer from `Box::into_raw`, swapped out once;
        // a box again, nothing is allocated.
        (!offer.is_null()).then(|| unsafe { Box::from_raw(offer) })
    }
}

/// A ring a source may change to at its first pull.
struct Offer {
    ring: Consumer<f32>,
    view: Arc<View>,
}

/// A ring for the decoder thread to change to, and whether it still runs:
/// once it has ended, no ring is handed over any more. A ring of a coupled
/// successor comes with its link to the track before.
struct Handover {
    running: bool,
    next: Option<NextRing>,
}

type NextRing = (Producer<f32>, Arc<View>, Option<Arc<Link>>);

/// What follows a track gaplessly, as far as its decoder thread is
/// concerned.
#[derive(Default)]
struct Succession {
    /// The next track, coupled to this one.
    next: Option<Arc<Link>>,
    /// Set once its decoder got to the end on the current ring (until a
    /// seek), with the end it left, if any: for a next track coupled after
    /// that.
    ended: Option<Option<Arc<Tail>>>,
}

/// Two tracks coupled for a gapless hand-over on one player (see
/// [`FeedHandle::couple`]): the next one's start, for the resampler of the
/// one before to play out into, and the one before's end, for the next one's
/// thread to start from — decided once, when that one's decoder gets to its
/// end, or goes without one.
struct Link {
    head: Arc<Head>,
    /// The rate of both rings.
    rate: SampleRate,
    /// None until decided; then the end to start from, if there is one.
    decision: Mutex<Option<Option<Arc<Tail>>>>,
    /// The next track's thread, when it waits for the decision.
    successor: Option<Thread>,
    /// The next track's side, to tell whether it is still there.
    next: Weak<Shared>,
}

impl Link {
    /// The next track was let go of (its download dropped, its thread
    /// ended): it never follows, and the track before ends on its own.
    fn gone(&self) -> bool {
        self.next
            .upgrade()
            .is_none_or(|next| !lock(&next.handover).running)
    }

    /// Decide what the next track starts from, unless that is decided.
    fn decide(&self, tail: Option<Arc<Tail>>) {
        let mut decision = lock(&self.decision);
        if decision.is_some() {
            return;
        }
        *decision = Some(tail);
        drop(decision);
        if let Some(thread) = &self.successor {
            thread.unpark();
        }
    }

    fn decision(&self) -> Option<Option<Arc<Tail>>> {
        lock(&self.decision).clone()
    }
}

impl Shared {
    /// The stage refused the decoder's `rate`: the source ends, failed.
    fn refuse(&self, rate: u32) {
        tracing::warn!("cannot resample a sample rate of {rate} Hz: the track ends");
        self.refused.store(rate.max(1), Ordering::Release);
    }

    fn take_next(&self) -> Option<NextRing> {
        lock(&self.handover).next.take()
    }

    /// The decoder got to the end on the current ring, leaving `tail` for a
    /// next track (or none it could start from): a coupled one is told now,
    /// one coupled later at once.
    fn ended(&self, tail: Option<Arc<Tail>>) {
        let next = {
            let mut succession = lock(&self.succession);
            succession.ended = Some(tail.clone());
            succession.next.clone()
        };
        if let Some(link) = next {
            link.decide(tail);
        }
    }

    /// The next track goes without this one's end: there is none (any more).
    fn release_successor(&self) {
        if let Some(link) = lock(&self.succession).next.take() {
            link.decide(None);
        }
    }

    /// The thread ends, unless a new ring waits for it.
    fn retire_unless_wanted(&self) -> bool {
        let mut handover = lock(&self.handover);
        if handover.next.is_some() {
            return false;
        }
        handover.running = false;
        true
    }

    /// The thread ends. A ring handed over meanwhile gets nothing: its source
    /// ends.
    fn retire(&self) {
        let mut handover = lock(&self.handover);
        handover.running = false;
        handover.next = None;
    }

    fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
}

fn ring_capacity(channels: ChannelCount, rate: SampleRate) -> usize {
    let channels = usize::from(channels.get());
    let frames = (f64::from(rate.get()) * RING_SECONDS) as usize;
    frames
        .min(RING_MAX_SAMPLES / channels)
        .max(4 * SILENCE_FRAMES)
        * channels
}

fn chunk_frames(capacity: usize, channels: usize) -> usize {
    CHUNK_FRAMES.min(capacity / channels / 4).max(1)
}

/// Start decoding `decoder`, positioned at `from`, on a thread of its own,
/// into a ring at `rate` (the output's). Returns the source the mixer pulls
/// and the player's handle on the thread. `seekable`: the stream can be
/// positioned (it has a length); `stop` ends the thread; `interrupt` is what
/// the decoder's reader gives up a wait on.
///
/// The first chunk is decoded right here, on the caller's thread (the
/// opener's): a source begins on audio, rather than on a span of silence that
/// would open a gap in a gapless hand-off.
pub fn start<S>(
    decoder: S,
    from: Duration,
    seekable: bool,
    stop: CancellationToken,
    interrupt: Arc<Interrupt>,
    rate: SampleRate,
) -> std::io::Result<(RingSource, FeedHandle)>
where
    S: Source + Send + 'static,
{
    let total_duration = decoder.total_duration();
    let mut stage = Stage::new(decoder, rate);
    let channels = stage.channels();
    let capacity = ring_capacity(channels, rate);
    let (mut producer, consumer) = RingBuffer::new(capacity);
    let view = Arc::new(View::new(0, Duration::ZERO, 0, rate, capacity));
    let shared = Arc::new(Shared {
        channels,
        total_duration,
        seekable,
        view: Mutex::new(view.clone()),
        handover: Mutex::new(Handover {
            running: true,
            next: None,
        }),
        thread: OnceLock::new(),
        interrupt,
        failed: AtomicBool::new(false),
        refused: AtomicU32::new(0),
        seek_trouble: AtomicBool::new(false),
        head: OnceLock::new(),
        replayable: AtomicBool::new(false),
        succession: Mutex::default(),
        offer: AtomicPtr::new(std::ptr::null_mut()),
        offer_for: AtomicU64::new(0),
        offered: AtomicBool::new(false),
        pulled: AtomicBool::new(false),
        offered_view: Mutex::default(),
    });
    let ch = usize::from(channels.get());
    if from.is_zero() {
        stage.keep_from_start(capacity / ch);
    }
    let (pushed, ended) = fill_first(&mut producer, &mut stage, chunk_frames(capacity, ch), ch);
    if ended {
        if let Some(rate) = stage.refused() {
            shared.refuse(rate);
            shared.ended(None);
        } else {
            // Too short to wait for a next track with.
            shared.ended(Some(Arc::new(stage.tail())));
        }
        view.end_at.store(pushed as u64, Ordering::Release);
    }
    let mut feed = Feed {
        stage,
        producer,
        view: view.clone(),
        shared: shared.clone(),
        stop,
        pushed: pushed as u64,
        state: if ended { Decoding::Ended } else { Decoding::On },
        done_epoch: 0,
        from,
        from_pushed: 0,
        resync: false,
        prev: None,
        coupled_epoch: None,
        fallback: None,
        tail_started: ended,
    };
    feed.publish();
    let thread = std::thread::Builder::new()
        .name("sonarune-decode".into())
        .spawn(move || feed.run())?;
    let _ = shared.thread.set(thread.thread().clone());
    let source = RingSource::new(consumer, view, shared.clone(), 0, Mode::Playing);
    Ok((source, FeedHandle { shared }))
}

/// Put up to `frames` frames from `source` into the ring, whole frames only:
/// a frame the source cuts short is made up with silence. Returns the samples
/// put in, and whether the source ran out.
fn fill<S: Iterator<Item = f32>>(
    producer: &mut Producer<f32>,
    source: &mut S,
    frames: usize,
    channels: usize,
) -> (usize, bool) {
    let Ok(mut chunk) = producer.write_chunk(frames * channels) else {
        return (0, false);
    };
    let (first, second) = chunk.as_mut_slices();
    let mut written = 0usize;
    let mut ran_out = false;
    for slot in first.iter_mut().chain(second.iter_mut()) {
        match source.next() {
            Some(sample) => {
                *slot = sample;
                written += 1;
            }
            None => {
                ran_out = true;
                break;
            }
        }
    }
    // The slots start out as silence.
    let whole = written.div_ceil(channels) * channels;
    chunk.commit(whole);
    (whole, ran_out)
}

/// The first chunk: up to `frames` frames, with what the resampler still
/// owes at the end of a track shorter than that. Returns the samples put in,
/// and whether that was all.
fn fill_first<S: Source>(
    producer: &mut Producer<f32>,
    stage: &mut Stage<S>,
    frames: usize,
    channels: usize,
) -> (usize, bool) {
    let mut pushed = 0;
    loop {
        let (written, ran_out) = fill(producer, stage, frames - pushed / channels, channels);
        pushed += written;
        if !ran_out {
            return (pushed, false);
        }
        if !stage.finish() {
            return (pushed, true);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decoding {
    On,
    /// The decoder got to the end, the resampler's tail is held back for a
    /// next track to be coupled (see [`Feed::successor_wait`]).
    Ending,
    /// At the end of the stream: idle until a seek.
    Ended,
    /// A seek failed: idle until the next one (or the end of the source).
    Stuck,
}

/// The ring a coupled successor had before its coupled one (see
/// [`FeedHandle::couple`]), and where the thread stood on it: kept until the
/// source has chosen between the two at its first pull, and gone back to
/// when it chose this one.
struct Fallback {
    producer: Producer<f32>,
    view: Arc<View>,
    pushed: u64,
    state: Decoding,
    done_epoch: u64,
    from: Duration,
    from_pushed: u64,
    tail_started: bool,
    ended: Option<Option<Arc<Tail>>>,
    /// The stage started anew for the coupled ring since: going back takes
    /// converting the start once more, on its own, up to where this ring
    /// got.
    restarted: bool,
}

/// The shortest the thread sleeps while a coupled source has not chosen its
/// ring and the one it had runs low (see `Feed::nap`): when it goes on with
/// that one, it is to be filled on soon.
const CHOICE_POLL: Duration = Duration::from_millis(10);

/// The decoder thread's side.
struct Feed<S> {
    stage: Stage<S>,
    producer: Producer<f32>,
    view: Arc<View>,
    shared: Arc<Shared>,
    stop: CancellationToken,
    /// Samples put into the current ring.
    pushed: u64,
    state: Decoding,
    /// The last seek of the current view seen to: carried out, or failed.
    done_epoch: u64,
    /// Where the decoder was put last, and how many samples the ring had
    /// then: where it stands is that plus what it put in since.
    from: Duration,
    from_pushed: u64,
    /// A read was cut short for nothing; the decoder is put back where it
    /// stands before it goes on.
    resync: bool,
    /// The current ring is that of a coupled successor (see
    /// [`FeedHandle::couple`]): the link to the track before, and the number
    /// of the seek that starts it, carried out once that track's end is
    /// decided.
    prev: Option<Arc<Link>>,
    coupled_epoch: Option<u64>,
    /// The ring before a coupled one, until the source has chosen.
    fallback: Option<Fallback>,
    /// The end is told to the stage on the current ring: once that has
    /// played out too, the track is over.
    tail_started: bool,
}

impl<S: Source> Feed<S> {
    fn run(mut self) {
        let shared = self.shared.clone();
        if std::panic::catch_unwind(AssertUnwindSafe(|| self.decode())).is_err() {
            // Its source plays what the ring holds and ends.
            shared.failed.store(true, Ordering::Release);
            tracing::error!("the decoder crashed");
        }
        shared.retire();
        // A next track still waiting for this one's end starts on its own.
        shared.release_successor();
        let stalls = self.view.stalls.load(Ordering::Relaxed);
        if stalls > 0 {
            tracing::debug!("the decoder fell behind {stalls} time(s)");
        }
        // The decoder and its reader go with this thread, not with the
        // source on the audio callback.
    }

    /// Sleep for `wait` at most — while a coupled source may still go on with
    /// the ring it had, back before a quarter of what that holds has played
    /// (it may be playing it already: nothing wakes the thread when it
    /// chooses), and no sooner than [`CHOICE_POLL`]. A full ring is looked at
    /// as seldom as ever.
    fn nap(&self, wait: Duration) {
        let wait = match &self.fallback {
            Some(fallback) => {
                let held = fallback.view.capacity - fallback.producer.slots();
                let lasts = fallback.view.duration(held as u64, self.shared.channels);
                wait.min((lasts / 4).max(CHOICE_POLL))
            }
            None => wait,
        };
        std::thread::park_timeout(wait);
    }

    fn decode(&mut self) {
        let channels = usize::from(self.shared.channels.get());
        loop {
            if self.stop.is_cancelled() {
                return;
            }
            // Whatever was asked for up to here is seen to below.
            self.shared.interrupt.settle();
            if let Some((producer, view, link)) = self.shared.take_next() {
                // Moved on: the old source plays what its ring holds and ends
                // — unless this is a coupled start, which the source may yet
                // turn down for the ring it has (see `RingSource::choose`).
                let producer = std::mem::replace(&mut self.producer, producer);
                let view = std::mem::replace(&mut self.view, view);
                if link.is_some() {
                    // At the same rate (see `FeedHandle::couple`): the stage
                    // stays as it is until the coupled start.
                    self.fallback = Some(Fallback {
                        producer,
                        view,
                        pushed: self.pushed,
                        state: self.state,
                        done_epoch: self.done_epoch,
                        from: self.from,
                        from_pushed: self.from_pushed,
                        tail_started: self.tail_started,
                        ended: lock(&self.shared.succession).ended.clone(),
                        restarted: false,
                    });
                } else {
                    // The new ring may be for an output at another rate; the
                    // seek below positions the decoder for it.
                    self.fallback = None;
                    self.stage.retarget(self.view.rate);
                }
                self.pushed = 0;
                self.done_epoch = self.view.applied.load(Ordering::Relaxed);
                self.resync = false;
                self.coupled_epoch = link
                    .is_some()
                    .then(|| unpack(self.view.request.load(Ordering::SeqCst)).0);
                self.prev = link;
                self.not_ended();
            }
            // What the stage has kept may have changed with the last seek.
            self.publish();
            if let Some(fallback) = &self.fallback {
                let gone = fallback.producer.is_abandoned();
                match self.view.chosen.load(Ordering::Acquire) {
                    CHOSEN_THIS => {
                        self.fallback = None;
                        *lock(&self.shared.view) = self.view.clone();
                    }
                    CHOSEN_BEFORE => {
                        self.go_back();
                        continue;
                    }
                    // Nothing pulls either ring: the source is gone.
                    _ if gone => {
                        if self.shared.retire_unless_wanted() {
                            return;
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            if self.producer.is_abandoned() {
                if self.shared.retire_unless_wanted() {
                    return;
                }
                continue;
            }
            let (epoch, _) = unpack(self.view.request.load(Ordering::SeqCst));
            if epoch != self.done_epoch {
                self.seek();
                continue;
            }
            if self.resync {
                self.resync();
                continue;
            }
            if self.state == Decoding::Ending {
                match self.successor_wait() {
                    Some(wait) => self.nap(wait),
                    None => self.end_track(),
                }
                continue;
            }
            if self.state != Decoding::On {
                self.nap(IDLE_POLL);
                continue;
            }
            let chunk = chunk_frames(self.view.capacity, channels);
            let free = self.producer.slots();
            if free < chunk * channels {
                // Full: back before half of what is there has played.
                let held = self.view.capacity - free;
                let lasts = self.view.duration(held as u64, self.shared.channels);
                self.nap((lasts / 2).min(IDLE_POLL));
                continue;
            }
            let (written, ran_out) = fill(&mut self.producer, &mut self.stage, chunk, channels);
            self.pushed += written as u64;
            self.publish();
            if !ran_out {
                continue;
            }
            if let Some(rate) = self.stage.refused() {
                // Not the end of the track: a failure, said so like a crash.
                self.shared.refuse(rate);
                self.state = Decoding::Ended;
                self.view.end_at.store(self.pushed, Ordering::Release);
                self.shared.ended(None);
            } else if self.shared.interrupt.pending() {
                // A read cut short for the player is no end: the loop goes on
                // with what it asked for — or, when that turns out to be
                // nothing, from where the decoder stands. The decoder gave up
                // on what it read: what it gave is not all of the track's
                // start any more.
                self.resync = self.shared.seekable;
                self.stage.forget_kept();
                self.publish();
            } else if self.tail_started {
                // The resampler's tail is out as well.
                self.state = Decoding::Ended;
                self.view.end_at.store(self.pushed, Ordering::Release);
            } else {
                // The end: seen to at the top, maybe after a wait.
                self.state = Decoding::Ending;
            }
        }
    }

    /// Tell the player's side what the stage has of the track's start, and
    /// how far the ring got.
    fn publish(&mut self) {
        if self.shared.head.get().is_none() {
            if let Some(head) = self.stage.take_head() {
                let _ = self.shared.head.set(Arc::new(head));
            }
        }
        self.shared
            .replayable
            .store(self.stage.replayable(), Ordering::Release);
        self.view.pushed.store(self.pushed, Ordering::Release);
    }

    /// The next track coupled to this one, unless it was let go of.
    fn successor(&self) -> Option<Arc<Link>> {
        let next = lock(&self.shared.succession).next.clone()?;
        (!next.gone()).then_some(next)
    }

    /// How long the end may wait for a next track to be coupled
    /// ([`FeedHandle::couple`]): while there is none, the track is
    /// resampled (it has a tail to play out into it) and the ring holds more
    /// than [`SUCCESSOR_MARGIN`]. A quarter of the spare time at a time: at
    /// twice the speed, half of it plays meanwhile. None: end now.
    fn successor_wait(&self) -> Option<Duration> {
        if !self.stage.resampling() || self.successor().is_some() {
            return None;
        }
        let held = self.view.capacity - self.producer.slots();
        let lasts = self.view.duration(held as u64, self.shared.channels);
        let spare = lasts.checked_sub(SUCCESSOR_MARGIN)?;
        (spare >= SUCCESSOR_MIN_WAIT).then(|| (spare / 4).min(IDLE_POLL))
    }

    /// The decoder is at the end: the resampler plays out into a coupled next
    /// track's start, if there is one, and that track gets this one's end.
    fn end_track(&mut self) {
        self.tail_started = true;
        if let Some(link) = self.successor().filter(|link| link.rate == self.view.rate) {
            self.stage.continue_into(&link.head);
            tracing::debug!("the track plays out into the start of the next one");
        }
        self.shared.ended(Some(Arc::new(self.stage.tail())));
        if self.stage.finish() {
            // The resampler's tail still to come.
            self.state = Decoding::On;
        } else {
            self.state = Decoding::Ended;
            self.view.end_at.store(self.pushed, Ordering::Release);
        }
    }

    /// Not at the end (any more): on a new ring, after a seek.
    fn not_ended(&mut self) {
        self.tail_started = false;
        lock(&self.shared.succession).ended = None;
    }

    /// Back to the ring the source had before its coupled one, which it
    /// chose at its first pull (the coupled start was not ready in time), or
    /// which there is no coupled start for: the track goes on there exactly
    /// as it would have without the coupling. After a coupled start the stage
    /// converts the track's start once more, on its own, and passes over what
    /// that ring holds.
    fn go_back(&mut self) {
        let Some(fallback) = self.fallback.take() else {
            return;
        };
        self.coupled_epoch = None;
        self.prev = None;
        let mut again = Ok(());
        if fallback.restarted {
            again = self.stage.restart_after(None).map(|_| ());
            let mut left = fallback.pushed;
            while again.is_ok() && left > 0 {
                match self.stage.next() {
                    Some(_) => left -= 1,
                    None if self.stage.finish() => {}
                    None => break,
                }
            }
        }
        self.producer = fallback.producer;
        self.view = fallback.view;
        *lock(&self.shared.view) = self.view.clone();
        self.pushed = fallback.pushed;
        self.state = fallback.state;
        self.done_epoch = fallback.done_epoch;
        self.from = fallback.from;
        self.from_pushed = fallback.from_pushed;
        self.tail_started = fallback.tail_started;
        lock(&self.shared.succession).ended = fallback.ended;
        tracing::debug!("the track goes on uncoupled: its coupled start was not ready in time");
        if let Err(e) = again {
            self.seek_failed(self.done_epoch, Duration::ZERO, &e);
        }
    }

    /// Where the decoder stands: the media time of the next sample it puts
    /// into the ring. (What the resampler holds is decoded again from there.)
    fn position(&self) -> Duration {
        let since = self.pushed.saturating_sub(self.from_pushed);
        self.from + self.view.duration(since, self.shared.channels)
    }

    /// Put the decoder back where it stands, after a read was cut short with
    /// no seek to carry out: the decoder gave up on what it was reading (an
    /// Ogg reader for good), and would go on from a broken packet otherwise.
    fn resync(&mut self) {
        self.resync = false;
        let at = self.position();
        match self.stage.try_seek(at) {
            Ok(()) => {}
            Err(_) if self.shared.interrupt.pending() => self.resync = true,
            // Under the number of the seek it stands on: a newer one, stored
            // meanwhile, is carried out next and makes this failure moot.
            Err(e) => self.seek_failed(self.done_epoch, at, &e),
        }
    }

    /// A seek that did not work: the source stays silent, and the player
    /// reopens the track there.
    fn seek_failed(&mut self, epoch: u64, target: Duration, e: &SeekError) {
        tracing::warn!("the decoder could not seek: {e}");
        self.state = Decoding::Stuck;
        self.shared.seek_trouble.store(true, Ordering::Release);
        self.view
            .seek_failed
            .store(pack(epoch, target), Ordering::Release);
    }

    /// Carry out the seek asked for last.
    fn seek(&mut self) {
        let (epoch, target) = unpack(self.view.request.load(Ordering::SeqCst));
        if self.coupled_epoch == Some(epoch) {
            self.start_coupled(epoch);
            return;
        }
        // Any other seek: the track goes on on its own.
        self.coupled_epoch = None;
        self.prev = None;
        match self.stage.try_seek(target) {
            Ok(()) => self.positioned(epoch, target),
            // Cut short by a newer request: that one is carried out next.
            Err(_) if self.shared.interrupt.pending() => return,
            Err(e) => self.seek_failed(epoch, target, &e),
        }
        self.done_epoch = epoch;
    }

    /// Start a coupled successor's ring: from the track's first frame,
    /// starting on the end of the track before — once that is decided, until
    /// then the thread waits (the track before is woken by its own end, a
    /// second and a half before this one plays). With the track's start no
    /// longer all at hand, back to the ring before.
    fn start_coupled(&mut self, epoch: u64) {
        if !self.stage.replayable() {
            self.go_back();
            return;
        }
        let Some(decision) = self.prev.as_ref().and_then(|link| link.decision()) else {
            self.nap(IDLE_POLL);
            return;
        };
        self.coupled_epoch = None;
        self.prev = None;
        if let Some(fallback) = &mut self.fallback {
            fallback.restarted = true;
        }
        match self.stage.restart_after(decision.as_deref()) {
            Ok(coupled) => {
                if coupled {
                    tracing::debug!("the track starts on the end of the one before");
                }
                self.positioned(epoch, Duration::ZERO);
            }
            Err(e) => self.seek_failed(epoch, Duration::ZERO, &e),
        }
        self.done_epoch = epoch;
    }

    /// The decoder stands at `target` for seek `epoch`.
    fn positioned(&mut self, epoch: u64, target: Duration) {
        self.state = Decoding::On;
        self.resync = false;
        self.from = target;
        self.from_pushed = self.pushed;
        self.not_ended();
        // A failure before this one is over and done with.
        self.shared.seek_trouble.store(false, Ordering::Release);
        self.view.seek_failed.store(NO_FAILURE, Ordering::Release);
        self.view.end_at.store(NO_END, Ordering::Relaxed);
        self.view.applied_at.store(self.pushed, Ordering::Relaxed);
        self.view.applied.store(epoch, Ordering::Release);
    }
}

/// The player's handle on a source's decoder thread. Holding it keeps
/// nothing alive.
#[derive(Clone)]
pub struct FeedHandle {
    shared: Arc<Shared>,
}

impl FeedHandle {
    /// The silence the source has handed out since its last seek, which rodio
    /// counts as played: what plays now is this much earlier in the track
    /// than rodio's position says.
    pub fn silence(&self) -> Duration {
        let view = lock(&self.shared.view).clone();
        let samples = view.silence.load(Ordering::Relaxed);
        view.duration(samples, self.shared.channels)
    }

    /// The stream can be positioned (it has a length).
    pub fn seekable(&self) -> bool {
        self.shared.seekable
    }

    /// The target of a seek the decoder could not carry out, once — unless
    /// the source has asked for another one since, which counts instead.
    pub fn take_seek_failure(&self) -> Option<Duration> {
        let view = lock(&self.shared.view).clone();
        let failed = view.seek_failed.swap(NO_FAILURE, Ordering::AcqRel);
        if failed == NO_FAILURE {
            return None;
        }
        let (epoch, target) = unpack(failed);
        let (asked, _) = unpack(view.request.load(Ordering::SeqCst));
        (epoch == asked).then_some(target)
    }

    /// The decoder crashed, or came to a rate that cannot be resampled: its
    /// source ended early.
    pub fn failed(&self) -> bool {
        self.failure().is_some()
    }

    /// Why the source ended early, if it did.
    pub fn failure(&self) -> Option<String> {
        let refused = self.shared.refused.load(Ordering::Acquire);
        if refused != 0 {
            Some(format!(
                "cannot decode the stream: a sample rate of {refused} Hz"
            ))
        } else if self.shared.failed.load(Ordering::Acquire) {
            Some("the decoder crashed".into())
        } else {
            None
        }
    }

    /// The decoder thread still runs.
    #[cfg(test)]
    pub(super) fn running(&self) -> bool {
        lock(&self.shared.handover).running
    }

    /// Have the thread look at what its source asked for now rather than at
    /// its next poll. For the player: the source asks from the audio callback,
    /// which wakes nobody.
    pub fn wake(&self) {
        self.shared.wake();
    }

    /// A new source on the same decoder, starting at `at`, at `rate` (the
    /// output's, which may have changed): the decoder seeks there and fills a
    /// ring of the new source's own, and the old source plays what its ring
    /// holds and ends. The download carries on, with what it has on disk.
    /// None when the decoder cannot be positioned (a live stream, a crashed
    /// decoder, one a seek failed in) or its thread has ended.
    pub fn reposition(&self, at: Duration, rate: SampleRate) -> Option<RingSource> {
        let shared = &self.shared;
        if !shared.seekable
            || shared.failed.load(Ordering::Acquire)
            || shared.refused.load(Ordering::Acquire) != 0
            || shared.seek_trouble.load(Ordering::Acquire)
        {
            return None;
        }
        let capacity = ring_capacity(shared.channels, rate);
        let (producer, consumer) = RingBuffer::new(capacity);
        let view = Arc::new(View::new(1, at, 0, rate, capacity));
        {
            let mut handover = lock(&shared.handover);
            if !handover.running {
                return None;
            }
            // A coupled start offered to the source before goes with the
            // ring it was for: nothing takes it now, and the thread lets go
            // of the ring it would have filled for it.
            shared.withdraw_offer();
            handover.next = Some((producer, view.clone(), None));
            *lock(&shared.view) = view.clone();
            shared.interrupt.raise();
        }
        shared.wake();
        let after_seek = Mode::Waiting {
            resume_at: RingSource::resume_after(&view, shared.channels, AFTER_SEEK),
            stalled: false,
        };
        Some(RingSource::new(
            consumer,
            view,
            shared.clone(),
            1,
            after_seek,
        ))
    }

    /// Couple `next`, the track this one hands over to gaplessly on one
    /// player at `rate`, to this one: this one's resampler plays out into
    /// `next`'s first frames instead of holding its last one, and `next`, if
    /// it is resampled, is converted once more from its first frame, starting
    /// on this one's last frames and on the output grid this one ends on —
    /// as if the two were one piece (see `resample.rs`). That needs what
    /// `next`'s thread kept of its start, and this one's decoder at its end.
    ///
    /// The converted start goes into a ring of its own, offered to `next`'s
    /// source, which takes it at its first pull if it is ready by then — and
    /// else plays the ring it has, which its thread then fills on as if
    /// nothing had been coupled. So the hand-over is never later than an
    /// uncoupled one. Returns whether `next` is converted anew (it is
    /// resampled, all it decoded is at hand, and its source has not played).
    pub fn couple(&self, next: &FeedHandle, rate: SampleRate) -> bool {
        self.couple_then(next, rate, || {})
    }

    /// [`Self::couple`], with `meanwhile` between offering the coupled
    /// start and looking whether the source has been pulled.
    fn couple_then(&self, next: &FeedHandle, rate: SampleRate, meanwhile: impl FnOnce()) -> bool {
        let Some(head) = next.shared.head.get().cloned() else {
            tracing::debug!("gapless hand-over not coupled: the next track's start is not at hand");
            return false;
        };
        if lock(&self.shared.view).rate != rate {
            return false;
        }
        let shared = &next.shared;
        let head_rate = head.rate();
        // At the output's rate it is untouched: nothing to start anew. The
        // ring it has is what it falls back on, at the same rate.
        let renew = head_rate != rate.get()
            && lock(&shared.view).rate == rate
            && shared.replayable.load(Ordering::Acquire)
            && !shared.failed.load(Ordering::Acquire)
            && shared.refused.load(Ordering::Acquire) == 0
            && !shared.seek_trouble.load(Ordering::Acquire)
            && !shared.pulled.load(Ordering::SeqCst)
            && !shared.offered.swap(true, Ordering::AcqRel);
        let link = Arc::new(Link {
            head,
            rate,
            decision: Mutex::new(None),
            successor: shared.thread.get().cloned().filter(|_| renew),
            next: Arc::downgrade(shared),
        });
        {
            let mut succession = lock(&self.shared.succession);
            if let Some(old) = succession.next.replace(link.clone()) {
                old.decide(None);
            }
            // At its end already (a short track, a late append).
            if let Some(tail) = &succession.ended {
                link.decide(tail.clone());
            }
        }
        // Its thread gone (it crashed): it never gets there. One that goes
        // after this lets go of the link as it goes.
        if !lock(&self.shared.handover).running {
            link.decide(None);
        }
        // It may hold its end back for this.
        self.shared.wake();
        let renewed = renew && next.offer_coupled_then(rate, link, meanwhile);
        tracing::info!(
            "gapless hand-over coupled to the next track ({head_rate} Hz, {})",
            if renewed {
                "converted anew from this track's end"
            } else {
                "as it is"
            }
        );
        renewed
    }

    /// Offer the source a ring for a coupled start (see [`Self::couple`]),
    /// and hand it over to the thread — unless the source has been pulled
    /// meanwhile without taking it, or the thread has ended.
    /// `meanwhile` comes between offering and looking whether the source has
    /// been pulled (for the tests: a first pull there).
    fn offer_coupled_then(
        &self,
        rate: SampleRate,
        link: Arc<Link>,
        meanwhile: impl FnOnce(),
    ) -> bool {
        let shared = &self.shared;
        let capacity = ring_capacity(shared.channels, rate);
        let (producer, consumer) = RingBuffer::new(capacity);
        let view = Arc::new(View::new(1, Duration::ZERO, 0, rate, capacity));
        *lock(&shared.offered_view) = Some(view.clone());
        let offer = Box::into_raw(Box::new(Offer {
            ring: consumer,
            view: view.clone(),
        }));
        // Under the hand-over's lock: a reposition, which withdraws the
        // offer, comes before all of this or after it.
        let mut handover = lock(&shared.handover);
        if !handover.running {
            // SAFETY: the box just made, never shared.
            drop(unsafe { Box::from_raw(offer) });
            return false;
        }
        // For the source of the ring there is now, and offered before it is
        // looked at whether that has played: a first pull after this takes
        // the offer, one before it does not.
        let current = lock(&shared.view).id;
        shared.offer_for.store(current, Ordering::SeqCst);
        let replaced = shared.offer.swap(offer, Ordering::SeqCst);
        debug_assert!(replaced.is_null(), "offered once");
        meanwhile();
        // Pulled and not taken: it plays the ring it has. Pulled and taken:
        // it chose, and the thread is to know.
        if shared.pulled.load(Ordering::SeqCst) && shared.withdraw_offer() {
            return false;
        }
        handover.next = Some((producer, view, Some(link)));
        drop(handover);
        shared.wake();
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Playing,
    /// Silent until the ring holds `resume_at` samples (or all there will
    /// be); `stalled`: it ran dry mid-track, and fades back in.
    Waiting {
        resume_at: usize,
        stalled: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Span {
    Audio,
    Silence,
    End,
}

/// What the mixer pulls: the audio a decoder thread put into its ring, or
/// silence while there is none. Runs on the audio callback: no locks, no
/// allocation, no waiting — atomics and the ring, nothing else.
///
/// It goes in spans, each decided when the one before ends: audio as far as
/// the ring holds it, or a short stretch of silence. A span is never longer
/// than what is sure to come, so the source ends exactly on its last sample —
/// where rodio looks at the next source's format.
pub struct RingSource {
    ring: Consumer<f32>,
    view: Arc<View>,
    shared: Arc<Shared>,
    channels: usize,
    /// The seek asked for last.
    epoch: u64,
    mode: Mode,
    span: Span,
    /// Samples left in the span.
    left: usize,
    /// Channel of the next sample: seeks come mid-frame.
    phase: usize,
    /// Samples taken out of the ring, played or thrown away.
    popped: u64,
    silence: u64,
    stalls: u32,
    /// Frames of the fade-in after a stall still to go, of `fade_frames`.
    fade_left: usize,
    fade_frames: usize,
    resume_after_seek: usize,
    resume_after_stall: usize,
    /// Samples still owed since a seek: what was left of the span the seek
    /// cut short. rodio's mixer counted on that many before it looks at the
    /// format again, so the source does not end before they are out — or the
    /// mixer would run the next track of the player through this one's
    /// converter (wrong speed, swapped channels). Owed samples are silence.
    owed: usize,
    /// Nothing pulled yet: at the first pull, a ring offered for a coupled
    /// start is chosen or not ([`Self::choose`]), and the first span decided
    /// again — the ring has filled since the source was made.
    fresh: bool,
    /// The ring not chosen at the first pull, kept rather than dropped here:
    /// its buffer is let go of with the source, on the reaper thread.
    before: Option<Box<Offer>>,
}

impl RingSource {
    fn new(
        ring: Consumer<f32>,
        view: Arc<View>,
        shared: Arc<Shared>,
        epoch: u64,
        mode: Mode,
    ) -> Self {
        let mut source = Self {
            channels: usize::from(shared.channels.get()),
            resume_after_seek: Self::resume_after(&view, shared.channels, AFTER_SEEK),
            resume_after_stall: Self::resume_after(&view, shared.channels, AFTER_STALL),
            fade_frames: view.frames(STALL_FADE).max(1),
            ring,
            view,
            shared,
            epoch,
            mode,
            span: Span::Silence,
            left: 0,
            phase: 0,
            popped: 0,
            silence: 0,
            stalls: 0,
            fade_left: 0,
            owed: 0,
            fresh: true,
            before: None,
        };
        source.begin_span();
        source
    }

    /// Before the first sample (or seek): from now on the source plays; it
    /// takes a ring offered for a coupled start, or not, and decides its
    /// first span again.
    fn first_pull(&mut self) {
        self.fresh = false;
        // What rodio may have read as the first span's length is out before
        // the source can end.
        self.owed = self.owed.max(self.left);
        self.shared.pulled.store(true, Ordering::SeqCst);
        // Only the offer made for this source's own ring.
        if let Some(offer) = self.shared.take_offer(self.view.id) {
            self.choose(offer);
        }
        self.begin_span();
    }

    /// The first pull of a source that was offered a ring for a coupled
    /// start ([`FeedHandle::couple`]): it plays that one if its start is in,
    /// [`AFTER_SEEK`] of it or all there is — else the ring it had, which its
    /// thread then fills on as if nothing had been coupled. Either way the
    /// thread is told, and the other ring kept. No lock, no allocation.
    fn choose(&mut self, mut offer: Box<Offer>) {
        let ready = offer.view.applied.load(Ordering::Acquire) == 1 && {
            let held = offer.ring.slots() / self.channels * self.channels;
            held >= self.resume_after_seek
                || (held > 0 && offer.view.end_at.load(Ordering::Acquire) != NO_END)
        };
        if ready {
            std::mem::swap(&mut self.ring, &mut offer.ring);
            std::mem::swap(&mut self.view, &mut offer.view);
            self.epoch = 1;
            self.mode = Mode::Playing;
            self.popped = 0;
            self.silence = 0;
            self.view.chosen.store(CHOSEN_THIS, Ordering::Release);
        } else {
            offer.view.chosen.store(CHOSEN_BEFORE, Ordering::Release);
        }
        self.before = Some(offer);
    }

    /// Samples of `wait` to wait for, at most half a ring: a ring is never
    /// quite full.
    fn resume_after(view: &View, channels: ChannelCount, wait: Duration) -> usize {
        let channels = usize::from(channels.get());
        let most = view.capacity / 2 / channels * channels;
        (view.frames(wait) * channels).min(most).max(channels)
    }

    /// Throw away `count` samples of the ring.
    fn discard(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        if let Ok(chunk) = self.ring.read_chunk(count) {
            chunk.commit_all();
            self.popped += count as u64;
        }
    }

    /// Throw away what the ring holds from before the seek asked for last.
    /// Returns where the stream ends after it (or [`NO_END`]), or None while
    /// the decoder has not carried it out.
    fn settle(&mut self) -> Option<u64> {
        // What the ring holds is looked at first: when the seek is not
        // carried out after that, all of it is from before. Looked at the
        // other way round, the target's first audio could arrive in between
        // and be thrown away with the rest.
        let held = self.ring.slots();
        if self.view.applied.load(Ordering::Acquire) != self.epoch {
            self.discard(held);
            return None;
        }
        let from = self.view.applied_at.load(Ordering::Relaxed);
        let end = self.view.end_at.load(Ordering::Acquire);
        if self.popped < from {
            let stale = (from - self.popped).min(self.ring.slots() as u64) as usize;
            self.discard(stale);
            if self.popped < from {
                return None;
            }
        }
        Some(end)
    }

    /// Decide the next span. Called at a frame boundary.
    fn begin_span(&mut self) {
        // The decoder thread let go of this ring (it ended, or the source was
        // given a new one): what the ring holds is all there will be.
        let gone = self.ring.is_abandoned();
        if gone {
            fence(Ordering::Acquire);
        }
        let end = self.settle();
        let ready = self.ring.slots() / self.channels * self.channels;
        let Some(end) = end else {
            // The decoder has not got to the seek yet, and it never will once
            // it is gone.
            return if gone { self.finish() } else { self.quiet() };
        };
        let finished = gone || end != NO_END;
        match self.mode {
            Mode::Waiting { resume_at, stalled } => {
                if ready == 0 && finished {
                    self.finish();
                } else if ready >= resume_at || (finished && ready > 0) {
                    self.mode = Mode::Playing;
                    if stalled {
                        self.fade_left = self.fade_frames;
                    }
                    self.audio(ready);
                } else {
                    self.quiet();
                }
            }
            Mode::Playing => {
                if ready > 0 {
                    self.audio(ready);
                } else if finished {
                    self.finish();
                } else {
                    self.stalls = self.stalls.wrapping_add(1);
                    self.view.stalls.store(self.stalls, Ordering::Relaxed);
                    self.mode = Mode::Waiting {
                        resume_at: self.resume_after_stall,
                        stalled: true,
                    };
                    self.quiet();
                }
            }
        }
    }

    fn audio(&mut self, ready: usize) {
        self.span = Span::Audio;
        self.left = ready.min(MAX_AUDIO_FRAMES * self.channels);
    }

    fn quiet(&mut self) {
        self.span = Span::Silence;
        self.left = SILENCE_FRAMES * self.channels;
    }

    /// The end, once what a seek left owed is out.
    fn finish(&mut self) {
        if self.owed > 0 {
            self.span = Span::Silence;
            self.left = self.owed;
        } else {
            self.span = Span::End;
            self.left = 0;
        }
    }

    #[inline]
    fn faded(&mut self, sample: f32) -> f32 {
        if self.fade_left == 0 {
            return sample;
        }
        let gain = 1.0 - self.fade_left as f32 / (self.fade_frames + 1) as f32;
        if self.phase + 1 == self.channels {
            self.fade_left -= 1;
        }
        sample * gain
    }
}

impl Iterator for RingSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.fresh {
            self.first_pull();
        }
        let sample = match self.span {
            Span::End => return None,
            Span::Audio => {
                // The span holds no more than the ring did.
                let sample = self.ring.pop().unwrap_or(0.0);
                self.popped += 1;
                self.faded(sample)
            }
            Span::Silence => {
                self.silence += 1;
                self.view.silence.store(self.silence, Ordering::Relaxed);
                0.0
            }
        };
        self.phase += 1;
        if self.phase == self.channels {
            self.phase = 0;
        }
        self.owed = self.owed.saturating_sub(1);
        self.left -= 1;
        if self.left == 0 {
            self.begin_span();
        }
        Some(sample)
    }
}

impl Source for RingSource {
    /// What is left of the span: what rodio's stages count down to look at
    /// the format again, and 0 only at the end.
    fn current_span_len(&self) -> Option<usize> {
        Some(self.left)
    }

    fn channels(&self) -> ChannelCount {
        self.shared.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.view.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.shared.total_duration
    }

    /// Ask the decoder thread for `pos`. Returns at once: the ring is
    /// emptied, and the source is silent until the target's audio is in.
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        if !self.shared.seekable {
            return Err(SeekError::NotSupported {
                underlying_source: "a live stream",
            });
        }
        if self.fresh {
            // The seek is on the ring it plays.
            self.first_pull();
        }
        self.epoch = (self.epoch + 1) & EPOCH_MASK;
        self.view
            .request
            .store(pack(self.epoch, pos), Ordering::SeqCst);
        self.shared.interrupt.raise();
        // The span this cuts short was counted on downstream.
        self.owed = self.owed.max(self.left);
        let stale = self.ring.slots();
        self.discard(stale);
        // rodio starts counting the position anew from the target.
        self.silence = 0;
        self.view.silence.store(0, Ordering::Relaxed);
        self.mode = Mode::Waiting {
            resume_at: self.resume_after_seek,
            stalled: false,
        };
        self.fade_left = 0;
        if self.phase == 0 {
            self.begin_span();
        } else {
            // The frame in progress ends in silence; the target's audio
            // starts on a frame of its own.
            self.span = Span::Silence;
            self.left = self.channels - self.phase;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::sync::Condvar;
    use std::time::Instant;

    /// What a test decoder may decode so far — the part of the track that is
    /// "downloaded" — and what it tells about itself.
    #[derive(Default)]
    struct Script {
        allowed: Mutex<usize>,
        more: Condvar,
        dropped: AtomicBool,
    }

    impl Script {
        fn allow(&self, frames: usize) {
            *lock(&self.allowed) = frames;
            self.more.notify_all();
        }
    }

    /// A decoder of `frames` frames whose samples say where they are: frame
    /// `f`, channel `c` is `f + 1 + c / 4`. It waits for frames the script
    /// has not allowed yet, as a decoder waits for its download, and gives
    /// up the wait when interrupted or stopped, as the reader does — and then
    /// has nothing more until it is seeked, as the Ogg reader.
    struct Fake {
        channels: u16,
        rate: u32,
        frames: usize,
        pos: usize,
        script: Arc<Script>,
        interrupt: Arc<Interrupt>,
        stop: CancellationToken,
        panic_at: Option<usize>,
        seek_fails: Arc<AtomicBool>,
        broken: bool,
    }

    fn value(frame: usize, channel: usize) -> f32 {
        frame as f32 + 1.0 + channel as f32 / 4.0
    }

    impl Iterator for Fake {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            let channels = usize::from(self.channels);
            if self.broken || self.pos >= self.frames * channels {
                return None;
            }
            let frame = self.pos / channels;
            if self.panic_at == Some(frame) {
                panic!("a test decoder crashing on purpose");
            }
            let mut allowed = lock(&self.script.allowed);
            // A test that leaves it waiting does not leave its thread behind
            // for long.
            let given_up = Instant::now() + Duration::from_secs(20);
            while frame >= *allowed {
                if self.interrupt.pending() || self.stop.is_cancelled() || Instant::now() > given_up
                {
                    self.broken = true;
                    return None;
                }
                allowed = self
                    .script
                    .more
                    .wait_timeout(allowed, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
            let sample = value(frame, self.pos % channels);
            self.pos += 1;
            Some(sample)
        }
    }

    impl Source for Fake {
        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> ChannelCount {
            ChannelCount::new(self.channels).unwrap()
        }

        fn sample_rate(&self) -> SampleRate {
            SampleRate::new(self.rate).unwrap()
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }

        fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
            if self.seek_fails.load(Ordering::SeqCst) {
                return Err(SeekError::NotSupported {
                    underlying_source: "a test decoder",
                });
            }
            let frame = (pos.as_secs_f64() * f64::from(self.rate)).round() as usize;
            self.pos = frame.min(self.frames) * usize::from(self.channels);
            self.broken = false;
            Ok(())
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.script.dropped.store(true, Ordering::SeqCst);
        }
    }

    const RATE: u32 = 8_000;

    /// The output's rate in the tests that resample nothing.
    fn out_rate() -> SampleRate {
        SampleRate::new(RATE).unwrap()
    }

    struct Test {
        source: RingSource,
        handle: FeedHandle,
        script: Arc<Script>,
        stop: CancellationToken,
    }

    fn fake(channels: u16, frames: usize, allowed: usize) -> (Fake, Arc<Script>) {
        let script = Arc::new(Script::default());
        script.allow(allowed);
        let fake = Fake {
            channels,
            rate: RATE,
            frames,
            pos: 0,
            script: script.clone(),
            interrupt: Arc::default(),
            stop: CancellationToken::new(),
            panic_at: None,
            seek_fails: Arc::default(),
            broken: false,
        };
        (fake, script)
    }

    fn start_fake(fake: Fake, seekable: bool) -> Test {
        start_fake_into(fake, seekable, out_rate())
    }

    /// `fake` started into a ring at `rate`.
    fn start_fake_into(fake: Fake, seekable: bool, rate: SampleRate) -> Test {
        let script = fake.script.clone();
        let stop = fake.stop.clone();
        let interrupt = fake.interrupt.clone();
        let (source, handle) = start(
            fake,
            Duration::ZERO,
            seekable,
            stop.clone(),
            interrupt,
            rate,
        )
        .expect("started");
        Test {
            source,
            handle,
            script,
            stop,
        }
    }

    fn feed(channels: u16, frames: usize, allowed: usize) -> Test {
        start_fake(fake(channels, frames, allowed).0, true)
    }

    fn eventually(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Pull `count` samples as the callback would: a silent span gives the
    /// decoder thread a moment.
    fn pull(source: &mut RingSource, count: usize) -> Vec<Option<f32>> {
        (0..count)
            .map(|_| {
                if source.span == Span::Silence && source.left == SILENCE_FRAMES * source.channels {
                    std::thread::sleep(Duration::from_millis(1));
                }
                source.next()
            })
            .collect()
    }

    /// The audio in `out` (the samples that are not silence), unfaded: the
    /// frames it holds, each as its first channel's value.
    fn frames_heard(out: &[Option<f32>], channels: usize) -> Vec<usize> {
        out.chunks(channels)
            .filter_map(|frame| {
                let first = frame[0]?;
                (first != 0.0).then(|| {
                    // Faded samples come out lower; the second channel keeps
                    // the proportion.
                    first.round() as usize - 1
                })
            })
            .collect()
    }

    #[test]
    fn a_source_that_runs_dry_plays_silence_and_goes_on() {
        let Test {
            mut source, script, ..
        } = feed(1, 6_000, 2_000);
        eventually("the decoder at the stall", || {
            source.ring.slots() + CHUNK_FRAMES >= 2_000
        });
        // What the decoder got to before the download stalled plays, then
        // silence: never the end.
        let first = pull(&mut source, 8_000);
        assert!(first.iter().all(Option::is_some), "it ended while waiting");
        let heard = frames_heard(&first, 1);
        let stalled_at = heard.len();
        assert!(
            stalled_at + CHUNK_FRAMES >= 2_000,
            "only {stalled_at} frames"
        );
        assert_eq!(heard, (0..stalled_at).collect::<Vec<_>>());
        assert!(first[stalled_at..].iter().all(|s| *s == Some(0.0)));
        assert_eq!(source.stalls, 1);
        // The rest arrives: it plays on from where it stopped, fading in, and
        // ends after the last frame.
        script.allow(6_000);
        eventually("the rest decoded", || {
            source.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let rest = pull(&mut source, 40_000);
        let end = rest.iter().position(Option::is_none).expect("it ended");
        assert!(rest[end..].iter().all(Option::is_none));
        let rest: Vec<f32> = rest[..end].iter().map(|s| s.unwrap()).collect();
        let start = rest.iter().position(|s| *s != 0.0).expect("audio again");
        assert!(rest[start] < value(stalled_at, 0) / 2.0, "fades in");
        let fade = source.fade_frames;
        let expected: Vec<f32> = (stalled_at + fade..6_000).map(|f| value(f, 0)).collect();
        assert_eq!(
            rest[start + fade..],
            expected,
            "nothing lost, nothing doubled"
        );
    }

    #[test]
    fn a_source_ends_only_once_its_decoder_is_done_and_its_ring_drained() {
        // Longer than a ring, so the decoder is still at it while it plays,
        // and slow at the very end.
        let Test {
            mut source, script, ..
        } = feed(2, 30_000, 29_000);
        let mut heard = 0;
        let mut quiet = 0;
        let mut released = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        while let Some(sample) = pull(&mut source, 1)[0] {
            assert!(Instant::now() < deadline, "never ended ({heard} heard)");
            if sample != 0.0 {
                heard += 1;
                quiet = 0;
                continue;
            }
            // Silent for a while near the end: it waits for the last frames
            // rather than end. (Where exactly depends on how far the decoder
            // got ahead; the source waits for half a second of audio to come
            // back after running dry, and none comes before the release.)
            quiet += 1;
            if quiet > 8_000 && heard > 50_000 && !released {
                script.allow(30_000);
                released = true;
            }
        }
        assert!(released, "it ended before its last frames came");
        assert_eq!(heard, 60_000, "every sample, exactly once");
        assert_eq!(source.current_span_len(), Some(0));
        assert_eq!(source.next(), None);
    }

    #[test]
    fn a_seek_empties_the_ring_and_plays_from_the_target() {
        let Test {
            mut source, handle, ..
        } = feed(2, 40_000, 40_000);
        eventually("a full ring", || source.ring.slots() > 8_000);
        // Mid-frame, as rodio seeks: an odd number of samples in.
        let before = pull(&mut source, 1_001);
        assert_eq!(before[1_000], Some(value(500, 0)));
        source.try_seek(Duration::from_secs(3)).expect("seeks");
        handle.wake();
        // The consumer must not catch up with the decoder below.
        eventually("the target decoded", || source.ring.slots() >= 16_000);
        let after = pull(&mut source, 12_000);
        // The rest of the frame, then silence, then the target — left first.
        let start = after.iter().position(|s| *s != Some(0.0)).expect("audio");
        assert_eq!((1_001 + start) % 2, 0, "starts on a frame");
        assert_eq!(after[start], Some(value(24_000, 0)));
        assert_eq!(after[start + 1], Some(value(24_000, 1)));
        let heard = frames_heard(&after[start..], 2);
        assert_eq!(heard, (24_000..24_000 + heard.len()).collect::<Vec<_>>());
        assert!(heard.len() > 5_000, "plays on from there");
    }

    #[test]
    fn silence_is_taken_off_the_position_rodio_counts() {
        let Test {
            source,
            handle,
            script,
            ..
        } = feed(2, 40_000, 16 * CHUNK_FRAMES);
        eventually("the decoder at the stall", || {
            source.ring.slots() >= 2 * 16 * CHUNK_FRAMES
        });
        let mut source = source.track_position();
        // 4 096 frames, then a stall.
        let mut pulled = 0;
        while handle.silence() < Duration::from_millis(300) {
            assert!(pulled < 1_000_000, "it never ran dry");
            source.next().unwrap();
            pulled += 1;
            if pulled % 512 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let media = source.get_pos().saturating_sub(handle.silence());
        assert_eq!(media.as_millis(), 512, "4 096 frames at 8 kHz: {media:?}");
        // On again, and a seek: rodio counts from the target, silence anew.
        script.allow(40_000);
        source.try_seek(Duration::from_secs(4)).unwrap();
        handle.wake();
        let mut audio = 0usize;
        let deadline = Instant::now() + Duration::from_secs(10);
        while audio < 2 * 8_000 {
            assert!(Instant::now() < deadline, "the target's audio never came");
            if source.next().unwrap() != 0.0 {
                audio += 1;
            } else {
                std::thread::yield_now();
            }
        }
        let media = source.get_pos().saturating_sub(handle.silence());
        let media = media.as_secs_f64();
        assert!((media - 5.0).abs() < 0.001, "the target and 1 s: {media}");
    }

    #[test]
    fn the_thread_ends_with_its_source() {
        // Dropped with the decoder idle (a full ring).
        let Test { source, script, .. } = feed(2, 100_000, 100_000);
        eventually("a full ring", || source.ring.slots() > 8_000);
        drop(source);
        eventually("the decoder dropped", || {
            script.dropped.load(Ordering::SeqCst)
        });
        // Its download let go while the decoder waits for data.
        let Test {
            source,
            script,
            stop,
            handle,
        } = feed(2, 100_000, 2 * CHUNK_FRAMES + 10);
        stop.cancel();
        eventually("the decoder dropped", || {
            script.dropped.load(Ordering::SeqCst)
        });
        assert!(
            handle.reposition(Duration::ZERO, out_rate()).is_none(),
            "nothing to reposition"
        );
        drop(source);
    }

    #[test]
    fn a_crashing_decoder_ends_its_source_and_says_so() {
        let (mut fake, _) = fake(1, 10_000, 10_000);
        fake.panic_at = Some(3_000);
        let Test {
            mut source, handle, ..
        } = start_fake(fake, true);
        eventually("the crash", || handle.failed());
        let out = pull(&mut source, 100_000);
        let end = out.iter().position(Option::is_none).expect("it ended");
        // Up to the chunk it crashed in.
        let heard = frames_heard(&out[..end], 1);
        assert!(heard.len() + CHUNK_FRAMES >= 3_000, "only {}", heard.len());
        assert_eq!(heard, (0..heard.len()).collect::<Vec<_>>());
        assert!(handle.failed());
    }

    #[test]
    fn a_seek_the_decoder_cannot_do_is_reported() {
        let (fake, _) = fake(1, 10_000, 10_000);
        fake.seek_fails.store(true, Ordering::SeqCst);
        let seek_fails = fake.seek_fails.clone();
        let Test {
            mut source, handle, ..
        } = start_fake(fake, true);
        source.try_seek(Duration::from_millis(500)).unwrap();
        handle.wake();
        eventually("the failure reported", || {
            source.next();
            handle.take_seek_failure() == Some(Duration::from_millis(500))
        });
        assert_eq!(handle.take_seek_failure(), None, "once");
        // Silent, not over: the player reopens the track there.
        assert!(pull(&mut source, 5_000).iter().all(|s| *s == Some(0.0)));
        assert!(handle.reposition(Duration::ZERO, out_rate()).is_none());
        // A failure the source has moved on from does not count: the seek
        // after it is what plays.
        source.try_seek(Duration::from_millis(250)).unwrap();
        handle.wake();
        eventually("the second failure", || {
            source.next();
            source.view.seek_failed.load(Ordering::Acquire) != NO_FAILURE
        });
        seek_fails.store(false, Ordering::SeqCst);
        source.try_seek(Duration::from_millis(750)).unwrap();
        handle.wake();
        assert_eq!(handle.take_seek_failure(), None, "superseded");
        let out = pull(&mut source, 20_000);
        assert_eq!(frames_heard(&out, 1).first(), Some(&6_000));
        assert!(
            handle.reposition(Duration::ZERO, out_rate()).is_some(),
            "seeks again"
        );
    }

    #[test]
    fn a_wait_cut_short_for_nothing_goes_on_where_it_was() {
        // A source preempted between storing a seek and announcing it can
        // cut short a wait after the seek is done. The decoder gives up on
        // the packet it was reading, and is put back where it stood.
        let Test {
            mut source,
            handle,
            script,
            ..
        } = feed(1, 10_000, 3 * CHUNK_FRAMES + 100);
        eventually("the decoder waiting", || {
            source.ring.slots() >= 3 * CHUNK_FRAMES
        });
        handle.shared.interrupt.raise();
        handle.wake();
        std::thread::sleep(Duration::from_millis(100));
        script.allow(10_000);
        eventually("the rest decoded", || {
            source.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let out = pull(&mut source, 30_000);
        let end = out.iter().position(Option::is_none).expect("it ended");
        let heard = frames_heard(&out[..end], 1);
        assert_eq!(
            heard,
            (0..10_000).collect::<Vec<_>>(),
            "nothing lost or doubled"
        );
    }

    /// The same with a resampler in between: the decoder is put back where
    /// the ring stands (what the resampler held is decoded again), and the
    /// track goes on without a frame lost or doubled.
    #[test]
    fn a_resampled_wait_cut_short_for_nothing_goes_on_where_it_was() {
        use super::super::resample::converted_len;
        let (mut fake, script) = fake(1, 10_000, 3_000);
        fake.rate = 44_100;
        let Test {
            mut source, handle, ..
        } = start_fake_into(fake, true, SampleRate::new(48_000).unwrap());
        eventually("the decoder waiting", || source.ring.slots() >= 2_000);
        handle.shared.interrupt.raise();
        handle.wake();
        std::thread::sleep(Duration::from_millis(100));
        script.allow(10_000);
        eventually("the rest decoded", || {
            source.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let out = pull(&mut source, 30_000);
        let end = out.iter().position(Option::is_none).expect("it ended");
        let out: Vec<f32> = out[..end].iter().map(|s| s.unwrap()).collect();
        let expected = converted_len(10_000, 44_100, 48_000) as usize;
        assert!(
            out.len().abs_diff(expected) <= 2,
            "{} of {expected}",
            out.len()
        );
        // A ramp of one per input frame: 0.92 per output frame, throughout
        // (held at both ends, which the check leaves out).
        let step = 44_100.0 / 48_000.0;
        for (i, pair) in out[1_500..out.len() - 1_500].windows(2).enumerate() {
            let rise = pair[1] - pair[0];
            assert!((rise - step).abs() < 0.2, "{rise} at {}", i + 1_500);
        }
    }

    #[test]
    fn a_seek_cuts_short_a_read_that_waits_for_the_download() {
        let Test {
            mut source, handle, ..
        } = feed(1, 10_000, 3_000);
        // The decoder waits at frame 3 000; the target is long there.
        eventually("the decoder waiting", || source.ring.slots() >= 2_000);
        source.try_seek(Duration::from_millis(100)).unwrap();
        handle.wake();
        // Up to the download again, and waiting there.
        eventually("the target decoded", || source.ring.slots() >= 2_000);
        std::thread::sleep(Duration::from_millis(200));
        let out = pull(&mut source, 20_000);
        let heard = frames_heard(&out, 1);
        assert_eq!(heard.first(), Some(&800), "from the target");
        assert_eq!(heard, (800..800 + heard.len()).collect::<Vec<_>>());
        assert!(
            800 + heard.len() + CHUNK_FRAMES >= 3_000,
            "up to the download"
        );
    }

    #[test]
    fn a_live_stream_is_not_positioned() {
        let Test {
            mut source, handle, ..
        } = start_fake(fake(1, 10_000, 10_000).0, false);
        assert!(source.try_seek(Duration::from_secs(1)).is_err());
        assert!(handle
            .reposition(Duration::from_secs(1), out_rate())
            .is_none());
        assert_eq!(pull(&mut source, 1)[0], Some(value(0, 0)));
    }

    #[test]
    fn a_source_moved_on_continues_from_its_position_on_the_same_decoder() {
        let Test {
            mut source,
            handle,
            script,
            ..
        } = feed(2, 40_000, 40_000);
        eventually("a full ring", || source.ring.slots() > 8_000);
        let played = pull(&mut source, 2_000);
        assert_eq!(frames_heard(&played, 2), (0..1_000).collect::<Vec<_>>());
        let mut moved = handle
            .reposition(Duration::from_millis(125), out_rate())
            .expect("repositioned");
        eventually("the new ring filled", || moved.ring.slots() >= 12_000);
        let out = pull(&mut moved, 10_000);
        let heard = frames_heard(&out, 2);
        assert_eq!(heard.first(), Some(&1_000));
        assert_eq!(heard, (1_000..1_000 + heard.len()).collect::<Vec<_>>());
        // The old source plays what its ring held and ends.
        let rest = pull(&mut source, 100_000);
        assert!(rest.iter().any(Option::is_none), "the old source ended");
        assert!(
            !script.dropped.load(Ordering::SeqCst),
            "the decoder lives on"
        );
        assert_eq!(handle.silence(), {
            // The new source's silence is what counts now.
            let samples = moved.silence;
            Duration::from_secs_f64(samples as f64 / (2.0 * f64::from(RATE)))
        });
    }

    /// The gapless hand-off: the next track is appended to the same player
    /// well before the boundary, its ring full by then.
    #[test]
    fn gapless_sources_follow_each_other_sample_exactly() {
        let a = feed(2, 5_000, 5_000);
        let b = feed(2, 3_000, 3_000);
        for test in [&a, &b] {
            eventually("the whole track decoded", || {
                test.source.view.end_at.load(Ordering::Acquire) != NO_END
            });
        }
        let (player, mut output) = rodio::Player::new();
        player.append(a.source);
        player.append(b.source);
        let mut out = Vec::new();
        while out.len() < 16_000 {
            out.push(output.next().unwrap());
            if out.len() == 10_000 + 2 {
                // rodio's count drops at the boundary: what the player
                // detects the hand-off by.
                assert_eq!(player.len(), 1);
            }
        }
        let expected: Vec<f32> = (0..5_000)
            .chain(0..3_000)
            .flat_map(|frame| [value(frame, 0), value(frame, 1)])
            .collect();
        assert_eq!(out, expected, "back to back, not a sample more or less");
    }

    /// What a fake decoder decodes of `frames`, as a buffer.
    fn fake_audio(channels: u16, rate: u32, frames: std::ops::Range<usize>) -> SamplesBuffer {
        let samples: Vec<f32> = frames
            .flat_map(|frame| (0..usize::from(channels)).map(move |c| value(frame, c)))
            .collect();
        SamplesBuffer::new(
            ChannelCount::new(channels).unwrap(),
            SampleRate::new(rate).unwrap(),
            samples,
        )
    }

    /// A fake track of `frames` frames at `rate`, decoded into a ring at
    /// `to`, whole.
    fn decoded_into(channels: u16, rate: u32, frames: usize, to: u32) -> Test {
        let (mut fake, _) = fake(channels, frames, frames);
        fake.rate = rate;
        let test = start_fake_into(fake, true, SampleRate::new(to).unwrap());
        eventually("the whole track decoded", || {
            test.source.view.end_at.load(Ordering::Acquire) != NO_END
        });
        test
    }

    /// The gapless hand-off of two tracks resampled to the output's rate:
    /// each exactly as long as it is at that rate, back to back.
    #[test]
    fn gapless_sources_follow_each_other_sample_exactly_at_any_rate() {
        use super::super::resample::converted_len;
        use super::super::resample::tests::convert_all;
        for (channels, rate) in [(2, 44_100), (2, 48_000), (2, 96_000), (1, 22_050)] {
            let a = decoded_into(channels, rate, 5_000, 48_000);
            let b = decoded_into(channels, rate, 3_000, 48_000);
            assert_eq!(a.source.sample_rate().get(), 48_000);
            assert_eq!(a.source.channels().get(), channels);
            let first = convert_all(fake_audio(channels, rate, 0..5_000), 48_000);
            let second = convert_all(fake_audio(channels, rate, 0..3_000), 48_000);
            let ch = usize::from(channels);
            assert_eq!(
                first.len() / ch,
                converted_len(5_000, rate, 48_000) as usize
            );
            assert_eq!(
                second.len() / ch,
                converted_len(3_000, rate, 48_000) as usize
            );
            if rate == 48_000 {
                // Untouched.
                assert_eq!(
                    first,
                    fake_audio(channels, rate, 0..5_000).collect::<Vec<_>>()
                );
            }
            let expected = [first.clone(), second].concat();
            let (player, mut output) = rodio::Player::new();
            player.append(a.source);
            player.append(b.source);
            let mut out = Vec::new();
            while out.len() < expected.len() {
                out.push(output.next().unwrap());
                if out.len() == first.len() + ch {
                    // rodio's count drops at the boundary: what the player
                    // detects the hand-off by.
                    assert_eq!(player.len(), 1, "{rate} Hz");
                }
            }
            assert_eq!(out, expected, "{rate} Hz: back to back");
            // Then nothing more of either.
            assert!(
                output.by_ref().take(100).all(|s| s == 0.0),
                "{rate} Hz: not a sample more"
            );
        }
    }

    /// A decoder claiming a rate no music has (a broken WAV header): its
    /// source ends at once, without a sample — nothing is built for it, and
    /// nothing aborts.
    #[test]
    fn a_source_at_a_rate_no_music_has_ends_at_once() {
        for hz in [1, 0xDEAD_BEEF] {
            let (mut fake, _) = fake(2, 10_000, 10_000);
            fake.rate = hz;
            let Test { mut source, .. } =
                start_fake_into(fake, true, SampleRate::new(48_000).unwrap());
            let out = pull(&mut source, 1_000);
            assert!(out.iter().all(Option::is_none), "{hz} Hz played");
        }
    }

    /// A decoder of `parts`, one after the other, as a chained stream; a part
    /// it is through with stays where it is.
    struct Parts {
        parts: Vec<SamplesBuffer>,
        at: usize,
    }

    impl Iterator for Parts {
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

    impl Source for Parts {
        fn current_span_len(&self) -> Option<usize> {
            Some(self.parts.get(self.at).map_or(0, |part| part.size_hint().0))
        }

        fn channels(&self) -> ChannelCount {
            self.parts[self.at.min(self.parts.len() - 1)].channels()
        }

        fn sample_rate(&self) -> SampleRate {
            self.parts[self.at.min(self.parts.len() - 1)].sample_rate()
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    /// A stream that comes to a rate no music has mid-way (a broken chain):
    /// what came before plays, then the source ends — failed, not at its
    /// end, so the player says it cannot play the track and goes on.
    #[test]
    fn a_rate_no_music_has_mid_stream_ends_the_source_as_failed() {
        use super::super::resample::tests::convert_all;
        let first = fake_audio(2, 44_100, 0..8_820);
        let broken = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(1).unwrap(),
            vec![0.25; 2_000],
        );
        let parts = Parts {
            parts: vec![first.clone(), broken],
            at: 0,
        };
        let (mut source, handle) = start(
            parts,
            Duration::ZERO,
            true,
            CancellationToken::new(),
            Arc::default(),
            SampleRate::new(48_000).unwrap(),
        )
        .expect("started");
        eventually("the decoder at the broken part", || handle.failed());
        let out = pull(&mut source, 40_000);
        let end = out.iter().position(Option::is_none).expect("it ended");
        let heard: Vec<f32> = out[..end].iter().map(|s| s.unwrap()).collect();
        assert_eq!(heard, convert_all(first, 48_000), "what came before, whole");
        assert_eq!(
            handle.failure().as_deref(),
            Some("cannot decode the stream: a sample rate of 1 Hz")
        );
        assert!(handle
            .reposition(Duration::ZERO, SampleRate::new(48_000).unwrap())
            .is_none());
        // A crash says so in its own words.
        let (mut crashing, _) = fake(1, 10_000, 10_000);
        // Past the first chunk, which the opener's thread decodes.
        crashing.panic_at = Some(3_000);
        // Its source kept: without one the thread ends before the crash.
        let Test {
            source: _playing,
            handle,
            ..
        } = start_fake(crashing, true);
        eventually("the crash", || handle.failed());
        assert_eq!(handle.failure().as_deref(), Some("the decoder crashed"));
    }

    /// The whole way out: a mono track resampled to the output's rate and a
    /// stereo one at it, gapless on one player, through the player's way into
    /// the mixer (`uniform.rs`) and the mixer, into a stereo mix: back to
    /// back, each in its own format, bit for bit what the stage made.
    #[test]
    fn gapless_tracks_of_other_formats_reach_the_mix_back_to_back() {
        use super::super::resample::tests::convert_all;
        let a = decoded_into(1, 44_100, 5_000, 48_000);
        let b = decoded_into(2, 48_000, 3_000, 48_000);
        let stereo = ChannelCount::new(2).unwrap();
        let rate = SampleRate::new(48_000).unwrap();
        let (mixer, mut mixed) = rodio::mixer::mixer(stereo, rate);
        let (player, output) = rodio::Player::new();
        player.append(a.source);
        player.append(b.source);
        mixer.add(crate::player::uniform::wrap(output, stereo, rate));
        let first: Vec<f32> = convert_all(fake_audio(1, 44_100, 0..5_000), 48_000)
            .into_iter()
            .flat_map(|s| [s, s])
            .collect();
        let second: Vec<f32> = fake_audio(2, 48_000, 0..3_000).collect();
        let expected = [first, second].concat();
        let out: Vec<f32> = mixed.by_ref().take(expected.len()).collect();
        assert_eq!(out, expected);
        assert!(mixed.take(100).all(|s| s == 0.0), "not a sample more");
    }

    /// The position of a resampled source after a seek: rodio's count, less
    /// the silence while the target was on its way, is the target and what
    /// played since — and what plays is the track from the target.
    #[test]
    fn a_seek_in_a_resampled_source_lands_exactly() {
        use super::super::resample::tests::convert_all;
        let (mut fake, _) = fake(2, 88_200, 88_200);
        fake.rate = 44_100;
        let Test { source, handle, .. } =
            start_fake_into(fake, true, SampleRate::new(48_000).unwrap());
        eventually("a full ring", || source.ring.slots() > 48_000);
        let mut source = source.track_position();
        for _ in 0..10_001 {
            source.next().unwrap();
        }
        source.try_seek(Duration::from_secs(1)).unwrap();
        handle.wake();
        let expected = convert_all(fake_audio(2, 44_100, 44_100..88_200), 48_000);
        let mut audio = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while audio.len() < 24_000 {
            assert!(Instant::now() < deadline, "the target's audio never came");
            let sample = source.next().unwrap();
            if sample != 0.0 || !audio.is_empty() {
                audio.push(sample);
            } else {
                std::thread::yield_now();
            }
        }
        assert_eq!(audio, expected[..24_000], "the track from the target");
        let media = source.get_pos().saturating_sub(handle.silence());
        let expected_media = 1.0 + 12_000.0 / 48_000.0;
        assert!(
            (media.as_secs_f64() - expected_media).abs() < 0.001,
            "{media:?}"
        );
    }

    /// Moved to an output at another rate: the new ring is at that rate,
    /// from the position, on the same decoder; the old one plays out at its
    /// own.
    #[test]
    fn a_source_moved_to_an_output_at_another_rate_is_resampled_for_it() {
        use super::super::resample::tests::convert_all;
        let (mut fake, _) = fake(2, 60_000, 60_000);
        fake.rate = 44_100;
        let Test {
            mut source,
            handle,
            script,
            ..
        } = start_fake_into(fake, true, SampleRate::new(48_000).unwrap());
        eventually("a full ring", || source.ring.slots() > 40_000);
        assert_eq!(source.sample_rate().get(), 48_000);
        pull(&mut source, 2_000);
        let mut moved = handle
            .reposition(Duration::from_millis(500), SampleRate::new(96_000).unwrap())
            .expect("repositioned");
        assert_eq!(moved.sample_rate().get(), 96_000);
        assert_eq!(moved.channels().get(), 2);
        // More than is pulled below, which goes faster than it plays.
        eventually("the new ring filled", || moved.ring.slots() >= 64_000);
        let out = pull(&mut moved, 60_000);
        let start = out.iter().position(|s| *s != Some(0.0)).expect("audio");
        let heard: Vec<f32> = out[start..].iter().map(|s| s.unwrap()).collect();
        let expected = convert_all(fake_audio(2, 44_100, 22_050..60_000), 96_000);
        assert_eq!(
            heard,
            expected[..heard.len()],
            "from the position, at 96 kHz"
        );
        assert!(heard.len() > 30_000);
        // Its silence counts at its own rate: the wait for the target.
        let waited = Duration::from_secs_f64(start as f64 / (2.0 * 96_000.0));
        assert_eq!(handle.silence(), waited);
        // The old source plays what its ring held and ends.
        let rest = pull(&mut source, 200_000);
        assert!(rest.iter().any(Option::is_none), "the old source ended");
        assert_eq!(source.sample_rate().get(), 48_000);
        assert!(
            !script.dropped.load(Ordering::SeqCst),
            "the decoder lives on"
        );
    }

    /// Seeks its source on the next sample once asked to, from the thread
    /// that pulls it — as rodio's player does with a `try_seek`, but at a
    /// moment the test decides.
    struct SeekWhenAsked<S> {
        inner: S,
        ask: Arc<Mutex<Option<Duration>>>,
    }

    impl<S: Source> Iterator for SeekWhenAsked<S> {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            if let Some(pos) = lock(&self.ask).take() {
                self.inner.try_seek(pos).expect("seeks");
            }
            self.inner.next()
        }
    }

    impl<S: Source> Source for SeekWhenAsked<S> {
        fn current_span_len(&self) -> Option<usize> {
            self.inner.current_span_len()
        }

        fn channels(&self) -> ChannelCount {
            self.inner.channels()
        }

        fn sample_rate(&self) -> SampleRate {
            self.inner.sample_rate()
        }

        fn total_duration(&self) -> Option<Duration> {
            self.inner.total_duration()
        }
    }

    /// rodio's mixer converts a player's audio span by span, taking the span
    /// length the source gave when the span began. A seek cuts a span short;
    /// were the source to end before the rest of it, the next track of the
    /// player would go through this one's converter.
    #[test]
    fn a_seek_near_the_end_leaves_the_next_track_its_own_format() {
        use rodio::buffer::SamplesBuffer;
        // A: a second of mono at 8 kHz, all of it decoded. B: stereo at twice
        // the rate, left and right apart.
        let a = feed(1, 8_000, 8_000);
        eventually("A decoded", || {
            a.source.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let stereo = ChannelCount::new(2).unwrap();
        let rate = SampleRate::new(16_000).unwrap();
        let b = SamplesBuffer::new(stereo, rate, [0.5, -0.5].repeat(4_000));
        let (mixer, mut mixed) = rodio::mixer::mixer(stereo, rate);
        let (player, output) = rodio::Player::new();
        let ask = Arc::new(Mutex::new(None));
        player.append(SeekWhenAsked {
            inner: a.source,
            ask: ask.clone(),
        });
        player.append(b);
        mixer.add(output);
        // Into a long span of A: a thousand of its samples, four output
        // samples each.
        let mut out: Vec<f32> = mixed.by_ref().take(4_000).collect();
        *lock(&ask) = Some(Duration::from_millis(990));
        out.extend(mixed.by_ref().take(4));
        assert!(lock(&ask).is_none(), "seeked");
        a.handle.wake();
        eventually("the seek carried out, to the end", || {
            let view = lock(&a.handle.shared.view).clone();
            view.applied.load(Ordering::Acquire) == 1
                && view.end_at.load(Ordering::Acquire) != NO_END
        });
        // The rest of A, the silence it owes, then B.
        let b_starts = loop {
            let sample = mixed.next().unwrap();
            out.push(sample);
            if sample.abs() == 0.5 {
                break out.len() - 1;
            }
            assert!(out.len() < 200_000, "B never started");
        };
        out.extend(mixed.by_ref().take(7_999));
        let expected = [0.5, -0.5].repeat(4_000);
        assert_eq!(&out[b_starts..], expected.as_slice(), "B in its own format");
        assert!(
            out[4_000..b_starts].iter().any(|s| *s >= 7_921.0),
            "A played from the target"
        );
    }

    fn rate(hz: u32) -> SampleRate {
        SampleRate::new(hz).unwrap()
    }

    /// How far the ring offered to `handle`'s source for a coupled start has
    /// been filled, and whether its end is in.
    fn offered(handle: &FeedHandle) -> (u64, bool) {
        lock(&handle.shared.offered_view)
            .as_ref()
            .map_or((0, false), |view| {
                (
                    view.pushed.load(Ordering::Acquire),
                    view.end_at.load(Ordering::Acquire) != NO_END,
                )
            })
    }

    /// `track` started at the top into a ring at 48 kHz, with its start
    /// kept for the track before to play out into.
    fn started(track: SamplesBuffer) -> (RingSource, FeedHandle) {
        let (source, handle) = start(
            track,
            Duration::ZERO,
            true,
            CancellationToken::new(),
            Arc::default(),
            rate(48_000),
        )
        .expect("started");
        eventually("its start kept", || handle.shared.head.get().is_some());
        (source, handle)
    }

    /// `total` samples of `source`, each chunk once its thread has put it
    /// in and a frame more (the span after it is decided on the last
    /// sample): as fast as the thread decodes, never faster. The tracks it
    /// is used for hold back more than that at their end.
    fn drain(source: &mut RingSource, total: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(total);
        while out.len() < total {
            let chunk = (total - out.len()).min(9_600);
            eventually("decoded on", || {
                source.ring.slots() > chunk + source.channels
            });
            out.extend((0..chunk).map(|_| source.next().unwrap()));
        }
        assert_eq!(source.stalls, 0, "it never ran dry");
        out
    }

    /// The measurement that found the ringing at gapless boundaries, the
    /// whole way out: one tone over four tracks — 44.1 kHz stereo twice, 48
    /// kHz stereo, 44.1 kHz mono — each on a decoder thread of its own,
    /// coupled as the player couples them when it appends one to the other,
    /// on one player into a stereo mix at 48 kHz.
    #[test]
    fn coupled_gapless_tracks_reach_the_mix_as_one_tone() {
        use super::super::resample::tests::{convert_all, residuals, tone_part};
        let parts = [
            (2, 44_100, 0.0, 44_100),
            (2, 44_100, 1.0, 44_100),
            (2, 48_000, 2.0, 48_000),
            (1, 44_100, 3.0, 44_100),
        ];
        let part = |i: usize| {
            let (channels, hz, at, frames) = parts[i];
            tone_part(channels, hz, at, frames)
        };
        let out = rate(48_000);
        let mut sources = Vec::new();
        let mut handles: Vec<FeedHandle> = Vec::new();
        for (i, (_, hz, ..)) in parts.iter().enumerate() {
            let (source, handle) = started(part(i));
            // Appended to the one before: converted anew, starting on its
            // end — but at the output's rate, as it is.
            let coupled = handles
                .last()
                .is_some_and(|before| before.couple(&handle, out));
            assert_eq!(coupled, i > 0 && *hz != 48_000, "track {i}");
            sources.push(source);
            handles.push(handle);
        }
        // Each decoded whole before it plays; the last one holds the end of
        // its resampler back for a next track that does not come.
        for i in [0, 2] {
            eventually("decoded", || {
                sources[i].view.end_at.load(Ordering::Acquire) != NO_END
            });
        }
        eventually("converted anew", || offered(&handles[1]).1);
        eventually("the last one converted anew", || {
            offered(&handles[3]).0 >= 44_000
        });
        let stereo = ChannelCount::new(2).unwrap();
        let (mixer, mut mixed) = rodio::mixer::mixer(stereo, out);
        let (player, output) = rodio::Player::new();
        for source in sources {
            player.append(source);
        }
        mixer.add(crate::player::uniform::wrap(output, stereo, out));
        let frames = 4 * 48_000 - 4_000;
        let left: Vec<f32> = mixed.by_ref().take(frames * 2).step_by(2).collect();
        let boundaries = [48_000, 96_000, 144_000];
        let (after, away) = residuals(&left, &boundaries, 2_000);
        // As each came out on its own.
        let apart: Vec<f32> = (0..parts.len())
            .flat_map(|i| {
                let channels = usize::from(parts[i].0);
                convert_all(part(i), 48_000)
                    .into_iter()
                    .step_by(channels)
                    .collect::<Vec<_>>()
            })
            .take(frames)
            .collect();
        let (before, _) = residuals(&apart, &boundaries, 2_000);
        eprintln!(
            "boundary residual through the mix, dBFS: before {before:.1?}, \
             coupled {after:.1?} (away {away:.1})"
        );
        assert!(before.iter().all(|db| *db > -60.0), "{before:?}");
        assert!(after.iter().all(|db| *db < -100.0), "{after:?}");
        assert!(away < -100.0, "{away}");
    }

    /// The track before got to its end before the next one was coupled to
    /// it (a short track, a slow open): the next one starts on that end all
    /// the same; only the end of the one before stays as it was.
    #[test]
    fn a_track_coupled_late_still_starts_on_the_end_of_the_one_before() {
        use super::super::resample::tests::{convert_all, convert_coupled, tone_part};
        let first = || tone_part(2, 44_100, 0.0, 20_122);
        let second = || tone_part(2, 44_100, 20_122.0 / 44_100.0, 30_000);
        let (mut a, a_handle) = started(first());
        // Shorter than what is held back for a next track: it ends at once.
        eventually("the first one at its end", || {
            a.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let (mut b, b_handle) = started(second());
        assert!(a_handle.couple(&b_handle, rate(48_000)), "converted anew");
        let expected = convert_coupled(vec![first(), second()], 48_000);
        // B holds the end of its resampler back: all but that.
        let ready = expected[1].len() - 4_000 * 2;
        eventually("B converted anew", || offered(&b_handle).0 >= ready as u64);
        let heard = drain(&mut b, ready);
        assert_eq!(heard, expected[1][..ready], "on A's end, on A's grid");
        // A ended as it would have on its own.
        let a_out: Vec<f32> = a.by_ref().collect();
        assert_eq!(a_out, convert_all(first(), 48_000));
    }

    /// A track whose decoder never gets to its end (here it waits for its
    /// download, and is let go): the next one, coupled to it, starts on its
    /// own once its thread is gone.
    #[test]
    fn a_coupled_track_starts_on_its_own_when_the_one_before_goes_without_an_end() {
        use super::super::resample::tests::{convert_all, tone_part};
        let out = rate(48_000);
        let (mut waiting, _) = fake(2, 40_000, 10_000);
        waiting.rate = 44_100;
        let a = start_fake_into(waiting, true, out);
        let second = || tone_part(2, 44_100, 0.0, 30_000);
        let (mut b, b_handle) = started(second());
        assert!(a.handle.couple(&b_handle, out), "converted anew");
        // Waiting for A's end.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(offered(&b_handle).0, 0);
        a.stop.cancel();
        drop(a.source);
        a.handle.wake();
        let expected = convert_all(second(), 48_000);
        let ready = expected.len() - 4_000 * 2;
        eventually("B converted anew", || offered(&b_handle).0 >= ready as u64);
        assert_eq!(drain(&mut b, ready), expected[..ready], "as on its own");
        // Coupled to one whose thread is gone already (it crashed): the same.
        let (mut crashing, _) = fake(2, 40_000, 40_000);
        crashing.rate = 44_100;
        crashing.panic_at = Some(3_000);
        let a = start_fake_into(crashing, true, out);
        eventually("the crash", || !a.handle.running());
        let (mut b, b_handle) = started(second());
        assert!(a.handle.couple(&b_handle, out), "converted anew");
        eventually("B converted anew", || offered(&b_handle).0 >= ready as u64);
        assert_eq!(drain(&mut b, ready), expected[..ready], "as on its own");
    }

    /// The next track has to play before the one before has got to its end
    /// (here: before its decoder does at all): it plays the ring it had, and
    /// its thread fills that on as if nothing had been coupled — past what
    /// the ring held, across the whole track.
    #[test]
    fn a_coupled_track_needed_before_its_start_is_decided_plays_as_it_was() {
        use super::super::resample::tests::{convert_all, tone_part};
        let out = rate(48_000);
        let (mut waiting, script) = fake(2, 40_000, 10_000);
        waiting.rate = 44_100;
        let a = start_fake_into(waiting, true, out);
        let second = || tone_part(2, 44_100, 0.0, 2 * 44_100 + 12_345);
        let (mut b, b_handle) = started(second());
        eventually("B's ring full", || b.ring.slots() > 60_000 * 2);
        assert!(a.handle.couple(&b_handle, out), "converted anew");
        let expected = convert_all(second(), 48_000);
        let total = expected.len() - 4_000 * 2;
        let mut heard = drain(&mut b, 20_000);
        // A's end comes after all, and changes nothing.
        script.allow(40_000);
        heard.extend(drain(&mut b, total - heard.len()));
        assert_eq!(heard, expected[..total], "as on its own");
        let chosen = lock(&b_handle.shared.offered_view)
            .as_ref()
            .unwrap()
            .chosen
            .load(Ordering::Acquire);
        assert_eq!(chosen, CHOSEN_BEFORE, "the ring it had");
        drop(a);
    }

    /// The coupled start was converted, but not far enough when the next
    /// track had to play (here: made to look so): it plays the ring it had
    /// and its thread, which had started the track anew, converts it once
    /// more on its own, and fills that ring on exactly where it stopped.
    #[test]
    fn a_coupled_start_not_ready_in_time_falls_back_without_a_seam() {
        use super::super::resample::tests::{convert_all, tone_part};
        let first = || tone_part(2, 44_100, 0.0, 20_122);
        let second = || tone_part(2, 44_100, 20_122.0 / 44_100.0, 2 * 44_100 + 777);
        let (a, a_handle) = started(first());
        eventually("the first one at its end", || {
            a.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let (mut b, b_handle) = started(second());
        eventually("B's ring full", || b.ring.slots() > 60_000 * 2);
        assert!(a_handle.couple(&b_handle, rate(48_000)), "converted anew");
        eventually("B converted anew", || offered(&b_handle).0 > 40_000);
        b.resume_after_seek = usize::MAX;
        let expected = convert_all(second(), 48_000);
        let total = expected.len() - 4_000 * 2;
        let heard = drain(&mut b, total);
        assert_eq!(heard, expected[..total], "as on its own, to the sample");
        let chosen = lock(&b_handle.shared.offered_view)
            .as_ref()
            .unwrap()
            .chosen
            .load(Ordering::Acquire);
        assert_eq!(chosen, CHOSEN_BEFORE, "the ring it had");
        drop(a);
    }

    /// In the order the player goes: the next track is coupled and appended
    /// to a running player three seconds ahead, while the playing track's
    /// decoder is still at it — a second and a half ahead of the speakers,
    /// or, with a download that barely keeps up, only just ahead. Played in
    /// real time, the two come out back to back, with no sample missing or
    /// added: the next one coupled, or — when its coupled start was not in
    /// time — as it would have been on its own.
    #[test]
    fn coupled_tracks_follow_each_other_in_real_time_however_late_the_end_comes() {
        use super::super::resample::tests::{convert_all, convert_coupled, tone_part};
        let out = rate(48_000);
        let frames = 3 * 44_100 + 22_050;
        for lead in [1.5, 0.2] {
            let (mut fake_a, script) = fake(2, frames, 0);
            fake_a.rate = 44_100;
            let ahead = (lead * 44_100.0) as usize;
            script.allow(ahead);
            let a = start_fake_into(fake_a, true, out);
            let second = || tone_part(2, 44_100, 0.25, 20_000);
            let (b, b_handle) = started(second());
            let (player, mut output) = rodio::Player::new();
            player.append(a.source);
            let mut heard: Vec<f32> = Vec::new();
            let mut coupled = false;
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut b = Some(b);
            while heard.len() < 2 * (48_000 * 7 / 2 + 21_000) {
                assert!(Instant::now() < deadline, "{lead} s: too slow");
                heard.extend(output.by_ref().take(960));
                let played = heard.len() / 2 * 44_100 / 48_000;
                script.allow((played + ahead).min(frames));
                // Three seconds before the boundary, as the player does.
                if !coupled && played + 3 * 44_100 >= frames {
                    coupled = true;
                    assert!(a.handle.couple(&b_handle, out), "{lead} s");
                    player.append(b.take().unwrap());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let a_audio = || fake_audio(2, 44_100, 0..frames);
            let expected = convert_coupled(vec![a_audio(), second()], 48_000);
            let a_len = expected[0].len();
            assert_eq!(heard[..a_len], expected[0][..], "{lead} s: A");
            let b_heard = &heard[a_len..a_len + 2 * 15_000];
            let as_coupled = b_heard == &expected[1][..2 * 15_000];
            let on_its_own = b_heard == &convert_all(second(), 48_000)[..2 * 15_000];
            eprintln!("{lead} s ahead: the next track coupled: {as_coupled}");
            assert!(as_coupled || on_its_own, "{lead} s: B back to back");
            if lead > 1.0 {
                assert!(as_coupled, "with time to spare it is coupled");
            }
        }
    }

    /// A next track coupled — its coupled start converted and offered — and
    /// then moved before it ever played: to another output, or parked with a
    /// closed one and resumed (a ring nothing reads, then another). Its
    /// source there plays from where it was put, at its own ring's rate, to
    /// the end; the offer went with the ring it was for.
    #[test]
    fn a_coupled_track_moved_before_it_played_plays_from_where_it_was_put() {
        use super::super::resample::tests::tone_part;
        let first = || tone_part(2, 44_100, 0.0, 20_122);
        let second = || tone_part(2, 44_100, 0.5, 3 * 44_100);
        let (a, a_handle) = started(first());
        eventually("the first one at its end", || {
            a.view.end_at.load(Ordering::Acquire) != NO_END
        });
        // What the second one is from 1 s on, on its own.
        let rest = || tone_part(2, 44_100, 1.5, 2 * 44_100);
        for park in [false, true] {
            let (b, b_handle) = started(second());
            assert!(a_handle.couple(&b_handle, rate(48_000)), "converted anew");
            eventually("B converted anew", || offered(&b_handle).0 >= 20_000);
            // Moved while its source is still there, as the player does
            // (`restart_at`, `refit`): a thread whose source is gone with no
            // new ring waiting may have ended already.
            let parked = park.then(|| {
                b_handle
                    .reposition(Duration::from_secs(1), rate(44_100))
                    .expect("parked")
            });
            let mut moved = b_handle
                .reposition(Duration::from_secs(1), rate(48_000))
                .expect("moved");
            drop(b);
            drop(parked);
            assert!(b_handle.shared.offer.load(Ordering::SeqCst).is_null());
            assert_eq!(moved.sample_rate().get(), 48_000);
            let expected = convert_all_from(rest(), 48_000);
            let total = expected.len() - 4_000 * 2;
            let heard = drain(&mut moved, total);
            assert_eq!(heard, expected[..total], "parked: {park}");
            let chosen = lock(&b_handle.shared.offered_view)
                .as_ref()
                .unwrap()
                .chosen
                .load(Ordering::Acquire);
            assert_eq!(chosen, CHOSEN_OPEN, "nothing took the offer");
        }
        drop(a);
    }

    fn convert_all_from(track: SamplesBuffer, to: u32) -> Vec<f32> {
        super::super::resample::tests::convert_all(track, to)
    }

    /// The source is pulled for the first time just as its coupled start is
    /// offered — after the offer, before the thread gets the ring: it takes
    /// the offer, finds it not ready and plays the ring it had, and the
    /// thread, handed the coupled ring all the same, goes back to that one.
    #[test]
    fn an_offer_taken_while_it_is_made_leaves_the_track_as_it_was() {
        use super::super::resample::tests::{convert_all, tone_part};
        let first = || tone_part(2, 44_100, 0.0, 20_122);
        let second = || tone_part(2, 44_100, 0.5, 2 * 44_100 + 4_321);
        let (a, a_handle) = started(first());
        eventually("the first one at its end", || {
            a.view.end_at.load(Ordering::Acquire) != NO_END
        });
        let (mut b, b_handle) = started(second());
        eventually("B's ring full", || b.ring.slots() > 60_000 * 2);
        let mut first_sample = None;
        let renewed = a_handle.couple_then(&b_handle, rate(48_000), || {
            first_sample = b.next();
        });
        assert!(renewed, "handed over: the source took the offer");
        let expected = convert_all(second(), 48_000);
        let total = expected.len() - 4_000 * 2;
        let mut heard = vec![first_sample.expect("a first sample")];
        heard.extend(drain(&mut b, total - 1));
        assert_eq!(heard, expected[..total], "as on its own");
        let chosen = lock(&b_handle.shared.offered_view)
            .as_ref()
            .unwrap()
            .chosen
            .load(Ordering::Acquire);
        assert_eq!(chosen, CHOSEN_BEFORE, "the ring it had");
        drop(a);
    }

    /// The next track coupled to this one is let go of before this one's end
    /// (a skip, a queue edit, a crossfade): this one ends on its own, with
    /// its last frame held, not into a start that never follows.
    #[test]
    fn a_track_whose_next_one_was_let_go_of_ends_on_its_own() {
        use super::super::resample::tests::{convert_all, tone_part};
        let out = rate(48_000);
        let frames = 40_000;
        let (mut fake_a, script) = fake(2, frames, 10_000);
        fake_a.rate = 44_100;
        let Test {
            mut source, handle, ..
        } = start_fake_into(fake_a, true, out);
        let (b, b_handle) = started(tone_part(2, 44_100, 0.0, 30_000));
        assert!(handle.couple(&b_handle, out));
        drop(b);
        eventually("B's thread gone", || !b_handle.running());
        script.allow(frames);
        // About as fast as it plays: it holds its end back for a next track
        // until its ring runs low.
        let mut heard = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        'play: loop {
            for _ in 0..960 {
                match source.next() {
                    Some(sample) => heard.push(sample),
                    None => break 'play,
                }
            }
            assert!(Instant::now() < deadline, "never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        let expected = convert_all(fake_audio(2, 44_100, 0..frames), 48_000);
        // Its end held, not B's start.
        assert_eq!(source.stalls, 0, "it never ran dry");
        assert_eq!(heard, expected);
    }

    /// A resampled track that nothing is coupled to after it holds the end
    /// of its resampler back only as long as its ring has more than enough
    /// to play: played in real time, it comes out whole, without a gap.
    #[test]
    fn a_track_nothing_follows_still_plays_out_its_end_in_time() {
        use super::super::resample::tests::{convert_all, tone_part};
        let track = || tone_part(2, 44_100, 0.0, 40_000);
        let (mut source, _handle) = start(
            track(),
            Duration::ZERO,
            true,
            CancellationToken::new(),
            Arc::default(),
            rate(48_000),
        )
        .unwrap();
        eventually("decoded but for the end", || source.ring.slots() > 80_000);
        assert_eq!(source.view.end_at.load(Ordering::Acquire), NO_END);
        // 10 ms at a time, about as fast as it plays.
        let mut heard = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        'play: loop {
            for _ in 0..960 {
                match source.next() {
                    Some(sample) => heard.push(sample),
                    None => break 'play,
                }
            }
            assert!(Instant::now() < deadline, "never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(source.stalls, 0, "it never ran dry");
        assert_eq!(heard, convert_all(track(), 48_000));
    }

    #[test]
    fn a_request_the_thread_has_seen_to_cuts_nothing_short() {
        let interrupt = Interrupt::default();
        assert!(!interrupt.pending());
        // Stored and announced: pending until the thread looks.
        interrupt.raise();
        assert!(interrupt.pending());
        interrupt.settle();
        assert!(!interrupt.pending());
        // Announced while the thread carries out what it took: it stays
        // pending for the next look, and nothing lingers after that.
        interrupt.raise();
        interrupt.raise();
        assert!(interrupt.pending());
        interrupt.settle();
        assert!(!interrupt.pending());
    }

    #[test]
    fn seek_requests_keep_their_number_and_target() {
        let word = pack(5, Duration::from_micros(123_456_789));
        assert_eq!(unpack(word), (5, Duration::from_micros(123_456_789)));
        let (epoch, _) = unpack(pack(EPOCH_MASK + 3, Duration::ZERO));
        assert_eq!(epoch, 2, "the number wraps");
    }
}
