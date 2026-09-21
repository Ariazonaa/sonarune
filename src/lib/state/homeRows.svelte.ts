/**
 * Which rows the home page shows, and in which order.
 *
 * Kept in the browser like the other view preferences: it says nothing about
 * the library, only about this screen. A row is identified by the field it
 * reads from `HomeData`, so a stored order survives renaming a heading.
 */
export const HOME_ROWS = [
  "recentlyPlayed",
  "recentlyAdded",
  "mostPlayed",
  "forgottenFavorites",
] as const;

export type HomeRowId = (typeof HOME_ROWS)[number];

const STORAGE_KEY = "jellysic.homeRows.v1";

interface HomeRowPrefs {
  /** Every row, in the order they are shown. */
  order: HomeRowId[];
  hidden: HomeRowId[];
}

/**
 * Make a usable preference out of whatever is in storage.
 *
 * Unknown ids are dropped and duplicates collapse, but a **known row that is
 * missing is appended rather than treated as hidden**: a version that adds a
 * row must show it, or it would be invisible to everyone who ever touched
 * this setting, with nothing in the UI to explain why.
 */
export function cleanRows(value: unknown): HomeRowPrefs {
  const input = value && typeof value === "object" ? (value as Partial<HomeRowPrefs>) : {};
  const known = new Set<string>(HOME_ROWS);
  const seen = new Set<HomeRowId>();
  const order: HomeRowId[] = [];
  for (const id of Array.isArray(input.order) ? input.order : []) {
    if (typeof id === "string" && known.has(id) && !seen.has(id as HomeRowId)) {
      seen.add(id as HomeRowId);
      order.push(id as HomeRowId);
    }
  }
  for (const id of HOME_ROWS) if (!seen.has(id)) order.push(id);

  const hidden = (Array.isArray(input.hidden) ? input.hidden : []).filter(
    (id): id is HomeRowId => typeof id === "string" && known.has(id),
  );
  return { order, hidden: [...new Set(hidden)] };
}

/** `order` with `a` and `b` swapped; unchanged if either is missing. */
export function swapRows(order: HomeRowId[], a: HomeRowId, b: HomeRowId): HomeRowId[] {
  const i = order.indexOf(a);
  const j = order.indexOf(b);
  if (i < 0 || j < 0) return order;
  const next = [...order];
  next[i] = b;
  next[j] = a;
  return next;
}

/** `order` with `id` moved by `delta` places, clamped to the ends. */
export function moveRow(order: HomeRowId[], id: HomeRowId, delta: number): HomeRowId[] {
  const from = order.indexOf(id);
  if (from < 0) return order;
  const to = Math.min(order.length - 1, Math.max(0, from + delta));
  if (to === from) return order;
  const next = [...order];
  next.splice(from, 1);
  next.splice(to, 0, id);
  return next;
}

function load(): HomeRowPrefs {
  if (typeof localStorage === "undefined") return cleanRows(null);
  try {
    return cleanRows(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}"));
  } catch {
    return cleanRows(null);
  }
}

class HomeRowPreferences {
  #prefs = $state<HomeRowPrefs>(load());

  get order(): HomeRowId[] {
    return this.#prefs.order;
  }

  isHidden(id: HomeRowId): boolean {
    return this.#prefs.hidden.includes(id);
  }

  /** The rows to render, in order — hidden ones left out. */
  get visible(): HomeRowId[] {
    return this.#prefs.order.filter((id) => !this.isHidden(id));
  }

  toggle(id: HomeRowId) {
    const hidden = this.isHidden(id)
      ? this.#prefs.hidden.filter((other) => other !== id)
      : [...this.#prefs.hidden, id];
    this.#save({ ...this.#prefs, hidden });
  }

  /** Move within the whole list, hidden rows included — for the settings
   *  list, which shows them all. */
  move(id: HomeRowId, delta: number) {
    this.#save({ ...this.#prefs, order: moveRow(this.#prefs.order, id, delta) });
  }

  /**
   * Move past the next *visible* row — for the home page, where that is the
   * only movement anyone can see. Stepping one position in the full order
   * would swap with a hidden row and look like nothing happened.
   */
  moveAmongVisible(id: HomeRowId, delta: number) {
    const visible = this.visible;
    const to = visible.indexOf(id) + delta;
    if (visible.indexOf(id) < 0 || to < 0 || to >= visible.length) return;
    this.#save({ ...this.#prefs, order: swapRows(this.#prefs.order, id, visible[to]) });
  }

  reset() {
    this.#save(cleanRows(null));
  }

  get isDefault(): boolean {
    return (
      this.#prefs.hidden.length === 0 &&
      this.#prefs.order.every((id, index) => id === HOME_ROWS[index])
    );
  }

  #save(next: HomeRowPrefs) {
    this.#prefs = next;
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    }
  }
}

export const homeRows = new HomeRowPreferences();
