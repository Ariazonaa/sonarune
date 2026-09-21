import { VisualizerEngine, type EnergyLevel, type EnergyState } from "./engine";
import { m } from "$lib/paraglide/messages";
import {
  blankPreset,
  loadAdditionalPresetPacks,
  loadInitialPresetPack,
  PRESET_PACK_COUNT,
  PresetRotation,
  PresetFetchError,
  resolvePreset,
  type PresetCatalogProgress,
  type PresetEntry,
} from "./presets";

/** "auto" = energy-driven switching; a number = fixed timer; 0 = off. */
export type CycleSetting = "auto" | number;

export type VisualizerQuality = "low" | "medium" | "high";

const STORAGE_KEY = "jellysic.vis.cycle";
const BLEND_KEY = "jellysic.vis.blend";
const FAVORITES_KEY = "jellysic.vis.favorites";
const BLOCKED_KEY = "jellysic.vis.blocked";
const QUARANTINED_KEY = "jellysic.vis.quarantined";
const FAVORITES_ONLY_KEY = "jellysic.vis.favoritesOnly";
const SENSITIVITY_KEY = "jellysic.vis.sensitivity";
const QUALITY_KEY = "jellysic.vis.quality";
/** Opt-in for the online Butterchurn Weekly presets (see `PresetRotation`). */
export const REMOTE_PRESETS_KEY = "jellysic.vis.remotePresets";
const DEFAULT_CYCLE: CycleSetting = "auto";
const DEFAULT_BLEND = 2.7;
const DEFAULT_SENSITIVITY = 1;
const DEFAULT_QUALITY: VisualizerQuality = "medium";
/** Energy mode: never switch more often than this... */
const MIN_DWELL_S = 8;
/** ...and never stay on one preset longer than this. */
const MAX_DWELL_S = 45;
/** Longest we hold a decided switch waiting for a beat to land on. */
const BEAT_ALIGN_WAIT_S = 2;

/** Render-quality presets: per-pixel mesh, output AA, device-pixel-ratio cap. */
const QUALITY_SETTINGS: Record<
  VisualizerQuality,
  { mesh: [number, number]; aa: boolean; dprCap: number }
> = {
  low: { mesh: [24, 18], aa: false, dprCap: 1 },
  medium: { mesh: [32, 24], aa: false, dprCap: 2 },
  high: { mesh: [48, 36], aa: true, dprCap: 3 },
};

export function loadCycleSetting(): CycleSetting {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === "auto") return "auto";
  const parsed = Number(raw);
  return raw !== null && Number.isFinite(parsed) ? parsed : DEFAULT_CYCLE;
}

/** Preset transition time in seconds (0 = hard cut). */
export function loadBlendSetting(): number {
  const raw = localStorage.getItem(BLEND_KEY);
  if (raw === null) return DEFAULT_BLEND;
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : DEFAULT_BLEND;
}

export function loadSensitivitySetting(): number {
  const p = Number(localStorage.getItem(SENSITIVITY_KEY));
  return Number.isFinite(p) && p > 0 ? p : DEFAULT_SENSITIVITY;
}

export function loadQualitySetting(): VisualizerQuality {
  const raw = localStorage.getItem(QUALITY_KEY);
  return raw === "low" || raw === "high" ? raw : DEFAULT_QUALITY;
}

/** Off unless the user explicitly turned online presets on. */
export function loadRemotePresetsSetting(): boolean {
  try {
    return localStorage.getItem(REMOTE_PRESETS_KEY) === "true";
  } catch {
    return false;
  }
}

/** Stores the opt-in and returns what actually stuck (off if storage fails). */
export function saveRemotePresetsSetting(on: boolean): boolean {
  try {
    localStorage.setItem(REMOTE_PRESETS_KEY, String(on));
  } catch {
    // storage unavailable — the read-back below decides
  }
  return loadRemotePresetsSetting();
}

function loadNameSet(key: string): Set<string> {
  try {
    const raw = localStorage.getItem(key);
    return raw ? new Set<string>(JSON.parse(raw)) : new Set();
  } catch {
    return new Set();
  }
}

interface ControllerCallbacks {
  onError: (message: string) => void;
  onPreset: (name: string) => void;
  onCatalog?: (entries: PresetEntry[], progress: PresetCatalogProgress) => void;
  isCancelled: () => boolean;
}

/**
 * Owns engine + preset rotation for a visualizer surface. Two switching
 * modes: a plain timer, or "auto" — a new preset (matched to the current
 * energy level) at detected energy breaks, bounded by min/max dwell times.
 */
