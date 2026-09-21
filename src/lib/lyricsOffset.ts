/**
 * Manual per-track lyrics sync nudge (ms on top of the server offset;
 * positive = lyrics earlier), persisted so a track that's always a beat off
 * stays corrected across sessions. Every window reads the same key.
 */
const KEY_PREFIX = "jellysic.lyricsOffset.";

export const lyricsOffsetKey = (itemId: string) => `${KEY_PREFIX}${itemId}`;

/** The saved nudge for a track; 0 when unset, unreadable, or not a number. */
export function loadLyricsOffset(itemId: string, storage?: Storage): number {
  try {
    const raw = (storage ?? localStorage).getItem(lyricsOffsetKey(itemId));
    const value = raw === null ? 0 : Number(raw);
    return Number.isFinite(value) ? value : 0;
  } catch {
    return 0;
  }
}

/** Remembers the nudge; 0 drops the key. */
export function saveLyricsOffset(itemId: string, offsetMs: number, storage?: Storage): void {
  try {
    const store = storage ?? localStorage;
    if (offsetMs === 0) store.removeItem(lyricsOffsetKey(itemId));
    else store.setItem(lyricsOffsetKey(itemId), String(offsetMs));
  } catch {
    // Storage unavailable: the nudge still applies for this session.
  }
}
