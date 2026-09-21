import butterchurn, { type ButterchurnVisualizer } from "butterchurn";
import { Channel, invoke } from "@tauri-apps/api/core";
import { m } from "$lib/paraglide/messages";
import { decodeFrame, resampleInterleaved, toMono, toStereo } from "./pcm";

export type EnergyLevel = "calm" | "medium" | "intense";

export interface EnergyState {
  level: EnergyLevel;
  /** Smoothed short-term loudness in dBFS (mean square of the mid signal). */
  rmsDb: number;
  /** True exactly once per detected energy break (drop, build, breakdown). */
  breakDetected: boolean;
  /** True on a detected onset/beat (broadband energy transient). */
  beatDetected: boolean;
}

/** Fast EMA time constant: reacts within ~1 s. */
const ENERGY_FAST_TAU_S = 1;
/** Slow EMA time constant: the "recent section" baseline. */
const ENERGY_SLOW_TAU_S = 10;
/** fast/slow mean-square ratio that counts as an energy break (~±5 dB). */
const BREAK_RATIO_UP = 3.0;
const BREAK_RATIO_DOWN = 1 / 3.0;
/** The ratio must hold this long before a break fires. */
const BREAK_SUSTAIN_S = 0.5;
/** Minimum gap between two breaks. */
const BREAK_COOLDOWN_S = 5;
/** Below this the signal counts as silence: always calm, never a break. */
const SILENCE_DB = -50;
/** Fixed dBFS thresholds; music normalized around -18 LUFS lands in between. */
const INTENSE_DB = -15;
const CALM_DB = -27;
/** Onset (beat) detection: a transient counts when the positive energy flux
 *  exceeds its recent average by this factor. */
const BEAT_FLUX_K = 1.5;
/** Minimum gap between beats (caps the rate ~8/s). */
const BEAT_COOLDOWN_S = 0.12;
/** Flux baseline time constant. */
const BEAT_FLUX_TAU_S = 0.5;

/**
 * Bridges the Rust PCM tap into a muted Web Audio graph that Butterchurn
 * analyses. The tap sends interleaved stereo (wire format in `pcm.ts` and
 * src-tauri/src/player/tap.rs); the worklet hands left and right to
 * Butterchurn's separate L/R analysers.
 */
export class VisualizerEngine {
  #ctx: AudioContext;
  #worklet: AudioWorkletNode;
  #sensitivity: GainNode;
  #visualizer: ButterchurnVisualizer;
  #canvas: HTMLCanvasElement;
  #channel: Channel<ArrayBuffer>;
  #onError: (message: string) => void;
  #contextLost: () => void;
  #raf = 0;
  #destroyed = false;
  /** Cap on the device-pixel-ratio used when sizing (quality control). */
  #dprCap = 3;

  // Energy analysis over the same PCM that feeds the visuals.
  #energyListener: ((state: EnergyState) => void) | null = null;
  #fastMs = 0;
  #slowMs = 0;
  #breakHeldS = 0;
  #breakCooldownS = 0;
  #prevMs = 0;
  #fluxAvg = 0;
  #beatCooldownS = 0;

