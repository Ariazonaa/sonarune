import { VisualizerEngine, type EnergyLevel, type EnergyState } from "./engine";
import { m } from "#lib/paraglide/messages.js";
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

const STORAGE_KEY = "sonarune.vis.cycle";
const BLEND_KEY = "sonarune.vis.blend";
const FAVORITES_KEY = "sonarune.vis.favorites";
const BLOCKED_KEY = "sonarune.vis.blocked";
const QUARANTINED_KEY = "sonarune.vis.quarantined";
const FAVORITES_ONLY_KEY = "sonarune.vis.favoritesOnly";
const SENSITIVITY_KEY = "sonarune.vis.sensitivity";
const QUALITY_KEY = "sonarune.vis.quality";
/** Opt-in for the online Butterchurn Weekly presets (see `PresetRotation`). */
export const REMOTE_PRESETS_KEY = "sonarune.vis.remotePresets";
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

/** Stores the choice, also while no controller runs — the next one starts from it. */
export function saveCycleSetting(cycle: CycleSetting) {
  localStorage.setItem(STORAGE_KEY, String(cycle));
}

/** Preset transition time in seconds (0 = hard cut). */
export function loadBlendSetting(): number {
  const raw = localStorage.getItem(BLEND_KEY);
  if (raw === null) return DEFAULT_BLEND;
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : DEFAULT_BLEND;
}

/** See `saveCycleSetting`. */
export function saveBlendSetting(seconds: number) {
  localStorage.setItem(BLEND_KEY, String(Math.max(0, seconds)));
}

