/**
 * Tiny stale-while-revalidate cache. Routes show the last value for a key
 * instantly on revisit, then refetch in the background — so navigating back
 * doesn't flash an empty page. Bounded by insertion order to stay small.
 */
const cache = new Map<string, unknown>();
const MAX_ENTRIES = 60;

export function swrGet<T>(key: string): T | undefined {
  return cache.get(key) as T | undefined;
}

export function swrSet<T>(key: string, value: T): void {
  // Re-insert so recently-used keys move to the end (evict from the front).
  cache.delete(key);
  cache.set(key, value);
  if (cache.size > MAX_ENTRIES) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
}

/** Drop a single entry (e.g. after a mutation invalidates it). */
export function swrDelete(key: string): void {
  cache.delete(key);
}

/** Clear everything (e.g. on sign-out). */
export function swrClear(): void {
  cache.clear();
}
