import { describe, expect, it } from "vitest";
import { positionToVolume, stepVolume, volumeToPosition } from "#lib/volume.js";

describe("volume taper", () => {
  it("maps slider positions and gains onto each other", () => {
    expect(volumeToPosition(0)).toBe(0);
    expect(volumeToPosition(1)).toBe(1);
    expect(positionToVolume(0.5)).toBeCloseTo(0.5 ** 2.5);
    expect(volumeToPosition(positionToVolume(0.4))).toBeCloseTo(0.4);
    expect(volumeToPosition(-1)).toBe(0);
    expect(positionToVolume(2)).toBe(1);
  });

  it("steps along the slider, not in gain", () => {
    // Slider at 40 %: a linear -0.05 went 0.10 → 0.05 → 0.001 (silent) in
    // two presses. Along the slider each press is 5 % of its travel.
    const start = positionToVolume(0.4);
    const once = stepVolume(start, -0.05);
    const twice = stepVolume(once, -0.05);
    expect(volumeToPosition(once)).toBeCloseTo(0.35);
    expect(volumeToPosition(twice)).toBeCloseTo(0.3);
    expect(twice).toBeGreaterThan(0.04);
    // Near the top a step still moves the slider by the same 5 %.
    expect(volumeToPosition(stepVolume(positionToVolume(0.9), 0.05))).toBeCloseTo(0.95);
  });

  it("stays inside 0–1", () => {
    expect(stepVolume(1, 0.05)).toBe(1);
    expect(stepVolume(0, -0.05)).toBe(0);
    expect(stepVolume(0, 0.05)).toBeGreaterThan(0);
  });
});
