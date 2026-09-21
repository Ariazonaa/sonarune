//! PCM tap for the visualizer: a Source wrapper sitting after the DSP chain
//! that streams whatever is audible, in stereo, to the WebView over a Tauri
//! IPC channel. The audible path is untouched; when no visualizer is
//! subscribed the tap is a relaxed atomic load and a counter increment per
//! sample (the counter keeps left and right apart when a feed starts
//! mid-stream, see `TapSource::sample_index`).
//!
//! While a visualizer listens, the audio callback thread still never
//! allocates, frees, waits on a lock or wakes another thread: a finished
//! chunk is copied into a buffer the first subscriber pre-allocated
//! (`ChunkPool`), and a forwarder thread polls the pool, encodes and sends.

use rodio::Source;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::Duration;

/// Stereo frames per IPC frame (~43 ms at 48 kHz, 16 KiB of samples).
const CHUNK_FRAMES: usize = 2048;

/// Interleaved samples per chunk (L and R of every frame).
const CHUNK_SAMPLES: usize = CHUNK_FRAMES * 2;

/// Channels on the wire. Butterchurn splits its input into a left and a right
/// analyser (MilkDrop draws the waveforms per side), so the feed carries the
/// real stereo picture instead of a mono downmix on both sides.
const WIRE_CHANNELS: u32 = 2;

/// Sample rate and channel count in front of the samples.
const HEADER_BYTES: usize = 8;

/// Chunks that can wait for the forwarder before new ones are dropped: at
/// least ~85 ms of audio (192 kHz), ~350 ms at 48 kHz.
const POOL_SLOTS: usize = 8;

/// How often the forwarder looks for finished chunks while someone listens.
/// The audio thread never wakes it (that would take a lock or a syscall on
/// the callback thread), so this is the added delivery latency — small next
/// to a chunk's ≥ 10 ms and the ~100 ms the frontend buffers.
const FORWARD_POLL: Duration = Duration::from_millis(5);

/// Safety-net wake-up while nobody listens; `subscribe` and dropping the tap
/// unpark the forwarder right away.
const IDLE_PARK: Duration = Duration::from_secs(1);

/// Wire format per frame: u32-le sample rate, u32-le channel count (always
/// [`WIRE_CHANNELS`]), then f32-le samples interleaved L, R, L, R, … The
/// frontend resamples to its AudioContext rate if they differ
/// (`src/lib/visualizer/pcm.ts`). Encodes into `bytes`, replacing its
/// contents, so the forwarder reuses one buffer for every frame.
fn encode_frame_into(bytes: &mut Vec<u8>, sample_rate: u32, interleaved: &[f32]) {
    bytes.clear();
    bytes.reserve(HEADER_BYTES + interleaved.len() * 4);
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&WIRE_CHANNELS.to_le_bytes());
    for sample in interleaved {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
}

/// Folds interleaved samples of any channel count into stereo frames: mono
/// goes to both sides, stereo passes through, and every channel past the
/// first two (center, LFE, surrounds) is mixed into both sides at half
/// weight, normalized so a full-scale frame stays full scale. A visualizer
/// needs the left/right picture, not a calibrated downmix. Runs per sample on
/// the audio thread: no allocation, no division until a frame is complete.
#[derive(Debug, Clone, Copy)]
struct StereoFold {
    channels: u16,
    pos: u16,
    left: f32,
    right: f32,
    extra: f32,
}

impl StereoFold {
    fn new(channels: u16) -> Self {
        Self {
            channels: channels.max(1),
            pos: 0,
            left: 0.0,
            right: 0.0,
            extra: 0.0,
        }
    }

    fn reset(&mut self) {
        self.pos = 0;
        self.left = 0.0;
        self.right = 0.0;
        self.extra = 0.0;
    }

    fn is_mid_frame(&self) -> bool {
        self.pos != 0
    }

    /// Feed one sample; returns `(left, right)` once all channels of the
    /// current frame are in.
    #[inline]
    fn push(&mut self, sample: f32) -> Option<(f32, f32)> {
        match self.pos {
            0 => self.left = sample,
            1 => self.right = sample,
            _ => self.extra += sample,
        }
        self.pos += 1;
        if self.pos < self.channels {
            return None;
        }
        let frame = match self.channels {
            1 => (self.left, self.left),
            2 => (self.left, self.right),
            n => {
                let extra = 0.5 * self.extra;
                let gain = 1.0 / (1.0 + 0.5 * f32::from(n - 2));
                ((self.left + extra) * gain, (self.right + extra) * gain)
            }
        };
        self.reset();
        Some(frame)
    }
}

/// [`Slot::state`]: nobody owns the slot, a writer may claim it.
const SLOT_FREE: u64 = 0;
/// [`Slot::state`]: one thread owns the slot (filling or reading it).
/// Every other value is a ready chunk's sequence number.
const SLOT_CLAIMED: u64 = u64::MAX;

