import { describe, expect, it } from "vitest";
import { SpectrumAnalyzer, barEdges, fftSizeFor } from "./spectrum";

const BARS = 56;

/** One tap frame's worth (2048 frames) of a sine at `hz`, mid signal. */
function sine(hz: number, rate: number, frames = 2048, start = 0): Float32Array {
  const out = new Float32Array(frames);
  for (let i = 0; i < frames; i++) out[i] = 0.5 * Math.sin((2 * Math.PI * hz * (start + i)) / rate);
  return out;
}

/** Feed a tone the way the tap delivers it and return the loudest bar. */
function loudestBar(hz: number, rate: number, pitchRate = rate): number {
  const analyzer = new SpectrumAnalyzer(BARS);
  const bars = new Float32Array(BARS);
  // Enough frames to fill the largest window (8192 samples at 192 kHz).
  for (let frame = 0; frame < 4; frame++) {
    analyzer.push(sine(hz, pitchRate, 2048, frame * 2048), rate, frame * 10, pitchRate);
  }
  expect(analyzer.analyze(bars)).toBe(true);
  return bars.indexOf(Math.max(...bars));
}

describe("fftSizeFor", () => {
  it("keeps the analysis window near 43 ms at any rate", () => {
    expect(fftSizeFor(44_100)).toBe(2048);
    expect(fftSizeFor(48_000)).toBe(2048);
    expect(fftSizeFor(88_200)).toBe(4096);
    expect(fftSizeFor(96_000)).toBe(4096);
    expect(fftSizeFor(176_400)).toBe(8192);
    expect(fftSizeFor(192_000)).toBe(8192);
    expect(fftSizeFor(22_050)).toBe(1024);
    expect(fftSizeFor(8_000)).toBe(1024);
  });
});

describe("barEdges", () => {
  it("spans the audible band, not the bins, so hi-res tracks leave no bar dead", () => {
    // 192 kHz: Nyquist is 96 kHz, but the last bar must still end at 20 kHz.
    const size = fftSizeFor(192_000);
    const edges = barEdges(BARS, size, 192_000);
    const binHz = 192_000 / size;
    expect(edges[BARS] * binHz).toBeLessThanOrEqual(20_100);
    expect(edges[BARS] * binHz).toBeGreaterThan(19_900);
    // The same layout in Hz as at 48 kHz.
    const reference = barEdges(BARS, 2048, 48_000);
    for (let b = 0; b <= BARS; b++) {
      expect(Math.abs(edges[b] * binHz - reference[b] * (48_000 / 2048))).toBeLessThan(binHz);
    }
  });

  it("stops at Nyquist for low rates and never leaves a bar without a bin", () => {
    const edges = barEdges(BARS, 1024, 22_050);
    expect(edges[BARS]).toBe(512);
    for (let b = 0; b < BARS; b++) {
      expect(edges[b]).toBeGreaterThanOrEqual(1);
      expect(edges[b + 1]).toBeGreaterThanOrEqual(edges[b]);
    }
  });
});

describe("SpectrumAnalyzer", () => {
  it("puts a tone in the same bar whatever the track's sample rate", () => {
    // 96 and 192 kHz get windows of the same length, so even the bass lines up.
    for (const hz of [60, 1_000, 10_000]) {
      const reference = loudestBar(hz, 48_000);
      expect(loudestBar(hz, 96_000)).toBe(reference);
      expect(loudestBar(hz, 192_000)).toBe(reference);
    }
    // 44.1 kHz bins are a little narrower; above the bass that never shows.
    for (const hz of [1_000, 3_000, 10_000]) {
      expect(loudestBar(hz, 44_100)).toBe(loudestBar(hz, 48_000));
    }
  });

  it("places a tone by its pitch, not by the frame's pace, at another speed", () => {
    // 1.5× with the pitch kept: frames come stamped 72 kHz (48 kHz × 1.5),
    // while the music still sounds at 48 kHz pitch.
    expect(loudestBar(1_000, 72_000, 48_000)).toBe(loudestBar(1_000, 48_000));
  });

  it("only analyses when a frame arrived since last time", () => {
    const analyzer = new SpectrumAnalyzer(BARS);
    const bars = new Float32Array(BARS);
    expect(analyzer.analyze(bars)).toBe(false);
    analyzer.push(sine(440, 48_000), 48_000, 0);
    expect(analyzer.analyze(bars)).toBe(true);
    expect(analyzer.analyze(bars)).toBe(false);
  });

  it("calls the feed stale when frames stop, scaled to how long a frame lasts", () => {
    const analyzer = new SpectrumAnalyzer(BARS);
    expect(analyzer.isStale(0)).toBe(true); // nothing ever arrived
    // 48 kHz: a frame every ~43 ms, so 150 ms without one means it stopped.
    analyzer.push(sine(440, 48_000), 48_000, 1_000);
    expect(analyzer.isStale(1_100)).toBe(false);
    expect(analyzer.isStale(1_151)).toBe(true);
    // 8 kHz at half speed: a 2048-frame chunk lasts half a second on the clock.
    analyzer.push(sine(440, 8_000), 4_000, 2_000, 8_000);
    expect(analyzer.isStale(2_900)).toBe(false);
    expect(analyzer.isStale(3_400)).toBe(true);
  });
});
