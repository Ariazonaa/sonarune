import type { EnergyLevel } from "./engine";
import { api } from "$lib/api";
import { m } from "$lib/paraglide/messages";

export type PresetComplexity = "light" | "medium" | "heavy";

export interface PresetEntry {
  name: string;
  preset: unknown;
  sourceUrl?: string;
  pack: string;
  author: string;
  intensity: EnergyLevel;
  complexity: PresetComplexity;
  /** Deterministic winner when two packs contain the same preset name. */
  priority: number;
}

interface PresetPackResult {
  id: string;
  label: string;
  entries: PresetEntry[];
}

export interface PresetCatalogProgress {
  loadedPacks: number;
  totalPacks: number;
  failedPacks: string[];
}

type PresetRecord = Record<string, unknown>;
type PresetSource = PresetRecord | { getPresets(): PresetRecord } | (() => unknown);
type PresetModule = {
  default?: PresetSource;
  getPresets?: () => PresetRecord;
};

interface PackDefinition {
  id: string;
  label: string;
  priority: number;
  loader: () => Promise<PresetModule>;
}

const CALM_WORDS = [
  "slow", "calm", "drift", "dream", "soft", "ambient", "ocean",
  "underwater", "aurora", "zen", "cloud", "moon", "float", "gentle",
  "lull", "still", "chill", "night",
];
const INTENSE_WORDS = [
  "acid", "fire", "flame", "storm", "chaos", "chaotic", "hard",
  "explos", "warp", "electric", "adrenal", "inferno", "burn", "frantic",
  "hyper", "blast", "rage", "speed", "strobe", "shred", "quake", "volcano",
];

const PACKS: PackDefinition[] = [
  {
    id: "extra",
    label: "Butterchurn Extra",
    priority: 20,
    loader: () => import("butterchurn-presets/lib/butterchurnPresetsExtra.min.js"),
  },
  {
    id: "extra2",
    label: "Butterchurn Extra 2",
    priority: 30,
    loader: () => import("butterchurn-presets/lib/butterchurnPresetsExtra2.min.js"),
  },
  {
    id: "md1",
    label: "MilkDrop 1",
    priority: 40,
    loader: () => import("butterchurn-presets/lib/butterchurnPresetsMD1.min.js"),
  },
  {
    id: "minimal",
    label: "Butterchurn Minimal",
    priority: 10,
    loader: () => import("butterchurn-presets/lib/butterchurnPresetsMinimal.min.js"),
  },
  {
    id: "non-minimal",
    label: "Butterchurn Complete",
    priority: 100,
    loader: () => import("butterchurn-presets/lib/butterchurnPresetsNonMinimal.min.js"),
  },
  {
    id: "weekly",
    label: "Butterchurn Weekly",
    priority: 50,
    loader: () => import("butterchurn-presets-weekly"),
  },
];

export const PRESET_PACK_COUNT = PACKS.length + 1;

/** The package's URL prefix; the file name after it is what Rust fetches. */
const WEEKLY_ORIGIN = "https://s3-us-east-2.amazonaws.com/butterchurn-presets/";
/** `MAX_PRESET_BYTES` in commands_visualizer.rs (UTF-8 never has more chars than bytes). */
const MAX_REMOTE_PRESET_CHARS = 2 * 1024 * 1024;
const REMOTE_CACHE_SIZE = 64;
const remoteCache = new Map<string, unknown>();

export class PresetFetchError extends Error {
  constructor(message: string, readonly permanent: boolean) {
    super(message);
    this.name = "PresetFetchError";
  }
}

/** The `<32 hex>.json` file name of a Weekly URL, or null for anything else. */
function weeklyFile(value: unknown): string | null {
  if (typeof value !== "string" || !value.startsWith(WEEKLY_ORIGIN)) return null;
  const tail = value.slice(WEEKLY_ORIGIN.length);
  return /^[a-f0-9]{32}\.json$/.test(tail) ? tail : null;
}

function isWeeklyUrl(value: unknown): value is string {
  return weeklyFile(value) !== null;
}

function throwIfAborted(signal?: AbortSignal) {
  if (signal?.aborted) throw signal.reason ?? new DOMException("Aborted", "AbortError");
}

