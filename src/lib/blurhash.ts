import { decode } from "blurhash";

const SIZE = 32;
/** Room for every cover of the longest list that is not virtualized: the
 *  queue panel shows up to FLIP_ROW_LIMIT (400) rows in full and redraws them
 *  on every track change, which then hits the cache instead of decoding each
 *  placeholder again. */
const MAX_ENTRIES = 512;
const cache = new Map<string, string | null>();

/** Decode a blurhash into a small data-URL image (memoized per hash). */
export function blurhashToDataUrl(hash: string): string | null {
  const cached = cache.get(hash);
  if (cached !== undefined) {
    // Back to the end of the line: a list on screen keeps its covers while
    // browsing elsewhere cycles the others out.
    cache.delete(hash);
    cache.set(hash, cached);
    return cached;
  }

  let url: string | null = null;
  try {
    const pixels = decode(hash, SIZE, SIZE);
    const canvas = document.createElement("canvas");
    canvas.width = SIZE;
    canvas.height = SIZE;
    const ctx = canvas.getContext("2d");
    if (ctx) {
      const imageData = ctx.createImageData(SIZE, SIZE);
      imageData.data.set(pixels);
      ctx.putImageData(imageData, 0, 0);
      url = canvas.toDataURL();
    }
  } catch {
    url = null; // malformed hash from the server — placeholder just stays off
  }
  cache.set(hash, url);
  // Bound the cache (least recently used out first) so browsing a huge
  // library doesn't leak thousands of data-URL strings for the session's
  // lifetime.
  if (cache.size > MAX_ENTRIES) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return url;
}
