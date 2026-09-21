// User-defined equalizer presets. A frontend preference like theme, layout
// and smart views, so it lives in localStorage — not in the Rust store,
// which only holds what the audio path itself needs (the active gains).

export interface EqPreset {
  name: string;
  /** One gain per band in dB (see BAND_LABELS in settings), -12..+12. */
  gains: number[];
}

const EQ_BANDS = 10;
const EQ_MIN_DB = -12;
const EQ_MAX_DB = 12;
export const MAX_CUSTOM_PRESETS = 20;
export const MAX_PRESET_NAME = 40;

const STORAGE_KEY = "jellysic.eq.presets.v1";

/** Ten finite gains, clamped to the slider range and snapped to its 0.5 dB
 *  steps; null for anything else. */
export function sanitizeGains(gains: unknown): number[] | null {
  if (!Array.isArray(gains) || gains.length !== EQ_BANDS) return null;
  const out: number[] = [];
  for (const g of gains) {
    if (typeof g !== "number" || !Number.isFinite(g)) return null;
    out.push(Math.round(Math.min(EQ_MAX_DB, Math.max(EQ_MIN_DB, g)) * 2) / 2);
  }
  return out;
}

export function normalizeName(name: string): string {
  return name.trim().replace(/\s+/g, " ").slice(0, MAX_PRESET_NAME);
}

const sameName = (a: string, b: string) => a.toLocaleLowerCase() === b.toLocaleLowerCase();

/** True when two gain sets are the same preset (to the slider's step). */
export function sameGains(a: readonly number[], b: readonly number[]): boolean {
  return a.length === b.length && a.every((g, i) => Math.abs(g - b[i]) < 0.25);
}

/** The stored presets; broken entries and duplicate names are dropped, so
 *  a hand-edited or old value can never break the settings page. */
export function loadCustomPresets(storage: Storage = localStorage): EqPreset[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(storage.getItem(STORAGE_KEY) ?? "[]");
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const presets: EqPreset[] = [];
  for (const entry of parsed) {
    if (!entry || typeof entry !== "object") continue;
    const { name, gains } = entry as { name?: unknown; gains?: unknown };
    if (typeof name !== "string") continue;
    const clean = normalizeName(name);
    const cleanGains = sanitizeGains(gains);
    if (!clean || !cleanGains || presets.some((p) => sameName(p.name, clean))) continue;
    presets.push({ name: clean, gains: cleanGains });
    if (presets.length >= MAX_CUSTOM_PRESETS) break;
  }
  return presets;
}

export function persistCustomPresets(presets: EqPreset[], storage: Storage = localStorage): void {
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(presets));
  } catch {
    // Storage full or unavailable: the presets stay for this session.
  }
}

type SaveResult =
  | { ok: true; presets: EqPreset[]; replaced: boolean }
  | { ok: false; reason: "name" | "full" };

/** Save `gains` under `name`; the same name (any case) is replaced. */
export function saveCustomPreset(presets: EqPreset[], name: string, gains: number[]): SaveResult {
  const clean = normalizeName(name);
  const cleanGains = sanitizeGains(gains);
  if (!clean || !cleanGains) return { ok: false, reason: "name" };
  const existing = presets.findIndex((p) => sameName(p.name, clean));
  if (existing >= 0) {
    const next = [...presets];
    next[existing] = { name: clean, gains: cleanGains };
    return { ok: true, presets: next, replaced: true };
  }
  if (presets.length >= MAX_CUSTOM_PRESETS) return { ok: false, reason: "full" };
  return { ok: true, presets: [...presets, { name: clean, gains: cleanGains }], replaced: false };
}

export function deleteCustomPreset(presets: EqPreset[], name: string): EqPreset[] {
  return presets.filter((p) => !sameName(p.name, name));
}