  private constructor(
    ctx: AudioContext,
    worklet: AudioWorkletNode,
    sensitivity: GainNode,
    visualizer: ButterchurnVisualizer,
    canvas: HTMLCanvasElement,
    channel: Channel<ArrayBuffer>,
    onError: (message: string) => void,
  ) {
    this.#ctx = ctx;
    this.#worklet = worklet;
    this.#sensitivity = sensitivity;
    this.#visualizer = visualizer;
    this.#canvas = canvas;
    this.#channel = channel;
    this.#onError = onError;
    // Without preventDefault the context is never restorable; either way we
    // stop rendering and let the route surface the error.
    this.#contextLost = () => {
      this.#onError(m.visualizer_error_context_lost());
      cancelAnimationFrame(this.#raf);
    };
    canvas.addEventListener("webglcontextlost", this.#contextLost);
  }

  /**
   * `isCancelled` is checked before the subscription is installed so an
   * abandoned create (user already navigated away) never replaces a newer
   * engine's channel.
   */
  static async create(
    canvas: HTMLCanvasElement,
    onError: (message: string) => void,
    isCancelled: () => boolean,
  ): Promise<VisualizerEngine | null> {
    const ctx = new AudioContext({ sampleRate: 48000 });
    let engine: VisualizerEngine | undefined;
    try {
      await ctx.resume(); // opening the visualizer is the required user gesture
      await ctx.audioWorklet.addModule("/visualizer-worklet.js");
      if (isCancelled()) {
        await ctx.close();
        return null;
      }

      const worklet = new AudioWorkletNode(ctx, "jellysic-vis-feed", {
        numberOfInputs: 0,
        numberOfOutputs: 1,
        outputChannelCount: [2],
      });
      // Sensitivity: a gain stage between the feed and Butterchurn's analyser so
      // quiet tracks can be scaled up (or loud ones down) without touching audio.
      const sensitivity = ctx.createGain();
      sensitivity.gain.value = 1;
      worklet.connect(sensitivity);
      // Muted path to destination keeps the graph pulling samples.
      const mute = ctx.createGain();
      mute.gain.value = 0;
      worklet.connect(mute).connect(ctx.destination);

      // Butterchurn renders into the canvas backing store: size it in device
      // pixels (and keep pixelRatio 1 so texture sizes aren't scaled twice).
      const dpr = window.devicePixelRatio || 1;
      const width = Math.max(1, Math.round((canvas.clientWidth || 800) * dpr));
      const height = Math.max(1, Math.round((canvas.clientHeight || 600) * dpr));
      canvas.width = width;
      canvas.height = height;
      const visualizer = butterchurn.createVisualizer(ctx, canvas, {
        width,
        height,
        pixelRatio: 1,
      });
      visualizer.connectAudio(sensitivity);

      const channel = new Channel<ArrayBuffer>();
      const created = new VisualizerEngine(ctx, worklet, sensitivity, visualizer, canvas, channel, onError);
      engine = created;
      channel.onmessage = (frame) => created.#feed(frame);

      if (isCancelled()) {
        await created.destroy();
        return null;
      }
      await invoke("visualizer_subscribe", { channel });

      const loop = () => {
        if (created.#destroyed) return;
        try {
          created.#visualizer.render();
        } catch (e) {
          console.error("visualizer render failed", e);
          created.#onError(String(e));
          return; // stop the loop; the route shows the error banner
        }
        created.#raf = requestAnimationFrame(loop);
      };
      created.#raf = requestAnimationFrame(loop);

      return created;
    } catch (error) {
      // A failed setup must not leak what it already created: the audio
      // context, the context-lost listener, a half-installed subscription.
      await (engine ? engine.destroy() : ctx.close()).catch(() => {});
      throw error;
    }
  }

  #feed(frame: ArrayBuffer) {
    if (this.#destroyed) return;
    const pcm = decodeFrame(frame);
    if (!pcm) return;
    const rate = pcm.sampleRate > 0 ? pcm.sampleRate : 48000;
    // Energy and beats keep running on the mid signal: their thresholds were
    // tuned on the mono feed, and a power sum over two channels would read
    // up to 3 dB hotter on wide stereo.
    if (this.#energyListener) this.#analyzeEnergy(toMono(pcm.samples, pcm.channels), rate);
    let stereo = toStereo(pcm.samples, pcm.channels);
    if (rate !== this.#ctx.sampleRate) {
      stereo = resampleInterleaved(stereo, 2, rate, this.#ctx.sampleRate);
    }
    // Without a conversion this is pcm.samples itself; transferring detaches
    // it, which is fine — nothing reads it afterwards.
    this.#worklet.port.postMessage(stereo, [stereo.buffer]);
  }

  /** Register the (single) energy consumer; called ~23x/s while music plays. */
  setEnergyListener(listener: ((state: EnergyState) => void) | null) {
    this.#energyListener = listener;
  }

  #analyzeEnergy(samples: Float32Array, sampleRate: number) {
    if (!this.#energyListener || samples.length === 0) return;
    let sum = 0;
    for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i];
    const meanSquare = sum / samples.length;
    const dt = samples.length / sampleRate;

    const alphaFast = Math.min(1, dt / ENERGY_FAST_TAU_S);
    const alphaSlow = Math.min(1, dt / ENERGY_SLOW_TAU_S);
    this.#fastMs += alphaFast * (meanSquare - this.#fastMs);
    this.#slowMs += alphaSlow * (meanSquare - this.#slowMs);

    const rmsDb = 10 * Math.log10(this.#fastMs + 1e-10);
    const silent = rmsDb < SILENCE_DB;
    const level: EnergyLevel =
      silent || rmsDb < CALM_DB ? "calm" : rmsDb > INTENSE_DB ? "intense" : "medium";

    let breakDetected = false;
    this.#breakCooldownS = Math.max(0, this.#breakCooldownS - dt);
    const ratio = this.#fastMs / (this.#slowMs + 1e-10);
    if (!silent && (ratio > BREAK_RATIO_UP || ratio < BREAK_RATIO_DOWN)) {
      this.#breakHeldS += dt;
      if (this.#breakHeldS >= BREAK_SUSTAIN_S && this.#breakCooldownS === 0) {
        breakDetected = true;
        this.#breakCooldownS = BREAK_COOLDOWN_S;
        // The new section becomes the baseline immediately.
        this.#slowMs = this.#fastMs;
        this.#breakHeldS = 0;
      }
    } else {
      this.#breakHeldS = 0;
    }

    // Onset/beat: a positive spike in energy relative to its recent average.
    const flux = Math.max(0, meanSquare - this.#prevMs);
    this.#prevMs = meanSquare;
    const alphaFlux = Math.min(1, dt / BEAT_FLUX_TAU_S);
    this.#fluxAvg += alphaFlux * (flux - this.#fluxAvg);
    this.#beatCooldownS = Math.max(0, this.#beatCooldownS - dt);
    let beatDetected = false;
    if (!silent && this.#beatCooldownS === 0 && flux > this.#fluxAvg * BEAT_FLUX_K && flux > 1e-5) {
      beatDetected = true;
      this.#beatCooldownS = BEAT_COOLDOWN_S;
    }

    this.#energyListener({ level, rmsDb, breakDetected, beatDetected });
  }

  loadPreset(preset: unknown, blendSeconds = 2.7) {
    this.#visualizer.loadPreset(preset, blendSeconds);
  }

  /** Fire the MilkDrop song-title overlay (best-effort; ignored if absent). */
  songTitle(text: string) {
    if (this.#destroyed || !text) return;
    try {
      this.#visualizer.launchSongTitleAnim(text);
    } catch {
      // preset/build without title support — non-fatal
    }
  }

  /** Scale what Butterchurn "hears" (1 = unchanged); higher = more reactive. */
  setSensitivity(multiplier: number) {
    this.#sensitivity.gain.value = Math.max(0.1, multiplier);
  }

  /** Render quality: per-pixel mesh resolution, output AA, and a dpr cap. */
  setQuality(meshWidth: number, meshHeight: number, antialias: boolean, dprCap: number) {
    this.#dprCap = dprCap;
    try {
      this.#visualizer.setInternalMeshSize(meshWidth, meshHeight);
      this.#visualizer.setOutputAA(antialias);
    } catch {
      // older butterchurn without these knobs — ignore
    }
    this.setSize(this.#canvas.clientWidth, this.#canvas.clientHeight);
  }

  /** PNG data URL of the current frame, or null if capture isn't supported. */
  capture(): string | null {
    try {
      return this.#visualizer.toDataURL();
    } catch {
      return null;
    }
  }

  setSize(width: number, height: number) {
    const dpr = Math.min(window.devicePixelRatio || 1, this.#dprCap);
    const w = Math.max(1, Math.round(width * dpr));
    const h = Math.max(1, Math.round(height * dpr));
    this.#canvas.width = w;
    this.#canvas.height = h;
    this.#visualizer.setRendererSize(w, h);
  }

  async destroy() {
    if (this.#destroyed) return;
    this.#destroyed = true;
    cancelAnimationFrame(this.#raf);
    this.#canvas.removeEventListener("webglcontextlost", this.#contextLost);
    try {
      await invoke("visualizer_unsubscribe", { channelId: this.#channel.id });
    } catch {
      // session may already be gone; the tap self-disables on send failure
    }
    await this.#ctx.close();
  }
}
