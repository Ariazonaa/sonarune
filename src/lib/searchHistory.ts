import type { SessionInfo } from "#lib/types.js";

/**
 * Recent searches, stored per server and account like the smart views: on a
 * shared PC one account's searches are none of the next one's business.
 */
const STORAGE_PREFIX = "sonarune:searchHistory:";
/** Before 0.6.1 one list served every account. */
const SHARED_KEY = "sonarune:searchHistory";

export const SEARCH_HISTORY_MAX = 8;

function storageKey(info: SessionInfo): string {
  return `${STORAGE_PREFIX}${encodeURIComponent(info.serverUrl)}:${encodeURIComponent(info.userId)}`;
}

function parse(raw: string | null): string[] {
  try {
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
      .slice(0, SEARCH_HISTORY_MAX);
  } catch {
    return []; // corrupt entry — ignore
  }
}

export function loadSearchHistory(info: SessionInfo | null): string[] {
  if (!info) return [];
  try {
    const own = localStorage.getItem(storageKey(info));
    const shared = localStorage.getItem(SHARED_KEY);
    if (shared !== null) {
      // The old shared list goes to the first account that opens Search after
      // the update — as a rule the one that was signed in then — and to
      // nobody after that.
      localStorage.removeItem(SHARED_KEY);
      if (own === null) {
        const adopted = parse(shared);
        if (adopted.length > 0) localStorage.setItem(storageKey(info), JSON.stringify(adopted));
        return adopted;
      }
    }
    return parse(own);
  } catch {
    return []; // storage unavailable — history stays in memory
  }
}

export function saveSearchHistory(info: SessionInfo | null, entries: string[]): void {
  if (!info) return;
  try {
    localStorage.setItem(storageKey(info), JSON.stringify(entries));
  } catch {
    /* private mode / quota — history stays in-memory only */
  }
}

export function clearSearchHistory(info: SessionInfo | null): void {
  if (!info) return;
  try {
    localStorage.removeItem(storageKey(info));
  } catch {
    /* ignore */
  }
}

/** On sign-out: a shared list nobody adopted yet must not reach the next
 *  account either. */
export function dropSharedSearchHistory(): void {
  try {
    localStorage.removeItem(SHARED_KEY);
  } catch {
    /* ignore */
  }
}
