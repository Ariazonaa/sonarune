<script lang="ts">
  import { onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { ACCENT_CHANGED } from "#lib/theme.js";
  import { player } from "#lib/state/player.svelte.js";
  import { decodeFrame, toMono } from "#lib/visualizer/pcm.js";
  import { SpectrumAnalyzer } from "#lib/visualizer/spectrum.js";

  // A lightweight spectrum analyzer. Subscribes to the same Rust PCM tap the
  // visualizer uses (multi-subscriber, so both can run) and draws a log-spaced
  // FFT of the post-EQ signal, folded from the stereo feed to its mid
  // channel. Zero cost server-side when nothing plays.
  let { class: cls = "h-16 w-full" }: { class?: string } = $props();

  const BARS = 56;

  let canvas = $state<HTMLCanvasElement | null>(null);

  const analyzer = new SpectrumAnalyzer(BARS);
  // Smoothed bar values and the levels they move toward.
  const targets = new Float32Array(BARS);
  const values = new Float32Array(BARS);

  onMount(() => {
    const channel = new Channel<ArrayBuffer>();
    // Every frame goes into the analyzer's window; the FFT runs once per
    // animation frame on the newest samples, so a backlog (the main thread
    // busy, e.g. Butterchurn also rendering) can't turn into latency.
    channel.onmessage = (frame) => {
      const pcm = decodeFrame(frame);
      if (!pcm) return;
      // The frame's rate is its pace on the clock, the track's rate times the
      // playback speed; with the pitch kept, the music still sounds at the
      // track's own rate.
      const { speed, preservePitch } = player.state;
      const pitchRate = preservePitch && speed > 0 ? pcm.sampleRate / speed : pcm.sampleRate;
      analyzer.push(toMono(pcm.samples, pcm.channels), pcm.sampleRate, performance.now(), pitchRate);
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

    // A spectrum, not a visualizer: it may sleep with its window, which then
    // simply sends it nothing (src-tauri/src/player/tap.rs, `TapConsumer`).
    invoke("visualizer_subscribe", { channel, consumer: "spectrum" }).catch(() => {});

    const draw = (now: number) => {
      if (disposed) return;
      raf = requestAnimationFrame(draw);
      // The tap sends nothing while playback is paused or stopped: let the
      // bars fall instead of freezing on the last spectrum.
      if (!analyzer.analyze(targets) && analyzer.isStale(now)) targets.fill(0);
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
