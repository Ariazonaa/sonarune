import type { SmartFilter } from "$lib/api/discovery";
import type { SessionInfo } from "$lib/types";

export interface SavedSmartView {
  id: string;
  name: string;
  filter: SmartFilter;
  createdAt: string;
  updatedAt: string;
}

const STORAGE_PREFIX = "jellysic:smart-views:v1:";

export const emptySmartFilter = (): SmartFilter => ({
  yearFrom: null,
  yearTo: null,
  genreIds: [],
  played: "all",
  favoriteOnly: false,
  minPlayCount: null,
  addedSince: null,
});

/** The backend's SmartFilter bounds (`validate_smart_filter`, i32 fields). */
const YEAR_MIN = 1;
const YEAR_MAX = 9999;
/** The backend refuses a year range wider than this. */
const YEAR_SPAN_MAX = 300;
const PLAY_COUNT_MAX = 2_147_483_647;

export function cloneSmartFilter(filter: SmartFilter): SmartFilter {
  return {
    yearFrom: wholeOrNull(filter.yearFrom, YEAR_MIN, YEAR_MAX),
    yearTo: wholeOrNull(filter.yearTo, YEAR_MIN, YEAR_MAX),
    genreIds: [...new Set(filter.genreIds.filter((id) => typeof id === "string" && id.length > 0))],
    played: ["all", "played", "unplayed"].includes(filter.played) ? filter.played : "all",
    favoriteOnly: Boolean(filter.favoriteOnly),
    minPlayCount: wholeOrNull(filter.minPlayCount, 0, PLAY_COUNT_MAX),
    addedSince: /^\d{4}-\d{2}-\d{2}$/.test(filter.addedSince ?? "") ? filter.addedSince : null,
  };
}

export type SmartYearProblem = "range" | "order" | "span";

/**
 * Why the year bounds can't be queried as entered, or null when they can.
 * "range": a year that isn't a whole number in 1..9999 — `cloneSmartFilter`
 * would quietly send a different one than the input shows. "order": the
 * start is after the end. "span": wider than the backend accepts.
 */
export function smartYearProblem(filter: SmartFilter): SmartYearProblem | null {
  const outOfRange = [filter.yearFrom, filter.yearTo].some(
    (year) => year != null && !(Number.isInteger(year) && year >= YEAR_MIN && year <= YEAR_MAX),
  );
  if (outOfRange) return "range";
  const { yearFrom, yearTo } = cloneSmartFilter(filter);
  if (yearFrom == null || yearTo == null) return null;
  if (yearFrom > yearTo) return "order";
  return yearTo - yearFrom > YEAR_SPAN_MAX ? "span" : null;
}

function smartViewStorageKey(info: SessionInfo): string {
  return `${STORAGE_PREFIX}${encodeURIComponent(info.serverUrl)}:${encodeURIComponent(info.userId)}`;
}

export function loadSmartViews(info: SessionInfo | null): SavedSmartView[] {
  if (!info || typeof localStorage === "undefined") return [];
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(smartViewStorageKey(info)) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed.flatMap((candidate): SavedSmartView[] => {
      if (!isRecord(candidate) || typeof candidate.id !== "string" || typeof candidate.name !== "string") {
        return [];
      }
      if (!isRecord(candidate.filter)) return [];
      const filter = candidate.filter as unknown as SmartFilter;
      if (!Array.isArray(filter.genreIds) || typeof filter.played !== "string") return [];
      const now = new Date().toISOString();
      return [{
        id: candidate.id,
        name: candidate.name,
        filter: cloneSmartFilter(filter),
        createdAt: typeof candidate.createdAt === "string" ? candidate.createdAt : now,
        updatedAt: typeof candidate.updatedAt === "string" ? candidate.updatedAt : now,
      }];
    });
  } catch {
    return [];
  }
}

export function persistSmartViews(info: SessionInfo | null, views: SavedSmartView[]): void {
  if (!info || typeof localStorage === "undefined") return;
  localStorage.setItem(smartViewStorageKey(info), JSON.stringify(views));
}

export function newSmartView(name: string, filter: SmartFilter): SavedSmartView {
  const now = new Date().toISOString();
  return {
    id: globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`,
    name: name.trim(),
    filter: cloneSmartFilter(filter),
    createdAt: now,
    updatedAt: now,
  };
}

/** A number input yields decimals and anything in f64 range; the backend
 *  deserializes into i32, so one stray value would fail every query. */
function wholeOrNull(value: number | null | undefined, min: number, max: number): number | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return Math.min(max, Math.max(min, Math.round(value)));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
