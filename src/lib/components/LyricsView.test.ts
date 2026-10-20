import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const clock = vi.hoisted(() => ({ positionMs: 30_000 }));
vi.mock("#lib/state/lyrics.svelte.js", () => import("./LyricsView.fixture.svelte.js"));
vi.mock("#lib/state/player.svelte.js", () => ({
  player: { estimatedPositionMs: () => clock.positionMs, run: () => {} },
}));

import LyricsView from "#lib/components/LyricsView.svelte";
import { setLocale } from "#lib/paraglide/runtime.js";
import { lyrics } from "./LyricsView.fixture.svelte.js";
import type { LyricsDto } from "#lib/types.js";

/** Synced lyrics with a line every 5 s from 5 s on; at 30 s line 5 is active. */
function syncedLyrics(tag = ""): LyricsDto {
  return {
    synced: true,
    offsetMs: 0,
    lines: Array.from({ length: 12 }, (_, i) => ({ startMs: (i + 1) * 5000, text: `line ${i}${tag}`, cues: [] })),
  };
}

let instance: Record<string, unknown> | null = null;
let scrolls: ScrollToOptions[];

beforeEach(() => {
  vi.useFakeTimers();
  scrolls = [];
  // jsdom has no scrolling; record what the view asks for.
  HTMLElement.prototype.scrollTo = function (options?: ScrollToOptions | number) {
    if (typeof options === "object") scrolls.push(options);
  } as HTMLElement["scrollTo"];
  clock.positionMs = 30_000;
  lyrics.lyrics = syncedLyrics();
  lyrics.forTrackId = "track-1";
  lyrics.manualOffsetMs = 0;
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  vi.useRealTimers();
  delete (HTMLElement.prototype as { scrollTo?: unknown }).scrollTo;
  document.body.innerHTML = "";
});

function render() {
  instance = mount(LyricsView, { target: document.body, props: { variant: "panel" } });
  flushSync();
}

const centered = (scroll: ScrollToOptions | undefined) => scroll?.behavior === "smooth";
const toTop = (scroll: ScrollToOptions | undefined) => scroll?.top === 0 && scroll.behavior === undefined;

describe("LyricsView", () => {
  it("opened while paused, centers the active line and leaves it there", () => {
    // The track-change reset ran on mount too, right after the centering, and
    // sent the view back to the top; paused, nothing centered it again.
    render();
    expect(centered(scrolls.at(-1))).toBe(true);
    expect(scrolls.some(toTop)).toBe(false);
  });

  it("on a new track, goes back to the top, then follows the new track's position", () => {
    render();
    scrolls = [];
    // A cached track: lyrics and track id change together, the scroller stays
    // — and the view's position is still the old track's.
    lyrics.lyrics = syncedLyrics("'");
    lyrics.forTrackId = "track-2";
    flushSync();
    expect(toTop(scrolls.at(-1))).toBe(true);

    // The next position tick brings the new track's position: line 1.
    clock.positionMs = 11_000;
    vi.advanceTimersByTime(200);
    flushSync();
    expect(centered(scrolls.at(-1))).toBe(true);
  });

  it("shows the sync offset with a sign and the locale's decimal mark", () => {
    lyrics.manualOffsetMs = 250;
    render();
    expect(document.body.textContent).toContain("+0.25s");
    unmount(instance!);

    setLocale("de", { reload: false });
    try {
      lyrics.manualOffsetMs = -500;
      render();
      expect(document.body.textContent).toContain("-0,50 s");
    } finally {
      setLocale("en", { reload: false });
    }
  });
});