struct PooledChunk {
    sample_rate: u32,
    /// Allocated once with [`CHUNK_SAMPLES`] capacity, never grown or freed.
    samples: Vec<f32>,
}

struct Slot {
    /// [`SLOT_FREE`], [`SLOT_CLAIMED`] or the sequence number of the ready
    /// chunk in it. Ownership moves only by compare-and-swap.
    state: AtomicU64,
    /// Touched only by the thread that moved `state` to [`SLOT_CLAIMED`], so
    /// the mutex is never held when someone tries it; see [`try_lock_slot`].
    chunk: Mutex<PooledChunk>,
}

/// Fixed set of pre-allocated chunk buffers between the audio callback thread
/// (fills them, [`ChunkPool::push`]) and the forwarder (empties them,
/// [`ChunkPool::pop_oldest`]). Nothing here allocates, frees, waits or
/// wakes on the audio side:
///
/// * claiming a buffer is one compare-and-swap on its slot state; a slot
///   that is taken or ready is skipped, and with no free slot the chunk is
///   dropped on the spot;
/// * the copy fits the buffer's capacity, so the `Vec` never reallocates,
///   and buffers stay in the pool for the life of the tap;
/// * the slot mutex is a safe cell, not a lock anyone waits on: only the
///   claimant touches it and only via `try_lock` — a single uncontended
///   compare-and-swap. Because no thread ever blocks in `lock()`, the mutex
///   never has a waiter, so its unlock never makes a wake-up call either;
/// * the forwarder is not notified. `std::sync::mpsc::SyncSender::try_send`,
///   which this replaces, locks the channel's waker mutex to unpark a blocked
///   receiver, and `Thread::unpark` is a syscall; the forwarder polls
///   instead ([`FORWARD_POLL`]).
///
/// Chunks carry a sequence number, so the forwarder sends them in the order
/// they were finished whichever slot they landed in. Several writers (two
/// sources during a device switch) are safe; they only share the slots.
struct ChunkPool {
    slots: [Slot; POOL_SLOTS],
    /// Next sequence number; starts at 1 so it never reads as [`SLOT_FREE`].
    next_seq: AtomicU64,
}

/// See [`Slot::chunk`]. Returns `None` rather than waiting if the buffer is
/// held after all; the caller then leaves this slot alone.
fn try_lock_slot(chunk: &Mutex<PooledChunk>) -> Option<MutexGuard<'_, PooledChunk>> {
    match chunk.try_lock() {
        Ok(guard) => Some(guard),
        // Nothing panics while holding a slot; if something did, the buffer
        // is still a valid (cleared on next use) Vec.
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl ChunkPool {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot {
                state: AtomicU64::new(SLOT_FREE),
                chunk: Mutex::new(PooledChunk {
                    sample_rate: 0,
                    samples: Vec::with_capacity(CHUNK_SAMPLES),
                }),
            }),
            next_seq: AtomicU64::new(1),
        }
    }

    /// Audio thread: copy one finished chunk into a free buffer. Returns
    /// `false` — the chunk is dropped — when every slot is waiting for or held
    /// by the forwarder, or the chunk would not fit a buffer.
    fn push(&self, sample_rate: u32, samples: &[f32]) -> bool {
        if samples.len() > CHUNK_SAMPLES {
            return false;
        }
        for slot in &self.slots {
            if slot
                .state
                .compare_exchange(SLOT_FREE, SLOT_CLAIMED, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            let Some(mut chunk) = try_lock_slot(&slot.chunk) else {
                slot.state.store(SLOT_FREE, Ordering::Release);
                continue;
            };
            chunk.sample_rate = sample_rate;
            chunk.samples.clear();
            chunk.samples.extend_from_slice(samples);
            drop(chunk);
            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            slot.state.store(seq, Ordering::Release);
            return true;
        }
        false
    }

    /// Forwarder: hand the oldest ready chunk to `read` and return its buffer
    /// to the pool. `false` when no chunk is ready.
    fn pop_oldest(&self, read: impl FnOnce(u32, &[f32])) -> bool {
        loop {
            let oldest = self
                .slots
                .iter()
                .map(|slot| (slot.state.load(Ordering::Acquire), slot))
                .filter(|(state, _)| *state != SLOT_FREE && *state != SLOT_CLAIMED)
                .min_by_key(|(state, _)| *state);
            let Some((seq, slot)) = oldest else {
                return false;
            };
            if slot
                .state
                .compare_exchange(seq, SLOT_CLAIMED, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // Taken by another reader in the meantime: look again.
                continue;
            }
            let Some(mut chunk) = try_lock_slot(&slot.chunk) else {
                slot.state.store(SLOT_FREE, Ordering::Release);
                continue;
            };
            read(chunk.sample_rate, &chunk.samples);
            chunk.samples.clear();
            drop(chunk);
            slot.state.store(SLOT_FREE, Ordering::Release);
            return true;
        }
    }

    /// Forwarder: encode every ready chunk, oldest first, into `bytes` and
    /// pass it to `send`. A chunk's buffer is back in the pool before `send`
    /// runs, so a slow IPC send never holds one the audio thread could use.
    /// Returns the number of frames sent.
    fn drain_encoded(&self, bytes: &mut Vec<u8>, mut send: impl FnMut(&[u8])) -> usize {
        let mut sent = 0;
        while self.pop_oldest(|sample_rate, samples| encode_frame_into(bytes, sample_rate, samples))
        {
            send(bytes.as_slice());
            sent += 1;
        }
        sent
    }
}