export class VisualizerController {
  #engine: VisualizerEngine;
  #rotation: PresetRotation;
  #callbacks: ControllerCallbacks;
  #cycle: CycleSetting = DEFAULT_CYCLE;
  #blendSeconds = DEFAULT_BLEND;
  #locked = false;
  #level: EnergyLevel = "medium";
  #sinceSwitchS = 0;
  #timer: ReturnType<typeof setInterval> | undefined;
  #destroyed = false;
  // Preset preferences / quality (persisted; see the *_KEY constants).
  #favorites = new Set<string>();
  #blocked = new Set<string>();
  #quarantined = new Set<string>();
  #favoritesOnly = false;
  #sensitivity = DEFAULT_SENSITIVITY;
  #quality: VisualizerQuality = DEFAULT_QUALITY;
  // Online presets: the persisted opt-in, and whether one is on screen now.
  #remoteOptIn = false;
  #currentRemote = false;
  // Beat-aligned switching: a decided switch waits for the next onset.
  #pendingSwitch = false;
  #pendingWaitS = 0;
  #loadGeneration = 0;
  #loadAbort: AbortController | undefined;
  #loadTimeout: ReturnType<typeof setTimeout> | undefined;
  #timedOutGeneration = 0;

  private constructor(
    engine: VisualizerEngine,
    rotation: PresetRotation,
    callbacks: ControllerCallbacks,
  ) {
    this.#engine = engine;
    this.#rotation = rotation;
    this.#callbacks = callbacks;
  }

