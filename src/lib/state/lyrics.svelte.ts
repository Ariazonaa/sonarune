import { api } from "$lib/api";
import { loadLyricsOffset, lyricsOffsetKey, saveLyricsOffset } from "$lib/lyricsOffset";
import { player } from "$lib/state/player.svelte";
import { session } from "$lib/state/session.svelte";
import type { LyricsDto } from "$lib/types";

const CACHE_MAX = 10;

/**
 * Follows the current track and keeps its lyrics ready (small LRU cache so
 * skipping back and forth doesn't refetch). Fetch failures degrade to
 * "no lyrics".
 */
class LyricsStore {
  lyrics = $state<LyricsDto | null>(null);
  loading = $state(false);
  /** Track the exposed `lyrics` belong to. */
  forTrackId = $state<string | null>(null);
  /**
   * Manual sync nudge for `forTrackId` in ms, on top of the server offset
   * (positive = lyrics earlier). Kept here rather than per view so the lyrics
   * panel and now playing always apply the same correction.
   */
  manualOffsetMs = $state(0);

  #cache = new Map<string, LyricsDto>(); // insertion order = LRU order
  // Plain field (untracked) so the watcher effect doesn't depend on it.
  #requestedFor: string | null = null;

  constructor() {
    // App-lifetime singleton; the root is intentionally never disposed.
    $effect.root(() => {
      $effect(() => {
        const itemId = session.info ? (player.state.current?.itemId ?? null) : null;
        this.#track(itemId);
      });
    });
    // Other windows (the projector) share localStorage but not this store;
    // `storage` fires only for changes made in another window.
    if (typeof window !== "undefined") {
      window.addEventListener("storage", (event) => {
        const id = this.forTrackId;
        if (id && (event.key === null || event.key === lyricsOffsetKey(id))) {
          this.manualOffsetMs = loadLyricsOffset(id);
        }
      });
    }
  }

  /** Shift the current track's lyrics by `deltaMs` and remember it. */
  nudgeOffset(deltaMs: number) {
    const id = this.forTrackId;
    if (!id) return;
    this.manualOffsetMs += deltaMs;
    saveLyricsOffset(id, this.manualOffsetMs);
  }

  resetOffset() {
    const id = this.forTrackId;
    if (!id) return;
    this.manualOffsetMs = 0;
    saveLyricsOffset(id, 0);
  }

  #track(itemId: string | null) {
    if (itemId === this.#requestedFor) return;
    this.#requestedFor = itemId;
    this.manualOffsetMs = itemId ? loadLyricsOffset(itemId) : 0;
    if (!itemId) {
      this.lyrics = null;
      this.forTrackId = null;
      this.loading = false;
      return;
    }

    const cached = this.#cache.get(itemId);
    if (cached) {
      this.#cache.delete(itemId);
      this.#cache.set(itemId, cached);
      this.lyrics = cached;
      this.forTrackId = itemId;
      this.loading = false;
      return;
    }

    this.lyrics = null;
    this.forTrackId = itemId;
    this.loading = true;
    api
      .getLyrics(itemId)
      .then((dto) => {
        this.#cache.set(itemId, dto);
        if (this.#cache.size > CACHE_MAX) {
          const oldest = this.#cache.keys().next().value;
          if (oldest !== undefined) this.#cache.delete(oldest);
        }
        if (this.#requestedFor !== itemId) return; // stale response
        this.lyrics = dto;
        this.loading = false;
      })
      .catch((e) => {
        if (this.#requestedFor !== itemId) return;
        console.warn("lyrics fetch failed", e);
        this.lyrics = null;
        this.loading = false;
      });
  }
}

export const lyrics = new LyricsStore();