struct TapShared {
    active: AtomicBool,
    /// The forwarder is gone (tap dropped, or its thread died): nothing would
    /// empty the pool, so `subscribe` no longer switches the tap on.
    closed: AtomicBool,
    /// Chunk buffers, allocated by the first `subscribe` — on a command
    /// thread, never on the audio thread — and kept for the life of the tap.
    pool: OnceLock<ChunkPool>,
    /// Multiple consumers (e.g. the full visualizer and a player-bar spectrum)
    /// can subscribe at once, keyed by the channel's unique id. Keying by id
    /// makes teardown races safe: a stale engine only ever removes its own id.
    /// Value is (owning window label, channel). The label lets us drop a
    /// subscription when its window is destroyed: closing a window tears the
    /// WebView down without running the frontend's unsubscribe, and the
    /// channel's `send` does NOT report the dead consumer back to us — the
    /// subscription would otherwise live, and keep the audio tap running, for
    /// the rest of the process.
    channels: Mutex<HashMap<u32, (String, tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>)>>,
}

impl TapShared {
    fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            pool: OnceLock::new(),
            channels: Mutex::new(HashMap::new()),
        }
    }

    /// Forwarder thread: send one encoded frame to every consumer.
    fn fan_out(&self, frame: &[u8]) {
        let mut guard = self.channels.lock().unwrap();
        if guard.is_empty() {
            return;
        }
        // One owned copy per consumer (the IPC body takes a Vec; ~16 KiB, a
        // handful of times) — on this thread, not the audio thread.
        guard.retain(|_, (_, channel)| {
            channel
                .send(tauri::ipc::InvokeResponseBody::Raw(frame.to_vec()))
                .is_ok()
        });
        // No consumers left: stop the audio thread producing frames.
        if guard.is_empty() {
            self.active.store(false, Ordering::Release);
        }
    }
}

/// Marks the tap closed when the forwarder thread ends, even by a panic.
struct ForwarderExit(Arc<TapShared>);

impl Drop for ForwarderExit {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.active.store(false, Ordering::Release);
    }
}

/// The forwarder thread: empties the pool every [`FORWARD_POLL`] while
/// someone listens, parks while nobody does. Chunks still waiting when the
/// last consumer leaves are thrown away with the next pass.
fn forward(shared: Arc<TapShared>) {
    let _exit = ForwarderExit(shared.clone());
    let mut bytes = Vec::with_capacity(HEADER_BYTES + CHUNK_SAMPLES * 4);
    while !shared.closed.load(Ordering::Acquire) {
        if let Some(pool) = shared.pool.get() {
            pool.drain_encoded(&mut bytes, |frame| shared.fan_out(frame));
        }
        if shared.active.load(Ordering::Acquire) {
            std::thread::sleep(FORWARD_POLL);
        } else {
            std::thread::park_timeout(IDLE_PARK);
        }
    }
}

/// The TapSource runs on the audio callback thread; encoding and IPC sends
/// must never happen there. Finished chunks go into a fixed pool of reused
/// buffers (`ChunkPool`) that a forwarder thread empties (chunks are
/// dropped, not blocked on, when the forwarder falls behind).
pub struct VisualizerTap {
    shared: Arc<TapShared>,
    /// The forwarder thread, unparked when a consumer arrives and when the
    /// tap goes away. `None` for test taps whose pool the test empties.
    forwarder: Option<std::thread::Thread>,
}

impl VisualizerTap {
    pub fn new() -> Self {
        let shared = Arc::new(TapShared::new());
        let forwarder_shared = shared.clone();
        let handle = std::thread::Builder::new()
            .name("jellysic-vis-forward".into())
            .spawn(move || forward(forwarder_shared))
            .expect("failed to spawn visualizer forwarder thread");
        Self {
            shared,
            forwarder: Some(handle.thread().clone()),
        }
    }

