/**
 * Offset paging over lists that can change while they are paged.
 *
 * The browse lists ask the server for the next page at "rows listed so far".
 * A library scan that inserts or renames something before that offset shifts
 * every later page by a row, so a page can repeat rows already listed (and
 * skip one, which only a refresh brings back).
 */

export interface MergedPage<T> {
  items: T[];
  /** Where paging stops: the server's total, or the rows listed once a page
   *  brought nothing new. */
  total: number;
  /** Whether the page added any rows. */
  grew: boolean;
}

/**
 * Append a fetched page to the rows already listed. Rows that are already
 * there are dropped, since a repeated key crashes the keyed list. A page that
 * adds nothing ends the paging, whatever the server's total claims: otherwise
 * the scroll handler would ask for the same offset again and again.
 */
export function appendPage<T>(
  items: T[],
  page: T[],
  total: number,
  key: (item: T) => string,
): MergedPage<T> {
  const seen = new Set(items.map(key));
  const fresh = page.filter((item) => {
    const id = key(item);
    if (seen.has(id)) return false;
    seen.add(id);
    return true;
  });
  if (fresh.length === 0) return { items, total: items.length, grew: false };
  return { items: [...items, ...fresh], total, grew: true };
}

/**
 * Which rows to ask for next when some listed rows have moved on the server.
 * An edit made in the list (title, track or disc number) can give a row a new
 * place in the sort order while it keeps its place in the list. If it moved
 * past the last listed row, every server offset after its old place shifts by
 * one: the first row not listed yet now sits at the last listed offset, and a
 * page asked for at "rows listed" would skip it. So the page starts one row
 * earlier for each moved row and asks for that many more; `appendPage` drops
 * the rows it repeats.
 *
 * The caller forgets a moved row once a page brings it back, since it is then
 * where it belongs. One that did not move past the listed rows never comes
 * back and costs a repeated row per page until the list is reloaded.
 */
export function pageWindow(
  listed: number,
  pageSize: number,
  moved: number,
): { start: number; limit: number } {
  const back = Math.min(moved, listed);
  return { start: listed - back, limit: pageSize + back };
}

/** What `pageUntil` needs to know about a paged list. */
export interface PagedList {
  length(): number;
  hasMore(): boolean;
  /** The page fetch in flight, or null when there is none. */
  pending(): Promise<boolean> | null;
  /** Start the next page; resolves whether it added rows. */
  loadMore(): Promise<boolean>;
}

/**
 * Page a list in until row `index` exists, for the A–Z rail. A page already in
 * flight (scroll paging, an earlier jump) is waited for rather than taken as
 * the end of the list. `current` turning false (a later letter, or the list
 * being reset) stops the walk. Resolves whether the jump is still current, that
 * is whether the caller should scroll to its row.
 */
export async function pageUntil(
  index: number,
  list: PagedList,
  current: () => boolean,
): Promise<boolean> {
  while (current() && list.length() <= index && list.hasMore()) {
    // A failed page, or one that added nothing, is as far as this list goes.
    if (!(await (list.pending() ?? list.loadMore()))) break;
  }
  return current();
}