/**
 * Map a `fetch_weekly_preset` rejection (`preset:<code>[:<detail>]`, see
 * commands_visualizer.rs) to a PresetFetchError. Permanent — the entry gets
 * quarantined — when retrying cannot help: the file is not pinned (unavailable),
 * its content no longer matches the pin, the server refuses it (4xx), or the
 * body is unusable. Network failures, timeouts, 5xx and anything unexpected
 * only pause online presets.
 */
function commandError(error: unknown): PresetFetchError {
  const text = error instanceof Error ? error.message : String(error);
  const match = /^preset:([a-z-]+)(?::([\s\S]*))?$/.exec(text);
  const detail = match?.[2] ?? "";
  switch (match?.[1]) {
    case "invalid-name":
    case "not-listed":
      return new PresetFetchError(m.visualizer_preset_error_unlisted(), true);
    case "changed":
      return new PresetFetchError(m.visualizer_preset_error_changed(), true);
    case "http": {
      const status = Number(detail);
      return new PresetFetchError(
        m.visualizer_preset_error_http({ status }),
        status >= 400 && status < 500,
      );
    }
    case "too-large":
      return new PresetFetchError(m.visualizer_preset_error_too_large(), true);
    case "undecodable":
      return new PresetFetchError(m.visualizer_preset_error_encoding(), true);
    case "network":
      return new PresetFetchError(m.visualizer_preset_error_download({ message: detail }), false);
    default:
      return new PresetFetchError(m.visualizer_preset_error_download({ message: text }), false);
  }
}

/**
 * Resolve a remote Weekly entry without ever accepting an arbitrary URL.
 * Remote entries additionally need `allowRemote` — the user's explicit opt-in
 * (see `PresetRotation`), since butterchurn turns the file into running code.
 * The download happens in Rust (`fetch_weekly_preset`), which only serves
 * files whose content matches their SHA-256 pin in `weekly_presets.json`.
 */
export async function resolvePreset(
  entry: PresetEntry,
  { allowRemote, signal }: { allowRemote: boolean; signal?: AbortSignal },
): Promise<unknown> {
  if (!entry.sourceUrl) return entry.preset;
  // Not permanent: a withdrawn opt-in must never quarantine the entry.
  if (!allowRemote) throw new PresetFetchError(m.visualizer_preset_error_remote_disabled(), false);
  const file = weeklyFile(entry.sourceUrl);
  if (!file) throw new PresetFetchError(m.visualizer_preset_error_untrusted(), true);

  const cached = remoteCache.get(file);
  if (cached !== undefined) {
    remoteCache.delete(file);
    remoteCache.set(file, cached);
    return cached;
  }

  throwIfAborted(signal);
  let raw: string;
  try {
    raw = await api.fetchWeeklyPreset(file);
  } catch (error) {
    // The command cannot be cancelled; an aborted load just drops its outcome.
    throwIfAborted(signal);
    throw commandError(error);
  }
  throwIfAborted(signal);
  if (typeof raw !== "string") {
    throw new PresetFetchError(m.visualizer_preset_error_schema(), true);
  }
  if (raw.length > MAX_REMOTE_PRESET_CHARS) {
    throw new PresetFetchError(m.visualizer_preset_error_too_large(), true);
  }

  let preset: unknown;
  try {
    preset = JSON.parse(raw);
  } catch {
    throw new PresetFetchError(m.visualizer_preset_error_json(), true);
  }
  if (!preset || typeof preset !== "object" || !("baseVals" in preset)) {
    throw new PresetFetchError(m.visualizer_preset_error_schema(), true);
  }
  remoteCache.set(file, preset);
  while (remoteCache.size > REMOTE_CACHE_SIZE) {
    const oldest = remoteCache.keys().next().value;
    if (oldest === undefined) break;
    remoteCache.delete(oldest);
  }
  return preset;
}

/**
 * An empty, data-only preset (butterchurn's defaults, no equations or
 * shaders): the last resort when a remote preset has to leave the screen and
 * no local preset can take its place.
 */
export function blankPreset(): Record<string, unknown> {
  return {
    baseVals: {},
    shapes: [],
    waves: [],
    init_eqs_str: "",
    frame_eqs_str: "",
    pixel_eqs_str: "",
    warp: "",
    comp: "",
  };
}

function classifyIntensity(name: string): EnergyLevel {
  const lower = name.toLowerCase();
  if (INTENSE_WORDS.some((word) => lower.includes(word))) return "intense";
  if (CALM_WORDS.some((word) => lower.includes(word))) return "calm";
  return "medium";
}