    pub fn subscribe(
        &self,
        window_label: String,
        channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
    ) {
        self.shared.pool.get_or_init(ChunkPool::new);
        self.shared
            .channels
            .lock()
            .unwrap()
            .insert(channel.id(), (window_label, channel));
        if !self.shared.closed.load(Ordering::Acquire) {
            self.shared.active.store(true, Ordering::Release);
        }
        self.wake_forwarder();
    }

    /// Drop every subscription belonging to a window. Called when that window
    /// is destroyed — see the comment on `TapShared::channels`.
    /// Whether any window is currently receiving PCM. The visualizer and the
    /// spectrum bar are the only consumers, and neither survives having its
    /// WebView suspended: the WebGL context and the AudioWorklet would be torn
    /// down under them.
    pub fn has_subscribers_in(&self, window_label: &str) -> bool {
        self.shared
            .channels
            .lock()
            .map(|c| c.values().any(|(label, _)| label == window_label))
            .unwrap_or(false)
    }

    pub fn unsubscribe_window(&self, window_label: &str) {
        let mut guard = self.shared.channels.lock().unwrap();
        guard.retain(|_, (label, _)| label != window_label);
        if guard.is_empty() {
            self.shared.active.store(false, Ordering::Release);
        }
    }

    /// Remove one consumer by channel id. Keying by id makes teardown races
    /// safe: a stale engine only removes its own channel, never a newer one.
    pub fn unsubscribe(&self, channel_id: u32) {
        let mut guard = self.shared.channels.lock().unwrap();
        guard.remove(&channel_id);
        if guard.is_empty() {
            self.shared.active.store(false, Ordering::Release);
        }
    }

    #[inline]
    fn is_active(&self) -> bool {
        self.shared.active.load(Ordering::Relaxed)
    }

    fn wake_forwarder(&self) {
        if let Some(forwarder) = &self.forwarder {
            forwarder.unpark();
        }
    }

    /// Audio thread: hand one finished chunk to the forwarder, or drop it
    /// when the pool is full. Never allocates, frees or blocks (see
    /// [`ChunkPool`]).
    #[inline]
    fn send(&self, sample_rate: u32, interleaved: &[f32]) {
        if let Some(pool) = self.shared.pool.get() {
            pool.push(sample_rate, interleaved);
        }
    }
}

impl Default for VisualizerTap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VisualizerTap {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.active.store(false, Ordering::Release);
        self.wake_forwarder();
    }
}

/// Source adapter feeding the tap. Folds interleaved frames to stereo on the
/// fly; chunk state resets on seek and on format changes. The per-source
/// `enabled` flag lets the player mute a source's feed during a crossfade —
/// otherwise two overlapping tracks would interleave in the visualizer.
pub struct TapSource<S> {
    inner: S,
    tap: std::sync::Arc<VisualizerTap>,
    enabled: std::sync::Arc<AtomicBool>,
    /// Interleaved stereo samples of the frame being filled. Allocated in
    /// `wrap` (off the audio thread) with room for exactly one chunk.
    chunk: Vec<f32>,
    fold: StereoFold,
    channels: u16,
    sample_rate: u32,
    /// Samples produced since the start (seeks included), counted even
    /// while nobody listens. A feed that starts mid-stream — a visualizer
    /// opened during a track, a source re-enabled — must begin on a frame
    /// boundary: starting on a right-channel sample would swap left and
    /// right for the rest of the track.
    sample_index: u64,
}

pub fn wrap<S>(
    source: S,
    tap: std::sync::Arc<VisualizerTap>,
) -> (TapSource<S>, std::sync::Arc<AtomicBool>)
where
    S: Source,
{
    let channels = source.channels().get();
    let sample_rate = source.sample_rate().get();
    let enabled = std::sync::Arc::new(AtomicBool::new(true));
    (
        TapSource {
            inner: source,
            tap,
            enabled: enabled.clone(),
            chunk: Vec::with_capacity(CHUNK_SAMPLES),
            fold: StereoFold::new(channels),
            channels,
            sample_rate,
            sample_index: 0,
        },
        enabled,
    )
}

impl<S: Source> TapSource<S> {
    /// `index` is the sample's position in the stream (see `sample_index`).
    #[inline]
    fn observe(&mut self, sample: f32, index: u64) {
        if !self.fold.is_mid_frame() {
            // Format can change between spans; cheap to re-check per frame start.
            let channels = self.inner.channels().get();
            let sample_rate = self.inner.sample_rate().get();
            if channels != self.channels || sample_rate != self.sample_rate {
                self.channels = channels;
                self.sample_rate = sample_rate;
                self.fold = StereoFold::new(channels);
                self.chunk.clear();
            }
            // Not a frame start: this is a later channel of a frame whose
            // beginning was not observed. Wait for the next frame.
            if !index.is_multiple_of(u64::from(self.channels.max(1))) {
                return;
            }
        }
        if let Some((left, right)) = self.fold.push(sample) {
            self.chunk.push(left);
            self.chunk.push(right);
            if self.chunk.len() >= CHUNK_SAMPLES {
                self.tap.send(self.sample_rate, &self.chunk);
                self.chunk.clear();
            }
        }
    }

