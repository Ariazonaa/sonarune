import { describe, expect, it } from "vitest";
import type { Monitor } from "@tauri-apps/api/window";
import {
  DEFAULT_PROJECTOR_SETTINGS,
  burnInTransform,
  isOverlayVisible,
  monitorKey,
  normalizeProjectorSettings,
  selectedMonitor,
} from "./projector";

function monitor(name: string, x = 0, y = 0, w = 1920, h = 1080): Monitor {
  return {
    name,
    position: { x, y },
    size: { width: w, height: h },
    scaleFactor: 1,
  } as unknown as Monitor;
}

describe("normalizeProjectorSettings", () => {
  it("returns defaults for junk input", () => {
    expect(normalizeProjectorSettings(null)).toEqual(DEFAULT_PROJECTOR_SETTINGS);
    expect(normalizeProjectorSettings("nope")).toEqual(DEFAULT_PROJECTOR_SETTINGS);
  });

  it("clamps overlay opacity and timeout, and falls back on bad fields", () => {
    const result = normalizeProjectorSettings({
      monitorKey: 123, // wrong type -> null
      overlays: { cover: { enabled: "yes", opacity: 5, timeoutMs: -10 } },
    });
    expect(result.monitorKey).toBeNull();
    expect(result.overlays.cover.opacity).toBe(1); // clamped to max
    expect(result.overlays.cover.timeoutMs).toBe(0); // clamped to min
    // enabled was not a boolean -> falls back to the default (true)
    expect(result.overlays.cover.enabled).toBe(true);
    // untouched overlays keep their defaults
    expect(result.overlays.lyrics).toEqual(DEFAULT_PROJECTOR_SETTINGS.overlays.lyrics);
  });
});

describe("isOverlayVisible", () => {
  const layer = { enabled: true, opacity: 1, timeoutMs: 5000 };
  it("stays visible before the timeout and hides after", () => {
    expect(isOverlayVisible(layer, 1000, 0)).toBe(true);
    expect(isOverlayVisible(layer, 6000, 0)).toBe(false);
  });
  it("timeout 0 means always visible while enabled", () => {
    expect(isOverlayVisible({ ...layer, timeoutMs: 0 }, 1e9, 0)).toBe(true);
    expect(isOverlayVisible({ ...layer, enabled: false, timeoutMs: 0 }, 0, 0)).toBe(false);
  });
});

describe("monitor selection", () => {
  it("selectedMonitor falls back to the first monitor for an unknown key", () => {
    const monitors = [monitor("A", 0, 0), monitor("B", 1920, 0)];
    expect(selectedMonitor(monitors, monitorKey(monitors[1]))?.name).toBe("B");
    expect(selectedMonitor(monitors, "does-not-exist")?.name).toBe("A");
    expect(selectedMonitor([], "x")).toBeUndefined();
  });
});

describe("burnInTransform", () => {
  const shape = /^translate3d\(-?\d+vw, -?\d+vh, 0\)$/;
  it("is a no-op when disabled", () => {
    expect(burnInTransform("cover", 3, "track-1", false, false)).toBe("translate3d(0, 0, 0)");
  });
  it("produces a bounded, deterministic transform", () => {
    const out = burnInTransform("cover", 1, "track-1", true, false);
    expect(out).toMatch(shape);
    expect(burnInTransform("cover", 1, "track-1", true, false)).toBe(out);
  });
  it("in reduced-motion mode ignores the drift step", () => {
    const a = burnInTransform("cover", 1, "track-1", true, true);
    const b = burnInTransform("cover", 99, "track-1", true, true);
    expect(a).toBe(b);
    expect(a).toMatch(shape);
  });
});
