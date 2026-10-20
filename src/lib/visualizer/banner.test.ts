import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NOTICE_MS, VisualizerBanner } from "#lib/visualizer/banner.svelte.js";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("VisualizerBanner", () => {
  it("hides a notice by itself — the visualizer carried on", () => {
    const banner = new VisualizerBanner();
    banner.notice("Weekly presets are unavailable");
    expect(banner.message).toBe("Weekly presets are unavailable");
    expect(banner.isError).toBe(false);
    vi.advanceTimersByTime(NOTICE_MS - 1);
    expect(banner.message).not.toBeNull();
    vi.advanceTimersByTime(1);
    expect(banner.message).toBeNull();
  });

  it("restarts the clock for a newer notice", () => {
    const banner = new VisualizerBanner();
    banner.notice("first");
    vi.advanceTimersByTime(NOTICE_MS - 100);
    banner.notice("second");
    vi.advanceTimersByTime(NOTICE_MS - 100);
    expect(banner.message).toBe("second");
    vi.advanceTimersByTime(100);
    expect(banner.message).toBeNull();
  });

  it("keeps an error until it is dismissed, and lets no notice cover it", () => {
    const banner = new VisualizerBanner();
    banner.notice("a preset was skipped");
    banner.error("Graphics context lost");
    vi.advanceTimersByTime(NOTICE_MS * 10);
    banner.notice("another preset was skipped");
    banner.clearNotice();
    expect(banner.message).toBe("Graphics context lost");
    expect(banner.isError).toBe(true);

    banner.dismiss();
    expect(banner.message).toBeNull();
    banner.notice("visible again");
    expect(banner.message).toBe("visible again");
  });

  it("takes a notice down early when the action succeeded after all", () => {
    const banner = new VisualizerBanner();
    banner.notice("Import failed");
    banner.clearNotice();
    expect(banner.message).toBeNull();
  });
});
