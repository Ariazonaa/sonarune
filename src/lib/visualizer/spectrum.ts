// Analysis behind the now-playing spectrum bar (SpectrumBar.svelte): a
// log-spaced bar spectrum of the PCM tap's mid signal. The bars are laid out
// in Hz and the analysis window is sized by the frame's sample rate, so a
// 24/192 track shows the same band, at the same resolution, as a 44.1 kHz one.

/** The band the bars span (capped at the frame's Nyquist frequency). */
const MIN_HZ = 20;
const MAX_HZ = 20_000;
/** The window the bars were designed on: 2048 samples at 48 kHz (~43 ms).
 *  Other rates get the power of two closest to the same duration. */
const REFERENCE_SIZE = 2048;
const REFERENCE_RATE = 48_000;
const MIN_SIZE = 1024;
const MAX_SIZE = 8192;
/** dBFS that leaves a bar empty; 0 dBFS fills it. */
const FLOOR_DB = -54;
/** The tap sends nothing while playback is paused or stopped. No frame for
 *  this long — or for this many frames' worth, when frames are longer —
 *  means the music stopped. */
const STALE_MIN_MS = 150;
const STALE_FRAMES = 2.5;

/** FFT size for a sample rate: about the reference window's duration. */
export function fftSizeFor(sampleRate: number): number {
  const ideal = (REFERENCE_SIZE * sampleRate) / REFERENCE_RATE;
  const size = 2 ** Math.round(Math.log2(Math.max(1, ideal)));
  return Math.min(MAX_SIZE, Math.max(MIN_SIZE, size));
}

/**
 * First FFT bin of each bar plus the end of the last one, log-spaced from
 * MIN_HZ to MAX_HZ (or Nyquist, if lower). A bar always covers at least one
 * bin, so the narrow bass bars repeat a bin rather than stay empty.
 */
export function barEdges(bars: number, size: number, sampleRate: number): Int32Array {
  const binHz = sampleRate / size;
  const top = Math.max(MIN_HZ, Math.min(MAX_HZ, sampleRate / 2));
  const edges = new Int32Array(bars + 1);
  for (let b = 0; b <= bars; b++) {
    const hz = MIN_HZ * Math.pow(top / MIN_HZ, b / bars);
    edges[b] = Math.min(size / 2, Math.max(1, Math.round(hz / binHz)));
  }
  return edges;
}

function hann(size: number): Float32Array {
  const window = new Float32Array(size);
  for (let i = 0; i < size; i++) window[i] = 0.5 * (1 - Math.cos((2 * Math.PI * i) / (size - 1)));
  return window;
}

/** In-place iterative radix-2 FFT (Cooley–Tukey). */
function fft(reArr: Float32Array, imArr: Float32Array) {
  const n = reArr.length;
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) {
      [reArr[i], reArr[j]] = [reArr[j], reArr[i]];
      [imArr[i], imArr[j]] = [imArr[j], imArr[i]];
    }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const ang = (-2 * Math.PI) / len;
    const wRe = Math.cos(ang);
    const wIm = Math.sin(ang);
    for (let i = 0; i < n; i += len) {
      let curRe = 1;
      let curIm = 0;
      for (let k = 0; k < len >> 1; k++) {
        const p = i + k;
        const q = p + (len >> 1);
        const bRe = reArr[q] * curRe - imArr[q] * curIm;
        const bIm = reArr[q] * curIm + imArr[q] * curRe;
        reArr[q] = reArr[p] - bRe;
        imArr[q] = imArr[p] - bIm;
        reArr[p] += bRe;
        imArr[p] += bIm;
        const nextRe = curRe * wRe - curIm * wIm;
        curIm = curRe * wIm + curIm * wRe;
        curRe = nextRe;
      }
    }
  }
}

/**
 * Collects the tap's frames and turns the newest window into bar levels.
 * Frames are kept in a ring of the newest samples, so a backlog never turns
 * into latency: the analysis always looks at what arrived last.
 */
export class SpectrumAnalyzer {
  readonly #bars: number;
  #history = new Float32Array(MAX_SIZE);
  #write = 0;
  /** The rate the pitch of the samples belongs to; 0 until the first frame. */
  #rate = 0;
  #size = REFERENCE_SIZE;
  #edges: Int32Array;
  #window: Float32Array = new Float32Array(0);
  #re: Float32Array = new Float32Array(0);
  #im: Float32Array = new Float32Array(0);
  #fresh = false;
  #lastAt = Number.NEGATIVE_INFINITY;
  #frameMs = 0;

  constructor(bars: number) {
    this.#bars = bars;
    this.#edges = new Int32Array(bars + 1);
  }

  /**
   * Add one frame's mid signal, received at `now` (ms). `sampleRate` is the
   * frame's pace on the clock (the tap scales it by the playback speed), and
   * `pitchRate` the rate its content sounds at, where the two differ — a
   * speed other than 1 with the pitch kept.
   */
  push(mono: Float32Array, sampleRate: number, now: number, pitchRate = sampleRate) {
    const rate = Math.round(pitchRate > 0 ? pitchRate : REFERENCE_RATE);
    if (rate !== this.#rate) this.#configure(rate);
    // After a pause, do not splice the new music onto the old.
    else if (this.isStale(now)) this.#history.fill(0);
    for (let i = Math.max(0, mono.length - MAX_SIZE); i < mono.length; i++) {
      this.#history[this.#write] = mono[i];
      this.#write = (this.#write + 1) % MAX_SIZE;
    }
    this.#frameMs = (mono.length / (sampleRate > 0 ? sampleRate : rate)) * 1000;
    this.#lastAt = now;
    this.#fresh = true;
  }

  /** No frame for a while at `now` (ms): playback paused, stopped or stalled. */
  isStale(now: number): boolean {
    return now - this.#lastAt > Math.max(STALE_MIN_MS, STALE_FRAMES * this.#frameMs);
  }

  /** Bar levels (0..1) of the newest window into `out`; false when no frame
   *  arrived since the last call (`out` is left alone then). */
  analyze(out: Float32Array): boolean {
    if (!this.#fresh) return false;
    this.#fresh = false;
    const size = this.#size;
    const re = this.#re;
    const im = this.#im;
    // The newest `size` samples, oldest first.
    let at = (this.#write - size + MAX_SIZE) % MAX_SIZE;
    for (let i = 0; i < size; i++) {
      re[i] = this.#history[at] * this.#window[i];
      at = (at + 1) % MAX_SIZE;
    }
    im.fill(0);
    fft(re, im);
    const norm = size / 2;
    for (let b = 0; b < this.#bars; b++) {
      let peak = 0;
      const start = this.#edges[b];
      const end = Math.max(start + 1, this.#edges[b + 1]);
      for (let bin = start; bin < end; bin++) {
        const mag = Math.hypot(re[bin], im[bin]) / norm;
        if (mag > peak) peak = mag;
      }
      const db = 20 * Math.log10(peak + 1e-6);
      out[b] = Math.min(1, Math.max(0, (db - FLOOR_DB) / -FLOOR_DB));
    }
    return true;
  }

  #configure(rate: number) {
    this.#rate = rate;
    this.#size = fftSizeFor(rate);
    this.#edges = barEdges(this.#bars, this.#size, rate);
    this.#window = hann(this.#size);
    this.#re = new Float32Array(this.#size);
    this.#im = new Float32Array(this.#size);
    this.#history.fill(0);
  }
}