function classifyComplexity(preset: unknown): PresetComplexity {
  // A deterministic proxy that works before a preset is rendered. It is not
  // a GPU benchmark, but it usefully separates small/simple definitions from
  // equation- and shape-heavy ones without stalling the render loop.
  let size = 0;
  try {
    size = JSON.stringify(preset)?.length ?? 0;
  } catch {
    return "heavy";
  }
  if (size < 4_000) return "light";
  if (size > 14_000) return "heavy";
  return "medium";
}

function extractAuthor(name: string): string {
  const cleaned = name.replace(/^\[[^\]]+\]\s*/, "").trim();
  const separator = cleaned.search(/\s[-–—]\s/);
  if (separator <= 0) return "Unknown";
  const author = cleaned.slice(0, separator).trim();
  return author.length <= 80 ? author : "Unknown";
}

/**
 * Pull the preset record out of a pack module.
 *
 * The packs are UMD bundles whose export is a **function** that carries
 * `getPresets` -- so `typeof` is "function", not "object". An object-only check
 * here silently yields `{}` for every pack, which the UI then reports as
 * "0 of 0 presets" while still counting the pack as loaded, and the visualizer
 * renders Butterchurn's blank preset: a grey waveform and nothing else.
 *
 * It only broke in production builds. Dev is served by esbuild's pre-bundle,
 * which hoists `getPresets` to a named export so the first branch hits;
 * Rollup's interop exposes `default` alone.
 */
export function unwrapPresets(module: PresetModule): PresetRecord {
  for (const candidate of [module, module.default]) {
    if (!candidate) continue;
    if (typeof candidate !== "object" && typeof candidate !== "function") continue;
    const get = (candidate as { getPresets?: () => PresetRecord }).getPresets;
    if (typeof get === "function") return get.call(candidate);
  }
  const value = module.default;
  return value && typeof value === "object" ? (value as PresetRecord) : {};
}

function makePack(
  id: string,
  label: string,
  priority: number,
  records: PresetRecord,
): PresetPackResult {
  return {
    id,
    label,
    entries: Object.entries(records)
      .filter(([, preset]) => typeof preset !== "string" || isWeeklyUrl(preset))
      .map(([name, preset]) => {
        const sourceUrl = isWeeklyUrl(preset) ? preset : undefined;
        return {
          name,
          preset: sourceUrl ? null : preset,
          sourceUrl,
          pack: label,
          author: extractAuthor(name),
          intensity: classifyIntensity(name),
          complexity: sourceUrl ? "medium" : classifyComplexity(preset),
          priority,
        };
      }),
  };
}

/** Load only the compact base pack so the first frame is never held up. */
export async function loadInitialPresetPack(): Promise<PresetPackResult> {
  const module = await import("butterchurn-presets");
  return makePack("base", "Butterchurn Base", 0, unwrapPresets(module));
}

/**
 * Load all optional packs concurrently. Each successful pack is delivered as
 * soon as it is ready; one broken chunk cannot prevent the others from loading.
 */
export async function loadAdditionalPresetPacks(
  onPack: (pack: PresetPackResult, progress: PresetCatalogProgress) => void,
): Promise<PresetCatalogProgress> {
  let loadedPacks = 1; // base is already available
  const totalPacks = PRESET_PACK_COUNT;
  const failedPacks: string[] = [];

  await Promise.all(
    PACKS.map(async (pack) => {
      try {
        const module = await pack.loader();
        loadedPacks += 1;
        onPack(makePack(pack.id, pack.label, pack.priority, unwrapPresets(module)), {
          loadedPacks,
          totalPacks,
          failedPacks: [...failedPacks],
        });
      } catch (error) {
        console.warn(`preset pack ${pack.id} failed to load`, error);
        failedPacks.push(pack.label);
        loadedPacks += 1;
        onPack(makePack(pack.id, pack.label, pack.priority, {}), {
          loadedPacks,
          totalPacks,
          failedPacks: [...failedPacks],
        });
      }
    }),
  );

  return { loadedPacks, totalPacks, failedPacks };
}

const RECENT_MEMORY = 10;

