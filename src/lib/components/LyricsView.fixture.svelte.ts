// Test-only stand-in for the lyrics store (LyricsView.test.ts): reactive like
// the real one, without its player/session wiring and server fetches.
import type { LyricsDto } from "#lib/types.js";

class FakeLyricsStore {
  lyrics = $state<LyricsDto | null>(null);
  loading = $state(false);
  forTrackId = $state<string | null>(null);
  manualOffsetMs = $state(0);

  nudgeOffset(deltaMs: number) {
    this.manualOffsetMs += deltaMs;
  }

  resetOffset() {
    this.manualOffsetMs = 0;
  }
}

export const lyrics = new FakeLyricsStore();
