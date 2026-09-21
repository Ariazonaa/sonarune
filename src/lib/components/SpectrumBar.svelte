<script lang="ts">
  import { onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { ACCENT_CHANGED } from "$lib/theme";
  import { decodeFrame, toMono } from "$lib/visualizer/pcm";

  // A lightweight spectrum analyzer. Subscribes to the same Rust PCM tap the
  // visualizer uses (multi-subscriber, so both can run) and draws a log-spaced
  // FFT of the post-EQ signal, folded from the stereo feed to its mid
  // channel. Zero cost server-side when nothing plays.
  let { class: cls = "h-16 w-full" }: { class?: string } = $props();

  const FFT_SIZE = 2048;
  const BARS = 56;

  let canvas = $state<HTMLCanvasElement | null>(null);

  // Hann window + log-spaced bin ranges, precomputed once.
  const window_ = new Float32Array(FFT_SIZE);
  for (let i = 0; i < FFT_SIZE; i++) {
    window_[i] = 0.5 * (1 - Math.cos((2 * Math.PI * i) / (FFT_SIZE - 1)));
  }
  const binEdges = new Int32Array(BARS + 1);
  {
    const minBin = 1;
    const maxBin = FFT_SIZE / 2;
    for (let b = 0; b <= BARS; b++) {
      binEdges[b] = Math.round(minBin * Math.pow(maxBin / minBin, b / BARS));
    }
  }

  // Reusable FFT scratch + smoothed bar values.
  const re = new Float32Array(FFT_SIZE);
  const im = new Float32Array(FFT_SIZE);
  const targets = new Float32Array(BARS);
  const values = new Float32Array(BARS);

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

  function onFrame(frame: ArrayBuffer) {
    const pcm = decodeFrame(frame);
    if (!pcm) return;
    // One spectrum for the whole signal: the stereo feed's mid channel.
    const samples = toMono(pcm.samples, pcm.channels);
    const n = Math.min(samples.length, FFT_SIZE);
    for (let i = 0; i < n; i++) re[i] = samples[i] * window_[i];
    for (let i = n; i < FFT_SIZE; i++) re[i] = 0;
    im.fill(0);
    fft(re, im);
    const norm = FFT_SIZE / 2;
    for (let b = 0; b < BARS; b++) {
      let peak = 0;
      const end = Math.max(binEdges[b] + 1, binEdges[b + 1]);
      for (let bin = binEdges[b]; bin < end; bin++) {
        const mag = Math.hypot(re[bin], im[bin]) / norm;
        if (mag > peak) peak = mag;
      }
      // dBFS -54..0 → 0..1.
      const db = 20 * Math.log10(peak + 1e-6);
      targets[b] = Math.min(1, Math.max(0, (db + 54) / 54));
    }
  }

  onMount(() => {
    const channel = new Channel<ArrayBuffer>();
    // Keep only the newest frame; the FFT runs once per animation frame on it,
    // dropping any backlog so latency can't accumulate when the main thread is
    // busy (e.g. Butterchurn also rendering).
    let latestFrame: ArrayBuffer | null = null;
    channel.onmessage = (frame) => {
      latestFrame = frame;
    };
    let raf = 0;
    let disposed = false;
    // The canvas needs the color as a value; re-read it whenever the accent
    // changes (settings, or the cover accent following the track).
    const readAccent = () =>
      getComputedStyle(document.documentElement).getPropertyValue("--color-accent").trim() ||
      "#1db954";
    let accent = readAccent();
    const onAccentChanged = () => (accent = readAccent());
    window.addEventListener(ACCENT_CHANGED, onAccentChanged);

    invoke("visualizer_subscribe", { channel }).catch(() => {});

    const draw = () => {
      if (disposed) return;
      raf = requestAnimationFrame(draw);
      if (latestFrame) {
        onFrame(latestFrame);
        latestFrame = null;
      }
      const el = canvas;
      const ctx = el?.getContext("2d");
      if (!el || !ctx) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.max(1, Math.round(el.clientWidth * dpr));
      const h = Math.max(1, Math.round(el.clientHeight * dpr));
      if (el.width !== w || el.height !== h) {
        el.width = w;
        el.height = h;
      }
      ctx.clearRect(0, 0, w, h);
      ctx.fillStyle = accent;
      const gap = Math.max(1, Math.round(w / BARS / 6));
      const barW = (w - gap * (BARS - 1)) / BARS;
      for (let b = 0; b < BARS; b++) {
        // Fast attack, slow release for a lively-but-smooth feel.
        const t = targets[b];
        values[b] += (t - values[b]) * (t > values[b] ? 0.55 : 0.12);
        const bh = Math.max(dpr, values[b] * h);
        ctx.globalAlpha = 0.35 + 0.65 * values[b];
        ctx.fillRect(b * (barW + gap), h - bh, barW, bh);
      }
      ctx.globalAlpha = 1;
    };
    raf = requestAnimationFrame(draw);

    return () => {
      disposed = true;
      cancelAnimationFrame(raf);
      window.removeEventListener(ACCENT_CHANGED, onAccentChanged);
      invoke("visualizer_unsubscribe", { channelId: channel.id }).catch(() => {});
    };
  });
</script>

<canvas bind:this={canvas} class={cls} aria-hidden="true"></canvas>