  static async create(
    canvas: HTMLCanvasElement,
    callbacks: ControllerCallbacks,
  ): Promise<VisualizerController | null> {
    const engine = await VisualizerEngine.create(
      canvas,
      callbacks.onError,
      callbacks.isCancelled,
    );
    if (!engine) return null;
    let controller: VisualizerController | undefined;
    try {
      const initialPack = await loadInitialPresetPack();
      const rotation = new PresetRotation(initialPack.entries);
      if (callbacks.isCancelled()) {
        await engine.destroy();
        return null;
      }
      const created = new VisualizerController(engine, rotation, callbacks);
      controller = created;
      engine.setEnergyListener((state) => created.#onEnergy(state));
      // Apply persisted preset prefs + quality before the first pick.
      created.#favorites = loadNameSet(FAVORITES_KEY);
      created.#blocked = loadNameSet(BLOCKED_KEY);
      created.#quarantined = loadNameSet(QUARANTINED_KEY);
      created.#favoritesOnly = localStorage.getItem(FAVORITES_ONLY_KEY) === "true";
      created.#remoteOptIn = loadRemotePresetsSetting();
      rotation.setFavorites(created.#favorites);
      rotation.setBlocked(created.#blocked);
      rotation.setQuarantined(created.#quarantined);
      rotation.setFavoritesOnly(created.#favoritesOnly);
      rotation.setRemoteEnabled(created.#remoteOptIn);
      created.setSensitivity(loadSensitivitySetting(), false);
      created.setQuality(loadQualitySetting(), false);
      created.nextPreset(0);
      created.setBlend(loadBlendSetting(), false);
      created.setCycle(loadCycleSetting());
      callbacks.onCatalog?.(rotation.entries(), {
        loadedPacks: 1,
        totalPacks: PRESET_PACK_COUNT,
        failedPacks: [],
      });
      // Optional chunks load concurrently after the first preset is already on
      // screen. The browser receives a fresh immutable snapshot per completed pack.
      void loadAdditionalPresetPacks((pack, progress) => {
        if (created.#destroyed || callbacks.isCancelled()) return;
        rotation.addEntries(pack.entries);
        callbacks.onCatalog?.(rotation.entries(), progress);
      });
      return created;
    } catch (error) {
      // The engine already owns an AudioContext and the PCM subscription (and
      // the controller its cycle timer): a failed setup must release them.
      await (controller ? controller.destroy() : engine.destroy()).catch(() => {});
      throw error;
    }
  }

  get cycle(): CycleSetting {
    return this.#cycle;
  }

  get blend(): number {
    return this.#blendSeconds;
  }

  /// `persist: false` when merely loading the saved value or for transient
  /// consumers (the now-playing backdrop) that must not overwrite it.
  setBlend(seconds: number, persist = true) {
    this.#blendSeconds = Math.max(0, seconds);
    if (persist) localStorage.setItem(BLEND_KEY, String(this.#blendSeconds));
  }

  get locked(): boolean {
    return this.#locked;
  }

  setLocked(locked: boolean) {
    this.#locked = locked;
    if (locked) {
      this.#pendingSwitch = false;
      this.#pendingWaitS = 0;
    }
  }

  // --- Preset browser / favorites (B2) ---
  presetNames(): string[] {
    return this.#rotation.names();
  }
  presetEntries(): PresetEntry[] {
    return this.#rotation.entries();
  }
  get favoritesOnly(): boolean {
    return this.#favoritesOnly;
  }
  isFavorite(name: string): boolean {
    return this.#favorites.has(name);
  }
  isBlocked(name: string): boolean {
    return this.#blocked.has(name);
  }
  favoriteNames(): string[] {
    return [...this.#favorites];
  }
  blockedNames(): string[] {
    return [...this.#blocked];
  }
  quarantinedNames(): string[] {
    return [...this.#quarantined];
  }

  /** Load a specific preset by name (from the browser). */
  selectPreset(name: string) {
    const entry = this.#rotation.entryByName(name);
    if (!entry || (entry.sourceUrl && !this.#remoteAllowed())) return;
    void this.#selectEntry(entry);
  }

  async #selectEntry(entry: PresetEntry) {
    const load = this.#beginLoad();
    try {
      const result = await this.#loadEntry(entry, this.#blendSeconds, load);
      if (result === "retry") await this.#pickNext(0, load);
    } finally {
      this.#finishLoad(load.generation);
    }
  }

  toggleFavorite(name: string) {
    if (this.#favorites.has(name)) this.#favorites.delete(name);
    else this.#favorites.add(name);
    localStorage.setItem(FAVORITES_KEY, JSON.stringify([...this.#favorites]));
    this.#rotation.setFavorites(this.#favorites);
  }

  toggleBlocked(name: string) {
    if (this.#blocked.has(name)) this.#blocked.delete(name);
    else this.#blocked.add(name);
    localStorage.setItem(BLOCKED_KEY, JSON.stringify([...this.#blocked]));
    this.#rotation.setBlocked(this.#blocked);
  }

  setFavoritesOnly(on: boolean) {
    this.#favoritesOnly = on;
    localStorage.setItem(FAVORITES_ONLY_KEY, String(on));
    this.#rotation.setFavoritesOnly(on);
  }

  get remotePresetsEnabled(): boolean {
    return this.#remoteOptIn;
  }

  /**
   * The user's opt-in for online presets; returns the effective value.
   * `persist: false` mirrors a change another window already stored. An
   * explicit change also lifts the session-only offline/timeout disable, and
   * opting out moves off a remote preset that is still rendering.
   */
  setRemotePresetsEnabled(on: boolean, persist = true): boolean {
    this.#remoteOptIn = persist ? saveRemotePresetsSetting(on) : on;
    this.#rotation.setRemoteEnabled(this.#remoteOptIn);
    if (!this.#remoteOptIn && this.#currentRemote) this.#leaveRemotePreset();
    return this.#remoteOptIn;
  }

  /**
   * Opting out must take a remote preset (compiled code) off screen even when
   * the rotation has nothing to offer — favorites-only with only online
   * favorites, or every local preset blocked. Then any local preset stands
   * in, and a blank preset as the last resort.
   */
  #leaveRemotePreset() {
    if (this.#destroyed) return;
    this.#pendingSwitch = false;
    this.#pendingWaitS = 0;
    const load = this.#beginLoad();
    const superseded = () =>
      !this.#currentRemote || load.generation !== this.#loadGeneration || this.#destroyed;
    void (async () => {
      await this.#pickNext(this.#blendSeconds, load);
      // Bounded like #pickNext: a failing entry is quarantined, so the next
      // fallback is a different one.
      const attempts = Math.min(this.#rotation.size, 50);
      for (let attempt = 0; attempt < attempts && !superseded(); attempt++) {
        const entry = this.#rotation.localFallback();
        if (!entry) break;
        if ((await this.#loadEntry(entry, 0, load)) === "stale") return;
      }
      if (superseded()) return;
      try {
        this.#engine.loadPreset(blankPreset(), 0);
        this.#currentRemote = false;
        this.#sinceSwitchS = 0;
        this.#callbacks.onPreset("");
      } catch (error) {
        console.warn("blank preset failed to load", error);
        this.#callbacks.onError(error instanceof Error ? error.message : String(error));
      }
    })().finally(() => this.#finishLoad(load.generation));
  }

  /**
   * The persisted opt-in is authoritative — another window (the visualizer
   * route, while this is the projector) may have changed it. Syncs rotation
   * only on a change, so a session-only offline disable stays in place.
   */
  #remoteAllowed(): boolean {
    const on = loadRemotePresetsSetting();
    if (on !== this.#remoteOptIn) {
      this.#remoteOptIn = on;
      this.#rotation.setRemoteEnabled(on);
    }
    return on;
  }

  /** Portable, data-only backup. No preset code is evaluated on import. */
  exportPreferences(): string {
    return JSON.stringify(
      {
        format: "jellysic-visualizer-preferences",
        version: 1,
        exportedAt: new Date().toISOString(),
        favorites: [...this.#favorites],
        blocked: [...this.#blocked],
      },
      null,
      2,
    );
  }

  importPreferences(raw: string) {
    if (raw.length > 2_000_000) throw new Error(m.visualizer_backup_error_too_large());
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") throw new Error(m.visualizer_backup_error_invalid());
    const backup = value as Record<string, unknown>;
    if (backup.format !== "jellysic-visualizer-preferences" || backup.version !== 1) {
      throw new Error(m.visualizer_backup_error_unsupported());
    }
    const readNames = (field: unknown) => {
      if (!Array.isArray(field) || field.some((name) => typeof name !== "string")) {
        throw new Error(m.visualizer_backup_error_entries());
      }
      return new Set((field as string[]).filter((name) => name.length <= 500));
    };
    this.#favorites = readNames(backup.favorites);
    this.#blocked = readNames(backup.blocked);
    localStorage.setItem(FAVORITES_KEY, JSON.stringify([...this.#favorites]));
    localStorage.setItem(BLOCKED_KEY, JSON.stringify([...this.#blocked]));
    this.#rotation.setFavorites(this.#favorites);
    this.#rotation.setBlocked(this.#blocked);
  }

  clearQuarantine() {
    this.#quarantined.clear();
    localStorage.removeItem(QUARANTINED_KEY);
    this.#rotation.setQuarantined(this.#quarantined);
  }

  // --- Quality / reactivity (B3) ---
  get sensitivity(): number {
    return this.#sensitivity;
  }
  get quality(): VisualizerQuality {
    return this.#quality;
  }

  setSensitivity(multiplier: number, persist = true) {
    this.#sensitivity = multiplier;
    this.#engine.setSensitivity(multiplier);
    if (persist) localStorage.setItem(SENSITIVITY_KEY, String(multiplier));
  }

  setQuality(quality: VisualizerQuality, persist = true) {
    this.#quality = quality;
    const q = QUALITY_SETTINGS[quality];
    this.#engine.setQuality(q.mesh[0], q.mesh[1], q.aa, q.dprCap);
    if (persist) localStorage.setItem(QUALITY_KEY, quality);
  }

  /** PNG data URL of the current frame (screenshot), or null. */
  capture(): string | null {
    return this.#engine.capture();
  }

  /// `persist: false` for transient consumers (the now-playing backdrop)
  /// that must not overwrite the user's saved visualizer setting.
  setCycle(cycle: CycleSetting, persist = true) {
    this.#cycle = cycle;
    this.#pendingSwitch = false;
    this.#pendingWaitS = 0;
    if (persist) localStorage.setItem(STORAGE_KEY, String(cycle));
    clearInterval(this.#timer);
    if (cycle === "auto") {
      // Fallback tick: enforces MAX_DWELL even while paused (no PCM frames),
      // and flushes a beat-aligned switch if no onset arrives in time.
      this.#timer = setInterval(() => {
        this.#sinceSwitchS += 1;
        if (this.#locked) return;
        if (this.#pendingSwitch) {
          this.#pendingWaitS += 1;
          if (this.#pendingWaitS >= BEAT_ALIGN_WAIT_S) this.nextPreset();
        }
        if (this.#sinceSwitchS >= MAX_DWELL_S) this.#requestSwitch();
      }, 1000);
    } else if (cycle > 0) {
      this.#timer = setInterval(() => {
        if (!this.#locked) this.nextPreset();
      }, cycle * 1000);
    }
  }

  nextPreset(blendSeconds = this.#blendSeconds) {
    if (this.#destroyed) return;
    this.#pendingSwitch = false;
    this.#pendingWaitS = 0;
    const load = this.#beginLoad();
    void this.#pickNext(blendSeconds, load).finally(() => this.#finishLoad(load.generation));
  }

  async #pickNext(
    blendSeconds: number,
    load: { generation: number; signal: AbortSignal },
  ) {
    this.#remoteAllowed(); // rotation must reflect the current opt-in
    const preferred = this.#cycle === "auto" ? this.#level : undefined;
    // A malformed preset is quarantined and skipped immediately. Bound the
    // loop so a damaged catalog can never lock up the UI thread.
    const attempts = Math.min(this.#rotation.size, 50);
    for (let attempt = 0; attempt < attempts; attempt++) {
      const entry = this.#rotation.next(preferred);
      if (!entry) return;
      const result = await this.#loadEntry(entry, blendSeconds, load);
      if (result === "loaded" || result === "stale") return;
      blendSeconds = 0;
    }
  }

  #beginLoad() {
    this.#loadGeneration += 1;
    this.#loadAbort?.abort();
    clearTimeout(this.#loadTimeout);
    this.#loadAbort = new AbortController();
    const generation = this.#loadGeneration;
    this.#loadTimeout = setTimeout(() => {
      if (generation === this.#loadGeneration) {
        this.#timedOutGeneration = generation;
        this.#loadAbort?.abort();
      }
    }, 15_000);
    return { generation, signal: this.#loadAbort.signal };
  }

  #finishLoad(generation: number) {
    if (generation !== this.#loadGeneration) return;
    clearTimeout(this.#loadTimeout);
    this.#loadTimeout = undefined;
    if (this.#timedOutGeneration === generation) this.#timedOutGeneration = 0;
  }

  async #loadEntry(
    entry: PresetEntry,
    blendSeconds: number,
    load: { generation: number; signal: AbortSignal },
  ): Promise<"loaded" | "retry" | "stale"> {
    // Every load path ends here: a remote preset needs the opt-in, checked
    // again after the download in case it was withdrawn meanwhile.
    if (entry.sourceUrl && !this.#remoteAllowed()) return "retry";
    try {
      const preset = await resolvePreset(entry, {
        allowRemote: this.#remoteOptIn,
        signal: load.signal,
      });
      if (load.generation !== this.#loadGeneration || this.#destroyed) return "stale";
      if (entry.sourceUrl && !this.#remoteAllowed()) return "retry";
      this.#engine.loadPreset(preset, blendSeconds);
      this.#currentRemote = Boolean(entry.sourceUrl);
      this.#sinceSwitchS = 0;
      this.#pendingSwitch = false;
      this.#callbacks.onPreset(entry.name);
      return "loaded";
    } catch (error) {
      if (load.generation !== this.#loadGeneration || this.#destroyed) {
        return "stale";
      }
      if (load.signal.aborted) {
        if (this.#timedOutGeneration !== load.generation) return "stale";
        this.#rotation.setRemoteEnabled(false);
        this.#callbacks.onError(m.visualizer_error_weekly_timeout());
        return "retry";
      }
      console.warn(`preset ${entry.name} failed to load`, error);
      if (error instanceof PresetFetchError && !error.permanent) {
        // A network outage must not permanently blacklist hundreds of remote
        // entries. Disable them for this session and continue with local packs.
        this.#rotation.setRemoteEnabled(false);
        this.#callbacks.onError(m.visualizer_error_weekly_offline());
        return "retry";
      }
      this.#quarantined.add(entry.name);
      localStorage.setItem(QUARANTINED_KEY, JSON.stringify([...this.#quarantined]));
      this.#rotation.setQuarantined(this.#quarantined);
      this.#callbacks.onError(
        m.visualizer_error_preset_skipped({
          name: entry.name,
          message: error instanceof Error ? error.message : String(error),
        }),
      );
      return "retry";
    }
  }

  /** Decide to switch, but let #onEnergy land it on the next beat. */
  #requestSwitch() {
    // Do not restart the fallback countdown on every timer tick once a
    // switch is already pending; otherwise a track with no detected beats can
    // remain on the same preset forever after MAX_DWELL_S.
    if (this.#locked || this.#destroyed || this.#pendingSwitch) return;
    this.#pendingSwitch = true;
    this.#pendingWaitS = 0;
  }

  /** Track changed: flash the title, and a new preset (unless locked). */
  onTrackChange(title?: string) {
    if (title) this.#engine.songTitle(title);
    if (!this.#locked) this.nextPreset();
  }

  setSize(width: number, height: number) {
    this.#engine.setSize(width, height);
  }

  #onEnergy(state: EnergyState) {
    this.#level = state.level;
    if (this.#cycle !== "auto" || this.#locked) return;
    // A musical break past the min dwell decides a switch...
    if (state.breakDetected && this.#sinceSwitchS >= MIN_DWELL_S) {
      this.#requestSwitch();
    }
    // ...which then lands on the next detected beat for a musical cut.
    if (this.#pendingSwitch && state.beatDetected) {
      this.nextPreset();
    }
  }

  async destroy() {
    this.#destroyed = true;
    this.#loadGeneration += 1;
    this.#loadAbort?.abort();
    clearTimeout(this.#loadTimeout);
    clearInterval(this.#timer);
    await this.#engine.destroy();
  }
}
