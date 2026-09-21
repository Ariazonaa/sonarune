/**
 * Dominant-color extraction for the ambient backdrop and the cover accent.
 * Covers come from the jfimg protocol, which sends ACAO so the canvas stays
 * untainted.
 */

import type { CoverHue } from "$lib/theme";

export interface AmbientColors {
  /** Two dark backdrop colors (lightness clamped to 15–35 %). */
  a: string;
  b: string;
  /** The most prominent clearly colored area, for the accent; null for
   *  greyscale artwork. `theme.ts` picks its lightness per mode. */
  accent: CoverHue | null;
}

/** An accent candidate needs this many of the 24x24 samples (~0.7 %), so a
 *  few stray pixels cannot win over the artwork itself. */
const ACCENT_MIN_SAMPLES = 4;

const SAMPLE_SIZE = 24;
const CACHE_MAX = 32;

// Memoized per cover URL (promise, so concurrent callers share one fetch).
const cache = new Map<string, Promise<AmbientColors | null>>();

export function ambientColors(url: string): Promise<AmbientColors | null> {
  let pending = cache.get(url);
  if (!pending) {
    // Memoize successes only: a transient cover failure must not disable the
    // backdrop for this track forever.
    pending = extract(url).then(
      (colors) => {
        if (colors === null) cache.delete(url);
        return colors;
      },
      () => {
        cache.delete(url);
        return null;
      },
    );
    cache.set(url, pending);
    if (cache.size > CACHE_MAX) {
      const oldest = cache.keys().next().value;
      if (oldest !== undefined) cache.delete(oldest);
    }
  }
  return pending;
}

interface Bucket {
  n: number;
  r: number;
  g: number;
  b: number;
}

async function extract(url: string): Promise<AmbientColors | null> {
  const img = await loadImage(url);
  const canvas = document.createElement("canvas");
  canvas.width = SAMPLE_SIZE;
  canvas.height = SAMPLE_SIZE;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) return null;
  ctx.drawImage(img, 0, 0, SAMPLE_SIZE, SAMPLE_SIZE);
  // Throws SecurityError on a tainted canvas -> caught by the caller.
  const { data } = ctx.getImageData(0, 0, SAMPLE_SIZE, SAMPLE_SIZE);

  // Quantize to 4 bits per channel and average each bucket's members.
  const buckets = new Map<number, Bucket>();
  for (let i = 0; i < data.length; i += 4) {
    if (data[i + 3] < 128) continue;
    const key = ((data[i] >> 4) << 8) | ((data[i + 1] >> 4) << 4) | (data[i + 2] >> 4);
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = { n: 0, r: 0, g: 0, b: 0 };
      buckets.set(key, bucket);
    }
    bucket.n++;
    bucket.r += data[i];
    bucket.g += data[i + 1];
    bucket.b += data[i + 2];
  }
  if (buckets.size === 0) return null;

  const candidates = [...buckets.values()].map(({ n, r, g, b }) => ({
    n,
    ...rgbToHsl(r / n, g / n, b / n),
  }));
  // Population weighted toward saturation; near-black/white buckets demoted
  // so grey borders don't win over the artwork itself.
  const score = (c: (typeof candidates)[number]) =>
    c.n * (0.15 + c.s) * (c.l > 0.04 && c.l < 0.95 ? 1 : 0.1);
  candidates.sort((x, y) => score(y) - score(x));

  const first = candidates[0];
  const second =
    candidates.find(
      (c) => c !== first && (hueDistance(c.h, first.h) > 30 || Math.abs(c.l - first.l) > 0.25),
    ) ??
    candidates[1] ??
    first;
  return { a: toCss(first), b: toCss(second), accent: pickAccent(candidates) };
}

/**
 * The accent is not the backdrop's first pick — that may be a large dark or
 * washed-out area. It is the most prominent bucket that is clearly colored:
 * population weighted by saturation, ignoring near-black, near-white and
 * near-grey buckets.
 */
export function pickAccent(
  candidates: readonly { n: number; h: number; s: number; l: number }[],
): CoverHue | null {
  let best: (typeof candidates)[number] | null = null;
  for (const c of candidates) {
    if (c.n < ACCENT_MIN_SAMPLES || c.s < 0.25 || c.l <= 0.12 || c.l >= 0.9) continue;
    if (!best || c.n * c.s > best.n * best.s) best = c;
  }
  return best ? { h: Math.round(best.h), s: best.s } : null;
}

function toCss(c: { h: number; s: number; l: number }): string {
  // Keep the backdrop dark regardless of the artwork's brightness.
  const l = Math.min(0.35, Math.max(0.15, c.l));
  return `hsl(${Math.round(c.h)} ${Math.round(c.s * 100)}% ${Math.round(l * 100)}%)`;
}

function hueDistance(a: number, b: number): number {
  const d = Math.abs(a - b) % 360;
  return d > 180 ? 360 - d : d;
}

function rgbToHsl(r: number, g: number, b: number): { h: number; s: number; l: number } {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return { h: 0, s: 0, l };
  const s = d / (1 - Math.abs(2 * l - 1));
  let h: number;
  if (max === r) h = ((g - b) / d) % 6;
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return { h: (h * 60 + 360) % 360, s: Math.min(1, s), l };
}

function loadImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("cover load failed"));
    img.src = url;
  });
}
