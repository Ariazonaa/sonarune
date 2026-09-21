import type { Monitor } from "@tauri-apps/api/window";

export const PROJECTOR_WINDOW_LABEL = "projector";
export const PROJECTOR_SETTINGS_EVENT = "visualizer:projector-settings";
export const PROJECTOR_SETTINGS_KEY = "jellysic.vis.projector.v1";

export type VisualizerOverlayKind = "cover" | "track" | "progress" | "lyrics";

export interface VisualizerOverlaySettings {
  enabled: boolean;
  /** 0..1, applied independently per layer. */
  opacity: number;
  /** 0 keeps the layer visible; otherwise it hides this many milliseconds after activation. */
  timeoutMs: number;
}

export interface VisualizerProjectorSettings {
  monitorKey: string | null;
  fullscreen: boolean;
  presetLocked: boolean;
  burnInMovement: boolean;
  reducedMotion: boolean;
  /** When enabled, pointer movement does not reactivate timed media overlays. */
  songChangeOnly: boolean;
  overlays: Record<VisualizerOverlayKind, VisualizerOverlaySettings>;
}

const DEFAULT_OVERLAYS: Record<VisualizerOverlayKind, VisualizerOverlaySettings> = {
  cover: { enabled: true, opacity: 0.9, timeoutMs: 10_000 },
  track: { enabled: true, opacity: 1, timeoutMs: 10_000 },
  progress: { enabled: true, opacity: 0.8, timeoutMs: 0 },
  lyrics: { enabled: true, opacity: 1, timeoutMs: 0 },
};

export const DEFAULT_PROJECTOR_SETTINGS: VisualizerProjectorSettings = {
  monitorKey: null,
  fullscreen: true,
  presetLocked: false,
  burnInMovement: true,
  reducedMotion: false,
  songChangeOnly: false,
  overlays: DEFAULT_OVERLAYS,
};

const OVERLAY_KINDS: VisualizerOverlayKind[] = ["cover", "track", "progress", "lyrics"];

function finiteInRange(value: unknown, fallback: number, min: number, max: number): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(max, Math.max(min, value))
    : fallback;
}

function normalizeOverlay(
  value: unknown,
  fallback: VisualizerOverlaySettings,
): VisualizerOverlaySettings {
  const candidate = value && typeof value === "object"
    ? value as Partial<VisualizerOverlaySettings>
    : {};
  return {
    enabled: typeof candidate.enabled === "boolean" ? candidate.enabled : fallback.enabled,
    opacity: finiteInRange(candidate.opacity, fallback.opacity, 0.1, 1),
    timeoutMs: Math.round(finiteInRange(candidate.timeoutMs, fallback.timeoutMs, 0, 300_000)),
  };
}

/** Validate persisted/event data so a stale settings version cannot break the projector. */
export function normalizeProjectorSettings(value: unknown): VisualizerProjectorSettings {
  const candidate = value && typeof value === "object"
    ? value as Partial<VisualizerProjectorSettings>
    : {};
  const overlayInput = candidate.overlays && typeof candidate.overlays === "object"
    ? candidate.overlays as Partial<Record<VisualizerOverlayKind, VisualizerOverlaySettings>>
    : {};
  const overlays = {} as Record<VisualizerOverlayKind, VisualizerOverlaySettings>;
  for (const kind of OVERLAY_KINDS) {
    overlays[kind] = normalizeOverlay(overlayInput[kind], DEFAULT_OVERLAYS[kind]);
  }
  const bool = (value: unknown, fallback: boolean) =>
    typeof value === "boolean" ? value : fallback;
  const d = DEFAULT_PROJECTOR_SETTINGS;
  return {
    monitorKey: typeof candidate.monitorKey === "string" ? candidate.monitorKey : d.monitorKey,
    fullscreen: bool(candidate.fullscreen, d.fullscreen),
    presetLocked: bool(candidate.presetLocked, d.presetLocked),
    burnInMovement: bool(candidate.burnInMovement, d.burnInMovement),
    reducedMotion: bool(candidate.reducedMotion, d.reducedMotion),
    songChangeOnly: bool(candidate.songChangeOnly, d.songChangeOnly),
    overlays,
  };
}

export function loadProjectorSettings(): VisualizerProjectorSettings {
  try {
    const raw = localStorage.getItem(PROJECTOR_SETTINGS_KEY);
    return raw ? normalizeProjectorSettings(JSON.parse(raw)) : normalizeProjectorSettings(null);
  } catch {
    return normalizeProjectorSettings(null);
  }
}

export function saveProjectorSettings(settings: VisualizerProjectorSettings) {
  localStorage.setItem(PROJECTOR_SETTINGS_KEY, JSON.stringify(normalizeProjectorSettings(settings)));
}

/** Stable enough across restarts while still distinguishing equal-name displays. */
export function monitorKey(monitor: Monitor): string {
  return [
    monitor.name ?? "",
    monitor.position.x,
    monitor.position.y,
    monitor.size.width,
    monitor.size.height,
  ].join("|");
}

export function selectedMonitor(
  monitors: Monitor[],
  key: string | null,
): Monitor | undefined {
  return monitors.find((monitor) => monitorKey(monitor) === key) ?? monitors[0];
}

export function isOverlayVisible(
  layer: VisualizerOverlaySettings,
  now: number,
  activatedAt: number,
): boolean {
  return layer.enabled && (layer.timeoutMs === 0 || now - activatedAt < layer.timeoutMs);
}

/**
 * Small deterministic offsets keep static pixels from sitting in one place.
 * Reduced-motion mode changes position only when the track seed changes.
 */
export function burnInTransform(
  kind: VisualizerOverlayKind,
  step: number,
  trackSeed: string,
  enabled: boolean,
  reducedMotion: boolean,
): string {
  if (!enabled) return "translate3d(0, 0, 0)";
  let hash = 2166136261;
  const input = `${kind}:${reducedMotion ? trackSeed : step}`;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  const x = ((hash >>> 3) % 9) - 4;
  const y = ((hash >>> 11) % 7) - 3;
  return `translate3d(${x}vw, ${y}vh, 0)`;
}
