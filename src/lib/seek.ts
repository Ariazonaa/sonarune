// Pure seek-bar math, shared by the player bar and the now-playing view.

/** Same step as the global ←/→ shortcuts, so a focused bar feels identical. */
export const SEEK_STEP_MS = 5_000;
export const SEEK_PAGE_MS = 30_000;

/** Media position under a pointer on a horizontal bar, clamped to the track. */
export function positionAtPointer(
  clientX: number,
  left: number,
  width: number,
  durationMs: number,
): number {
  if (width <= 0 || durationMs <= 0) return 0;
  const fraction = Math.min(1, Math.max(0, (clientX - left) / width));
  return Math.round(fraction * durationMs);
}

/** Seek target for a slider key, or null for a key the bar does not handle. */
export function positionForKey(key: string, positionMs: number, durationMs: number): number | null {
  let target: number;
  switch (key) {
    case "ArrowLeft":
    case "ArrowDown":
      target = positionMs - SEEK_STEP_MS;
      break;
    case "ArrowRight":
    case "ArrowUp":
      target = positionMs + SEEK_STEP_MS;
      break;
    case "PageDown":
      target = positionMs - SEEK_PAGE_MS;
      break;
    case "PageUp":
      target = positionMs + SEEK_PAGE_MS;
      break;
    case "Home":
      target = 0;
      break;
    case "End":
      target = durationMs;
      break;
    default:
      return null;
  }
  return Math.min(Math.max(0, target), Math.max(0, durationMs));
}

/** Bars never shrink below this share of the height, so silence still
 *  reads as part of the track rather than a gap. */
export const WAVE_MIN_BAR = 0.08;

/**
 * Group a track's peaks (0–255, see `player/waveform.rs`) into `count` bars
 * of 0–1 height. Each bar takes the loudest peak of its group, so short
 * transients survive the downsampling.
 */
export function waveformBars(peaks: readonly number[], count: number): number[] {
  if (peaks.length === 0 || count <= 0) return [];
  const bars: number[] = [];
  for (let bar = 0; bar < count; bar++) {
    const from = Math.floor((bar * peaks.length) / count);
    const to = Math.max(from + 1, Math.floor(((bar + 1) * peaks.length) / count));
    let loudest = 0;
    for (let i = from; i < to && i < peaks.length; i++) loudest = Math.max(loudest, peaks[i]);
    bars.push(Math.max(WAVE_MIN_BAR, Math.min(1, loudest / 255)));
  }
  return bars;
}

/** Where `ms` sits on a track of `durationMs`, in percent (0–100). */
export function percentOf(ms: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return Math.min(100, Math.max(0, (ms / durationMs) * 100));
}
