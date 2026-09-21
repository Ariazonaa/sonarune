import { describe, expect, it } from "vitest";
import {
  percentOf,
  positionAtPointer,
  positionForKey,
  SEEK_PAGE_MS,
  SEEK_STEP_MS,
  WAVE_MIN_BAR,
  waveformBars,
} from "$lib/seek";

describe("waveformBars", () => {
  it("keeps the loudest peak of each group", () => {
    const peaks = [0, 255, 0, 0, 51, 0, 0, 0];
    expect(waveformBars(peaks, 4)).toEqual([1, WAVE_MIN_BAR, 0.2, WAVE_MIN_BAR]);
  });

  it("gives every bar at least one peak, even with more bars than peaks", () => {
    const bars = waveformBars([255, 0], 5);
    expect(bars).toHaveLength(5);
    expect(bars.every((bar) => bar >= WAVE_MIN_BAR && bar <= 1)).toBe(true);
  });

  it("is empty without data", () => {
    expect(waveformBars([], 100)).toEqual([]);
    expect(waveformBars([255], 0)).toEqual([]);
  });
});

describe("positionAtPointer", () => {
  it("maps the pointer onto the track and clamps outside the bar", () => {
    // Bar from x=100 to x=300, 200 s track.
    expect(positionAtPointer(100, 100, 200, 200_000)).toBe(0);
    expect(positionAtPointer(200, 100, 200, 200_000)).toBe(100_000);
    expect(positionAtPointer(300, 100, 200, 200_000)).toBe(200_000);
    expect(positionAtPointer(50, 100, 200, 200_000)).toBe(0);
    expect(positionAtPointer(900, 100, 200, 200_000)).toBe(200_000);
  });

  it("never divides by a collapsed bar or an unknown duration", () => {
    expect(positionAtPointer(150, 100, 0, 200_000)).toBe(0);
    expect(positionAtPointer(150, 100, 200, 0)).toBe(0);
  });
});

describe("positionForKey", () => {
  it("steps like the global shortcuts and pages by 30 s", () => {
    expect(positionForKey("ArrowRight", 60_000, 200_000)).toBe(60_000 + SEEK_STEP_MS);
    expect(positionForKey("ArrowUp", 60_000, 200_000)).toBe(60_000 + SEEK_STEP_MS);
    expect(positionForKey("ArrowLeft", 60_000, 200_000)).toBe(60_000 - SEEK_STEP_MS);
    expect(positionForKey("PageUp", 60_000, 200_000)).toBe(60_000 + SEEK_PAGE_MS);
    expect(positionForKey("PageDown", 60_000, 200_000)).toBe(60_000 - SEEK_PAGE_MS);
    expect(positionForKey("Home", 60_000, 200_000)).toBe(0);
    expect(positionForKey("End", 60_000, 200_000)).toBe(200_000);
  });

  it("stays inside the track", () => {
    expect(positionForKey("ArrowLeft", 2_000, 200_000)).toBe(0);
    expect(positionForKey("PageUp", 190_000, 200_000)).toBe(200_000);
  });

  it("ignores keys the bar does not own", () => {
    expect(positionForKey("Enter", 60_000, 200_000)).toBeNull();
    expect(positionForKey(" ", 60_000, 200_000)).toBeNull();
  });
});

describe("percentOf", () => {
  it("is clamped and safe for an unknown duration", () => {
    expect(percentOf(50_000, 200_000)).toBe(25);
    expect(percentOf(300_000, 200_000)).toBe(100);
    expect(percentOf(-1, 200_000)).toBe(0);
    expect(percentOf(10, 0)).toBe(0);
  });
});