/** Shuffle-bag rotation that can grow while optional chunks are loading. */
export class PresetRotation {
  #entries: PresetEntry[] = [];
  #byName = new Map<string, number>();
  #bag: number[] = [];
  #recent: number[] = [];
  #favorites = new Set<string>();
  #blocked = new Set<string>();
  #quarantined = new Set<string>();
  #favoritesOnly = false;
  // Off by default. A remote preset is not data — butterchurn compiles the
  // `*_eqs_str` fields with `new Function`, so whoever serves the file runs
  // code in this window, which holds the full Tauri invoke surface. Rust
  // only serves files matching their SHA-256 pin (`weekly_presets.json`),
  // but a pin just freezes whatever the third party served when it was
  // taken — it does not make that code trustworthy. Local packs are
  // unaffected; enable this only behind an explicit user opt-in.
  #remoteEnabled = false;

  constructor(entries: PresetEntry[]) {
    this.addEntries(entries);
  }

  get size(): number {
    return this.#entries.length;
  }

  entries(): PresetEntry[] {
    return [...this.#entries];
  }

  names(): string[] {
    return this.#entries.map((entry) => entry.name);
  }

  entryByName(name: string): PresetEntry | undefined {
    const index = this.#byName.get(name);
    return index === undefined ? undefined : this.#entries[index];
  }

  addEntries(entries: PresetEntry[]): number {
    let added = 0;
    for (const entry of entries) {
      const index = this.#byName.get(entry.name);
      if (index === undefined) {
        this.#byName.set(entry.name, this.#entries.length);
        this.#entries.push(entry);
        added += 1;
      } else if (entry.priority > this.#entries[index].priority) {
        // Later/full packs intentionally replace reduced versions while the
        // stable list position and user preferences remain intact.
        this.#entries[index] = entry;
      }
    }
    if (entries.length > 0) this.#bag = [];
    return added;
  }

  setFavorites(names: Set<string>) {
    this.#favorites = names;
    this.#bag = [];
  }

  setBlocked(names: Set<string>) {
    this.#blocked = names;
    this.#bag = [];
  }

  setQuarantined(names: Set<string>) {
    this.#quarantined = names;
    this.#bag = [];
  }

  setFavoritesOnly(on: boolean) {
    this.#favoritesOnly = on;
    this.#bag = [];
  }

  setRemoteEnabled(on: boolean) {
    this.#remoteEnabled = on;
    this.#bag = [];
  }

  next(preferred?: EnergyLevel): PresetEntry | null {
    if (this.#entries.length === 0) return null;
    if (this.#bag.length === 0) this.#refill();
    if (this.#bag.length === 0) return null;

    let pick = this.#bag.length - 1;
    if (preferred) {
      for (let i = this.#bag.length - 1; i >= 0; i--) {
        if (this.#entries[this.#bag[i]].intensity === preferred) {
          pick = i;
          break;
        }
      }
    }
    const [index] = this.#bag.splice(pick, 1);
    this.#recent.push(index);
    if (this.#recent.length > RECENT_MEMORY) this.#recent.shift();
    return this.#entries[index];
  }

  /**
   * A local stand-in for when `next()` has nothing (favorites-only with only
   * online favorites, every local preset blocked) but a remote preset has to
   * leave the screen: any non-remote entry, ignoring favorites-only and the
   * recent memory, unblocked ones first. Quarantined entries stay out.
   */
  localFallback(): PresetEntry | null {
    let blocked: PresetEntry | null = null;
    for (const entry of this.#entries) {
      if (entry.sourceUrl || this.#quarantined.has(entry.name)) continue;
      if (!this.#blocked.has(entry.name)) return entry;
      blocked ??= entry;
    }
    return blocked;
  }

  #eligible(index: number): boolean {
    const name = this.#entries[index].name;
    if (!this.#remoteEnabled && this.#entries[index].sourceUrl) return false;
    if (this.#blocked.has(name) || this.#quarantined.has(name)) return false;
    if (this.#favoritesOnly && this.#favorites.size > 0 && !this.#favorites.has(name)) return false;
    return true;
  }

  #refill() {
    const recent = new Set(this.#recent);
    let candidates = this.#entries
      .map((_, index) => index)
      .filter((index) => this.#eligible(index) && !recent.has(index));
    if (candidates.length === 0) {
      candidates = this.#entries.map((_, index) => index).filter((index) => this.#eligible(index));
    }
    // Never relax the block/quarantine rules. An all-blocked catalog simply
    // pauses automatic rotation until the user changes the filters.
    for (let i = candidates.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [candidates[i], candidates[j]] = [candidates[j], candidates[i]];
    }
    this.#bag = candidates;
  }
}