    fn reset_feed(&mut self) {
        self.chunk.clear();
        self.fold.reset();
    }
}

impl<S: Source> Iterator for TapSource<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;
        let index = self.sample_index;
        self.sample_index = index.wrapping_add(1);
        if self.tap.is_active() && self.enabled.load(Ordering::Relaxed) {
            self.observe(sample, index);
        } else if !self.chunk.is_empty() || self.fold.is_mid_frame() {
            // Also drop a half-folded frame so a re-enable starts fresh.
            self.reset_feed();
        }
        Some(sample)
    }
}

impl<S: Source> Source for TapSource<S> {
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
        self.reset_feed();
        // Keep counting. rodio polls for a seek every N samples, not frames
        // (441 for 44.1 kHz stereo), and the decoder resumes on the channel it
        // stopped at: the running index stays aligned, while a reset to 0
        // would swap left and right after every other seek.
        self.inner.try_seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        encode_frame_into, wrap, ChunkPool, StereoFold, TapShared, VisualizerTap, CHUNK_FRAMES,
        CHUNK_SAMPLES, HEADER_BYTES, POOL_SLOTS, SLOT_CLAIMED, SLOT_FREE,
    };
    use rodio::buffer::SamplesBuffer;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    /// Counts heap operations of the current thread while it runs inside
    /// `count_heap_ops`; everything else passes straight to `System`.
    struct CountingAllocator;

    thread_local! {
        static HEAP_OPS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    fn note_heap_op() {
        // `try_with`: allocations also happen while thread-locals are torn down.
        let _ = HEAP_OPS.try_with(|ops| {
            if let Some(count) = ops.get() {
                ops.set(Some(count + 1));
            }
        });
    }

    // SAFETY: every call is forwarded unchanged to `System`; the bookkeeping
    // neither allocates nor touches the memory.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note_heap_op();
            // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            note_heap_op();
            // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            note_heap_op();
            // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note_heap_op();
            // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    /// Runs `f`; returns how many allocations, reallocations and frees this
    /// thread made meanwhile.
    fn count_heap_ops(f: impl FnOnce()) -> usize {
        HEAP_OPS.with(|ops| ops.set(Some(0)));
        f();
        HEAP_OPS.with(Cell::take).unwrap_or(0)
    }

    /// The encoder as it was before the buffer pool (issue #11), verbatim:
    /// the wire format must not change by a single byte
    /// (`src/lib/visualizer/pcm.ts`).
    fn reference_encode(sample_rate: u32, interleaved: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + interleaved.len() * 4);
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        for s in interleaved {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        bytes
    }

    /// A tap that is "subscribed" and whose pool the test empties instead of
    /// a forwarder thread and a WebView.
    fn test_tap() -> Arc<VisualizerTap> {
        let shared = Arc::new(TapShared::new());
        shared.pool.get_or_init(ChunkPool::new);
        shared.active.store(true, Ordering::Relaxed);
        Arc::new(VisualizerTap {
            shared,
            forwarder: None,
        })
    }

    /// What the forwarder would send right now: every ready chunk, encoded,
    /// oldest first.
    fn drain(tap: &VisualizerTap) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut bytes = Vec::new();
        tap.shared
            .pool
            .get()
            .expect("test taps have a pool")
            .drain_encoded(&mut bytes, |frame| frames.push(frame.to_vec()));
        frames
    }

    fn decode(frame: &[u8]) -> (u32, u32, Vec<f32>) {
        let word = |at: usize| u32::from_le_bytes(frame[at..at + 4].try_into().unwrap());
        let samples = frame[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        (word(0), word(4), samples)
    }

    fn buffer_at(channels: u16, sample_rate: u32, samples: Vec<f32>) -> SamplesBuffer {
        SamplesBuffer::new(
            rodio::ChannelCount::new(channels).unwrap(),
            rodio::SampleRate::new(sample_rate).unwrap(),
            samples,
        )
    }

    fn buffer(channels: u16, samples: Vec<f32>) -> SamplesBuffer {
        buffer_at(channels, 48_000, samples)
    }

    /// Deterministic samples in [-1, 1) that differ everywhere, so a shifted,
    /// swapped or reordered sample shows up.
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    /// Where each pool buffer lives, to prove they are reused, not replaced.
    fn pool_buffers(pool: &ChunkPool) -> Vec<(*const f32, usize)> {
        pool.slots
            .iter()
            .map(|slot| {
                let chunk = slot.chunk.lock().unwrap();
                (chunk.samples.as_ptr(), chunk.samples.capacity())
            })
            .collect()
    }

    #[test]
    fn mono_goes_to_both_sides_and_stereo_passes_through() {
        let mut mono = StereoFold::new(1);
        assert_eq!(mono.push(0.5), Some((0.5, 0.5)));

        let mut stereo = StereoFold::new(2);
        assert_eq!(stereo.push(0.25), None);
        assert_eq!(stereo.push(-0.75), Some((0.25, -0.75)));
        assert!(!stereo.is_mid_frame());
    }

    #[test]
    fn surround_channels_fold_into_both_sides_without_clipping() {
        // 5.1: FL FR C LFE SL SR — all at full scale stays at full scale.
        let mut fold = StereoFold::new(6);
        let frame = [1.0f32; 6].map(|s| fold.push(s));
        assert_eq!(frame[..5], [None; 5]);
        assert_eq!(frame[5], Some((1.0, 1.0)));
        // Only the center speaks: it lands equally on both sides.
        let mut fold = StereoFold::new(6);
        let (l, r) = [0.0, 0.0, 0.9, 0.0, 0.0, 0.0]
            .map(|s| fold.push(s))
            .into_iter()
            .flatten()
            .next()
            .unwrap();
        assert_eq!(l, r);
        assert!(l > 0.0);
    }

    #[test]
    fn frames_carry_rate_channel_count_and_interleaved_samples() {
        // The reused buffer still holds a longer, older frame.
        let mut bytes = vec![0xAA; 64];
        encode_frame_into(&mut bytes, 44_100, &[0.5, -0.5, 1.0, 0.0]);
        assert_eq!(decode(&bytes), (44_100, 2, vec![0.5, -0.5, 1.0, 0.0]));
        assert_eq!(bytes, reference_encode(44_100, &[0.5, -0.5, 1.0, 0.0]));
    }

    #[test]
    fn the_tap_ships_real_stereo_and_leaves_the_audio_untouched() {
        let tap = test_tap();
        // Left and right differ in every frame: L = +i, R = -i (scaled).
        let samples: Vec<f32> = (0..CHUNK_FRAMES)
            .flat_map(|i| {
                let v = i as f32 / CHUNK_FRAMES as f32;
                [v, -v]
            })
            .collect();
        let (source, _enabled) = wrap(buffer(2, samples.clone()), tap.clone());
        let audible: Vec<f32> = source.collect();
        assert_eq!(audible, samples, "the tap must not change what is heard");

        let frames = drain(&tap);
        assert_eq!(frames.len(), 1, "exactly one frame");
        let (rate, channels, shipped) = decode(&frames[0]);
        assert_eq!((rate, channels), (48_000, 2));
        assert_eq!(shipped, samples);
    }

    #[test]
    fn a_feed_starting_mid_frame_waits_for_the_next_frame_and_keeps_left_left() {
        let tap = test_tap();
        // Left is always +0.1, right always -0.1: a swap is visible.
        let samples: Vec<f32> = (0..CHUNK_FRAMES * 3).flat_map(|_| [0.1, -0.1]).collect();
        let (mut source, enabled) = wrap(buffer(2, samples), tap.clone());
        // One left sample observed, then the feed goes off (a crossfade tail,
        // or the last visualizer closing) …
        source.next();
        enabled.store(false, Ordering::Relaxed);
        for _ in 0..CHUNK_FRAMES * 2 {
            source.next();
        }
        assert!(drain(&tap).is_empty(), "nothing while off");
        // … and on again with the stream sitting on a RIGHT sample (the same
        // happens when a visualizer opens in the middle of a track).
        enabled.store(true, Ordering::Relaxed);
        source.by_ref().for_each(drop);
        let frames = drain(&tap);
        let (_, _, shipped) = decode(frames.first().expect("a full frame after re-enable"));
        assert_eq!(shipped.len(), CHUNK_FRAMES * 2);
        for pair in shipped.as_chunks::<2>().0 {
            assert_eq!(*pair, [0.1, -0.1], "left and right must not swap");
        }
    }

    #[test]
    fn a_visualizer_opened_mid_track_starts_on_a_frame_boundary() {
        let tap = test_tap();
        tap.shared.active.store(false, Ordering::Relaxed);
        let samples: Vec<f32> = (0..CHUNK_FRAMES * 2 + 1)
            .flat_map(|_| [0.3, -0.3])
            .collect();
        let (mut source, _enabled) = wrap(buffer(2, samples), tap.clone());
        // Nobody listens for three samples, then a subscriber arrives.
        for _ in 0..3 {
            source.next();
        }
        tap.shared.active.store(true, Ordering::Relaxed);
        source.by_ref().for_each(drop);
        let frames = drain(&tap);
        let (_, _, shipped) = decode(frames.first().expect("a full frame"));
        assert!(shipped
            .as_chunks::<2>()
            .0
            .iter()
            .all(|pair| *pair == [0.3, -0.3]));
    }

    #[test]
    fn the_audio_path_neither_allocates_nor_frees_while_subscribed() {
        assert_eq!(
            count_heap_ops(|| drop(std::hint::black_box(vec![0u8; 16]))),
            2,
            "the counter sees an allocation and its free"
        );
        let tap = test_tap();
        let pool = tap.shared.pool.get().unwrap();
        let buffers = pool_buffers(pool);
        let overflow = POOL_SLOTS + 4;
        let steady = POOL_SLOTS * 3;
        let (mut source, _enabled) = wrap(
            buffer(2, noise(CHUNK_SAMPLES * (overflow + steady), 11)),
            tap.clone(),
        );
        let own_chunk = (source.chunk.as_ptr(), source.chunk.capacity());
        let mut bytes = Vec::with_capacity(HEADER_BYTES + CHUNK_SAMPLES * 4);
        let mut sent = 0;

        // The forwarder is stuck: the pool runs full and chunks are dropped.
        let ops = count_heap_ops(|| {
            for _ in 0..CHUNK_SAMPLES * overflow {
                source.next();
            }
        });
        assert_eq!(ops, 0, "filling and overflowing the pool touched the heap");
        let ops = count_heap_ops(|| sent += pool.drain_encoded(&mut bytes, |_| {}));
        assert_eq!(ops, 0, "draining touched the heap");
        assert_eq!(sent, POOL_SLOTS, "overflowing chunks are dropped");

        // Steady state: a chunk out, a chunk in, over and over.
        for round in 0..steady {
            let ops = count_heap_ops(|| {
                for _ in 0..CHUNK_SAMPLES {
                    source.next();
                }
            });
            assert_eq!(ops, 0, "the audio thread touched the heap in chunk {round}");
            let ops = count_heap_ops(|| sent += pool.drain_encoded(&mut bytes, |_| {}));
            assert_eq!(ops, 0, "draining chunk {round} touched the heap");
        }
        assert_eq!(sent, POOL_SLOTS + steady);
        assert_eq!(pool_buffers(pool), buffers, "the pool keeps its buffers");
        assert_eq!(
            (source.chunk.as_ptr(), source.chunk.capacity()),
            own_chunk,
            "the source keeps its chunk buffer"
        );
    }

    #[test]
    fn a_full_pool_drops_chunks_at_once_and_recovers() {
        let pool = ChunkPool::new();
        let chunk = |value: f32| vec![value; CHUNK_SAMPLES];
        let first_samples = |pool: &ChunkPool| {
            let mut firsts = Vec::new();
            while pool.pop_oldest(|_, samples| firsts.push(samples[0])) {}
            firsts
        };

        for i in 0..POOL_SLOTS {
            assert!(pool.push(48_000, &chunk(i as f32)));
        }
        assert!(
            !pool.push(48_000, &chunk(99.0)),
            "a full pool drops the chunk"
        );
        assert!(
            !pool.push(48_000, &[0.0; CHUNK_SAMPLES + 2]),
            "a chunk that would outgrow a buffer is dropped"
        );
        let expected: Vec<f32> = (0..POOL_SLOTS).map(|i| i as f32).collect();
        assert_eq!(
            first_samples(&pool),
            expected,
            "the dropped chunk is not shipped"
        );

        // A slot the forwarder is reading and a buffer somebody holds are
        // skipped, not waited on (`lock()` here would deadlock this thread).
        pool.slots[0].state.store(SLOT_CLAIMED, Ordering::Relaxed);
        let held = pool.slots[1].chunk.lock().unwrap();
        assert!(pool.push(44_100, &chunk(1.5)));
        assert_eq!(
            pool.slots[1].state.load(Ordering::Relaxed),
            SLOT_FREE,
            "a held buffer's slot is given back untouched"
        );
        // Every slot busy one way or another: dropped immediately.
        for slot in &pool.slots[3..] {
            slot.state.store(SLOT_CLAIMED, Ordering::Relaxed);
        }
        assert!(!pool.push(44_100, &chunk(2.5)));
        drop(held);
        for slot in pool
            .slots
            .iter()
            .filter(|slot| slot.state.load(Ordering::Relaxed) == SLOT_CLAIMED)
        {
            slot.state.store(SLOT_FREE, Ordering::Relaxed);
        }
        let mut rates = Vec::new();
        assert!(pool.pop_oldest(|rate, samples| rates.push((rate, samples.len(), samples[0]))));
        assert_eq!(rates, [(44_100, CHUNK_SAMPLES, 1.5)]);
        assert!(first_samples(&pool).is_empty());

        // And the pool takes a full load again.
        for i in 0..POOL_SLOTS {
            assert!(pool.push(48_000, &chunk(i as f32)));
        }
        assert_eq!(first_samples(&pool), expected);
    }

    #[test]
    fn chunks_leave_the_pool_oldest_first_even_when_slots_are_reused() {
        let pool = ChunkPool::new();
        for value in [1.0, 2.0, 3.0] {
            assert!(pool.push(48_000, &vec![value; CHUNK_SAMPLES]));
        }
        let mut order = Vec::new();
        // The forwarder takes the oldest (slot 0), and the audio thread
        // refills that lowest slot at once with a newer chunk.
        assert!(pool.pop_oldest(|_, samples| order.push(samples[0])));
        assert!(pool.push(48_000, &vec![4.0; CHUNK_SAMPLES]));
        while pool.pop_oldest(|_, samples| order.push(samples[0])) {}
        assert_eq!(order, [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn pooled_frames_are_byte_identical_to_the_reference_encoder() {
        for (channels, rate) in [(1u16, 22_050u32), (2, 44_100)] {
            let tap = test_tap();
            let width = usize::from(channels);
            let chunks = 3;
            // A few samples past the last full chunk: never shipped.
            let samples = noise(CHUNK_FRAMES * width * chunks + 5, u32::from(channels));
            let (mut source, _enabled) =
                wrap(buffer_at(channels, rate, samples.clone()), tap.clone());
            // Drain after the first chunk, then the rest in one go.
            for _ in 0..CHUNK_FRAMES * width {
                source.next();
            }
            let mut frames = drain(&tap);
            source.by_ref().for_each(drop);
            frames.extend(drain(&tap));

            let stereo: Vec<f32> = if channels == 1 {
                samples.iter().flat_map(|&s| [s, s]).collect()
            } else {
                samples
            };
            let expected: Vec<Vec<u8>> = stereo
                .as_chunks::<CHUNK_SAMPLES>()
                .0
                .iter()
                .map(|chunk| reference_encode(rate, chunk))
                .collect();
            assert_eq!(expected.len(), chunks);
            assert_eq!(frames.len(), chunks, "{channels} channel(s)");
            assert!(
                frames == expected,
                "{channels} channel(s): frames differ from the reference encoding"
            );
        }
    }

    #[test]
    fn has_subscribers_in_answers_per_window() {
        // The suspend-on-hide path asks this before freezing the main WebView:
        // freezing it under a running visualizer would tear down its WebGL
        // context and AudioWorklet.
        let tap = VisualizerTap::new();
        let channel = |_label: &str| tauri::ipc::Channel::new(|_| Ok(()));

        assert!(!tap.has_subscribers_in("main"));

        let projector = channel("projector");
        tap.subscribe("projector".into(), projector);
        assert!(
            !tap.has_subscribers_in("main"),
            "another window must not count"
        );
        assert!(tap.has_subscribers_in("projector"));

        let main = channel("main");
        let main_id = main.id();
        tap.subscribe("main".into(), main);
        assert!(tap.has_subscribers_in("main"));

        tap.unsubscribe(main_id);
        assert!(!tap.has_subscribers_in("main"));
        assert!(
            tap.has_subscribers_in("projector"),
            "the other window stays"
        );
    }

    #[test]
    fn the_forwarder_sends_each_frame_to_every_subscriber_until_they_leave() {
        let tap = Arc::new(VisualizerTap::new());
        assert!(!tap.is_active());
        let subscribe = |label: &str| {
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let channel = tauri::ipc::Channel::new(move |body| {
                if let tauri::ipc::InvokeResponseBody::Raw(bytes) = body {
                    let _ = tx.send(bytes);
                }
                Ok(())
            });
            let id = channel.id();
            tap.subscribe(label.to_string(), channel);
            (id, rx)
        };
        let (main_id, main_rx) = subscribe("main");
        let (_, projector_rx) = subscribe("projector");
        assert!(tap.is_active());

        let samples = noise(CHUNK_SAMPLES * 2 + 3, 5);
        let (source, _enabled) = wrap(buffer(2, samples.clone()), tap.clone());
        source.for_each(drop);
        for rx in [&main_rx, &projector_rx] {
            for expected in samples.as_chunks::<CHUNK_SAMPLES>().0 {
                let frame = rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the forwarder delivers every frame");
                assert!(frame == reference_encode(48_000, expected));
            }
            assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        }

        tap.unsubscribe_window("projector");
        assert!(tap.is_active(), "the main window still listens");
        tap.unsubscribe(main_id);
        assert!(!tap.is_active());
    }
}
