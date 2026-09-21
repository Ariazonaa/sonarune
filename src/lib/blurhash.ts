import { decode } from "blurhash";

const SIZE = 32;
const MAX_ENTRIES = 256;
const cache = new Map<string, string | null>();

/** Decode a blurhash into a small data-URL image (memoized per hash). */
export function blurhashToDataUrl(hash: string): string | null {
  const cached = cache.get(hash);
  if (cached !== undefined) return cached;

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
  // Bound the cache (FIFO by insertion) so browsing a huge library doesn't
  // leak thousands of data-URL strings for the session's lifetime.
  if (cache.size > MAX_ENTRIES) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return url;
}