function loadFavoritesOnlySetting(): boolean {
  return localStorage.getItem(FAVORITES_ONLY_KEY) === "true";
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
  /** The visuals cannot go on (the graphics context is gone, rendering failed). */
  onError: (message: string) => void;
  /**
   * Something failed and the visualizer carried on: a preset was skipped,
   * online presets are unavailable for now. Goes to `onError` if not given.
   */
  onNotice?: (message: string) => void;
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
  /** Auto mode's one-second bookkeeping tick. */
  #timer: ReturnType<typeof setInterval> | undefined;
  /** Fixed cycle: the next switch, due `cycle` seconds after the last one. */
  #cycleTimer: ReturnType<typeof setTimeout> | undefined;
  #destroyed = false;
  /** A preset made it onto the screen since create. */
  #hasPreset = false;
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
  /**
   * A switch is under way (#beginLoad until its #finishLoad). Automatic
   * switching waits for it: a new load would abort this one, and an online
   * preset can take seconds to download.
   */
  #loading = false;

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
      created.#favoritesOnly = loadFavoritesOnlySetting();
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
        // The first pick only saw the base pack. With favorites-only and no
        // favorite in it (or every base preset blocked) it found nothing,
        // and nothing would try again before the next switch — up to a dwell
        // time, a whole cycle, or the next track. Try with what just arrived.
        if (!created.#hasPreset && !created.#loading) created.nextPreset(0);
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
    if (persist) saveBlendSetting(this.#blendSeconds);
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
    if (this.#halted()) return;
    const entry = this.#rotation.entryByName(name);
    if (!entry || (entry.sourceUrl && !this.#remoteAllowed())) return;
    void this.#selectEntry(entry);
  }

  async #selectEntry(entry: PresetEntry) {
    // The user's pick replaces a switch that was waiting for a beat.
    this.#pendingSwitch = false;
    this.#pendingWaitS = 0;
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

  /**
   * Take over the preset preferences another window stored — the main
   * window's preset browser, while this controller runs the projector, which
   * has no browser of its own. `key` is the storage event's key; null (storage
   * cleared) re-reads all of them. Nothing is written back.
   */
  syncStoredPreferences(key: string | null) {
    const changed = (stored: string) => key === null || key === stored;
    if (changed(FAVORITES_KEY)) {
      this.#favorites = loadNameSet(FAVORITES_KEY);
      this.#rotation.setFavorites(this.#favorites);
    }
    if (changed(BLOCKED_KEY)) {
      this.#blocked = loadNameSet(BLOCKED_KEY);
      this.#rotation.setBlocked(this.#blocked);
    }
    if (changed(QUARANTINED_KEY)) {
      this.#quarantined = loadNameSet(QUARANTINED_KEY);
      this.#rotation.setQuarantined(this.#quarantined);
    }
    if (changed(FAVORITES_ONLY_KEY)) {
      this.#favoritesOnly = loadFavoritesOnlySetting();
      this.#rotation.setFavoritesOnly(this.#favoritesOnly);
    }
    if (changed(SENSITIVITY_KEY)) this.setSensitivity(loadSensitivitySetting(), false);
    if (changed(QUALITY_KEY)) this.setQuality(loadQualitySetting(), false);
    if (changed(BLEND_KEY)) this.setBlend(loadBlendSetting(), false);
    if (changed(STORAGE_KEY)) this.setCycle(loadCycleSetting(), false);
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
    if (this.#halted()) return;
    this.#pendingSwitch = false;
    this.#pendingWaitS = 0;
    const load = this.#beginLoad();
    const superseded = () =>
      !this.#currentRemote || load.generation !== this.#loadGeneration || this.#halted();
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
        this.#hasPreset = true;
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
        format: "sonarune-visualizer-preferences",
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
    // "jellysic-…" is what builds before the rename exported.
    const formats = ["sonarune-visualizer-preferences", "jellysic-visualizer-preferences"];
    if (!formats.includes(backup.format as string) || backup.version !== 1) {
      throw new Error(m.visualizer_backup_error_unsupported());
    }
    const readNames = (field: unknown) => {
      if (!Array.isArray(field) || field.some((name) => typeof name !== "string")) {
        throw new Error(m.visualizer_backup_error_entries());
      }
      return new Set((field as string[]).filter((name) => name.length <= 500));
    };
    // Both lists first: a backup rejected halfway must leave the current
    // favorites alone, or the next star would save the rejected ones.
    const favorites = readNames(backup.favorites);
    const blocked = readNames(backup.blocked);
    this.#favorites = favorites;
    this.#blocked = blocked;
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
    if (persist) saveCycleSetting(cycle);
    clearInterval(this.#timer);
    this.#timer = undefined;
    clearTimeout(this.#cycleTimer);
    this.#cycleTimer = undefined;
    if (cycle === "auto") {
      // Fallback tick: enforces MAX_DWELL even while paused (no PCM frames),
      // and flushes a beat-aligned switch if no onset arrives in time. A load
      // in flight is a switch already under way: asking for another would
      // abort it (see #loading).
      this.#timer = setInterval(() => {
        this.#sinceSwitchS += 1;
        if (this.#locked || this.#loading || this.#halted()) return;
        if (this.#pendingSwitch) {
          this.#pendingWaitS += 1;
          if (this.#pendingWaitS >= BEAT_ALIGN_WAIT_S) this.nextPreset();
        }
        if (this.#sinceSwitchS >= MAX_DWELL_S) this.#requestSwitch();
      }, 1000);
    } else if (cycle > 0) {
      this.#scheduleCycle();
    }
  }

  /**
   * Fixed cycle: the next switch comes `cycle` seconds after the last one,
   * re-armed by every load, so a preset picked by hand (or for a new track)
   * gets its full time instead of what was left of a free-running interval.
   */
  #scheduleCycle() {
    clearTimeout(this.#cycleTimer);
    this.#cycleTimer = undefined;
    const cycle = this.#cycle;
    if (cycle === "auto" || cycle <= 0 || this.#halted()) return;
    this.#cycleTimer = setTimeout(() => {
      this.#scheduleCycle();
      if (!this.#locked && !this.#loading) this.nextPreset();
    }, cycle * 1000);
  }

  /** Destroyed, or the graphics context is gone: nothing loads any more. */
  #halted(): boolean {
    return this.#destroyed || this.#engine.contextLost;
  }

  nextPreset(blendSeconds = this.#blendSeconds) {
    if (this.#halted()) return;
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
    this.#loading = true;
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
    this.#loading = false;
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
      if (load.generation !== this.#loadGeneration || this.#halted()) return "stale";
      if (entry.sourceUrl && !this.#remoteAllowed()) return "retry";
      this.#engine.loadPreset(preset, blendSeconds);
      this.#currentRemote = Boolean(entry.sourceUrl);
      this.#hasPreset = true;
      this.#sinceSwitchS = 0;
      this.#pendingSwitch = false;
      this.#scheduleCycle();
      this.#callbacks.onPreset(entry.name);
      return "loaded";
    } catch (error) {
      // A lost graphics context fails every load — butterchurn hands WebGL
      // the null shaders it gets back — which says nothing about the preset:
      // quarantining for it would empty the whole catalog for good.
      if (load.generation !== this.#loadGeneration || this.#halted()) {
        return "stale";
      }
      if (load.signal.aborted) {
        if (this.#timedOutGeneration !== load.generation) return "stale";
        this.#rotation.setRemoteEnabled(false);
        this.#notice(m.visualizer_error_weekly_timeout());
        return "retry";
      }
      console.warn(`preset ${entry.name} failed to load`, error);
      if (error instanceof PresetFetchError && !error.permanent) {
        // A network outage must not permanently blacklist hundreds of remote
        // entries. Disable them for this session and continue with local packs.
        this.#rotation.setRemoteEnabled(false);
        this.#notice(m.visualizer_error_weekly_offline());
        return "retry";
      }
      this.#quarantine(entry.name);
      this.#notice(
        m.visualizer_error_preset_skipped({
          name: entry.name,
          message: error instanceof Error ? error.message : String(error),
        }),
      );
      return "retry";
    }
  }

  /**
   * Exclude a broken preset for good. Other windows (the projector, the
   * now-playing backdrop) keep their own set: merge with what is stored
   * instead of overwriting their additions, and drop what a reset elsewhere
   * removed.
   */
  #quarantine(name: string) {
    const merged = loadNameSet(QUARANTINED_KEY);
    merged.add(name);
    this.#quarantined = merged;
    localStorage.setItem(QUARANTINED_KEY, JSON.stringify([...merged]));
    this.#rotation.setQuarantined(merged);
  }

  #notice(message: string) {
    (this.#callbacks.onNotice ?? this.#callbacks.onError)(message);
  }

  /** Decide to switch, but let #onEnergy land it on the next beat. */
  #requestSwitch() {
    // Do not restart the fallback countdown on every timer tick once a
    // switch is already pending; otherwise a track with no detected beats can
    // remain on the same preset forever after MAX_DWELL_S. A load in flight
    // is the switch itself.
    if (this.#locked || this.#destroyed || this.#pendingSwitch || this.#loading) return;
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
    // A load in flight is a switch already under way (see #loading).
    if (this.#cycle !== "auto" || this.#locked || this.#loading) return;
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
    this.#loading = false;
    this.#loadAbort?.abort();
    clearTimeout(this.#loadTimeout);
    clearInterval(this.#timer);
    clearTimeout(this.#cycleTimer);
    await this.#engine.destroy();
  }
}
