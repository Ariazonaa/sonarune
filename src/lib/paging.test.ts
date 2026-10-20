import { describe, expect, it } from "vitest";
import { appendPage, pageUntil, pageWindow, type PagedList } from "./paging";

const key = (row: { id: string }) => row.id;
const rows = (...ids: string[]) => ids.map((id) => ({ id }));
const ids = (list: { id: string }[]) => list.map(key);

describe("appendPage", () => {
  it("appends a page of new rows and keeps the server's total", () => {
    const merged = appendPage(rows("a", "b"), rows("c", "d"), 10, key);
    expect(ids(merged.items)).toEqual(["a", "b", "c", "d"]);
    expect(merged.total).toBe(10);
    expect(merged.grew).toBe(true);
  });

  it("drops rows that are already listed", () => {
    // Something inserted before the offset pushed "b" onto the next page; a
    // second "b" would crash the keyed list.
    const merged = appendPage(rows("a", "b"), rows("b", "c"), 10, key);
    expect(ids(merged.items)).toEqual(["a", "b", "c"]);
    expect(merged.grew).toBe(true);
  });

  it("drops a row the page itself repeats", () => {
    expect(ids(appendPage(rows("a"), rows("b", "b"), 10, key).items)).toEqual(["a", "b"]);
  });

  it("ends paging on a page that adds nothing, whatever the total claims", () => {
    // The list fell one row behind the server: the last offset only returns
    // a row that is already there. Keeping the server's total would ask for
    // that offset again and again.
    const listed = rows("a", "b");
    const merged = appendPage(listed, rows("b"), 3, key);
    expect(merged.grew).toBe(false);
    expect(merged.total).toBe(2);
    expect(merged.items).toBe(listed);
  });

  it("ends paging on an empty page, the first one included", () => {
    // A server whose count promises more than it pages out.
    expect(appendPage([], [], 500, key)).toEqual({ items: [], total: 0, grew: false });
  });
});

/** The server side of a list sorted by name and paged by offset, like Songs. */
type Song = { id: string; name: string };
const library = (count: number): Song[] =>
  Array.from({ length: count }, (_, i) => ({
    id: `s${i + 1}`,
    name: `song ${String(i + 1).padStart(2, "0")}`,
  }));
const serve = (songs: Song[], start: number, limit: number) => {
  const sorted = [...songs].sort((a, b) => a.name.localeCompare(b.name));
  return { items: sorted.slice(start, start + limit), total: sorted.length };
};
const rename = (songs: Song[], id: string, name: string) =>
  songs.map((song) => (song.id === id ? { ...song, name } : song));

/** Page the rest of the list in the way the Songs page does. */
function pageToEnd(songs: Song[], listed: Song[], moved: Set<string>, pageSize: number) {
  let items = listed;
  for (;;) {
    const { start, limit } = pageWindow(items.length, pageSize, moved.size);
    const page = serve(songs, start, limit);
    for (const song of page.items) moved.delete(song.id);
    const merged = appendPage(items, page.items, page.total, key);
    items = merged.items;
    if (!merged.grew || items.length >= merged.total) return items;
  }
}

describe("pageWindow", () => {
  it("asks for the rows after the listed ones while nothing moved", () => {
    expect(pageWindow(400, 200, 0)).toEqual({ start: 400, limit: 200 });
  });

  it("does not skip the row a rename pulled back into the listed range", () => {
    // Five of ten listed, then "s2" is renamed to sort after "s7". On the
    // server "s6" now sits at offset 4; a page asked for at offset 5 skips it.
    const before = library(10);
    const listed = serve(before, 0, 5).items;
    const after = rename(before, "s2", "song 07b");
    const moved = new Set(["s2"]);
    expect(ids(pageToEnd(after, listed, moved, 5)).sort()).toEqual(ids(after).sort());
    expect(moved.size).toBe(0); // a page brought "s2" back, so it no longer counts
  });

  it("starts early enough for several renames, over several pages", () => {
    const before = library(10);
    const listed = serve(before, 0, 5).items;
    const after = rename(rename(before, "s2", "song 08a"), "s3", "song 08b");
    const all = pageToEnd(after, listed, new Set(["s2", "s3"]), 2);
    expect(ids(all).sort()).toEqual(ids(after).sort());
  });

  it("only repeats a row for an edit that did not move the row past the list", () => {
    // "s4" now sorts first, so no page brings it back and each one starts a
    // row early. Nothing is skipped, nothing is listed twice.
    const before = library(12);
    const listed = serve(before, 0, 6).items;
    const moved = new Set(["s4"]);
    const all = pageToEnd(rename(before, "s4", "song 00"), listed, moved, 3);
    expect(all).toHaveLength(12);
    expect(new Set(ids(all)).size).toBe(12);
    expect(moved.has("s4")).toBe(true);
  });

  it("never starts before the first row", () => {
    expect(pageWindow(1, 200, 3)).toEqual({ start: 0, limit: 201 });
  });
});

/** A list whose pages arrive only when the test lets them. */
class FakeList implements PagedList {
  rows = 0;
  requests = 0;
  #inFlight: { promise: Promise<boolean>; settle: (ok: boolean) => void } | null = null;

  constructor(
    readonly size: number,
    readonly pageSize = 100,
  ) {}

  length = () => this.rows;
  hasMore = () => this.rows < this.size;
  pending = () => this.#inFlight?.promise ?? null;
  loadMore = () => {
    // Like the pages: no second fetch while one is in flight.
    if (this.#inFlight || !this.hasMore()) return Promise.resolve(false);
    this.requests++;
    let settle: (ok: boolean) => void = () => {};
    const promise = new Promise<boolean>((resolve) => {
      settle = (ok) => {
        this.#inFlight = null;
        if (ok) this.rows = Math.min(this.size, this.rows + this.pageSize);
        resolve(ok);
      };
    });
    this.#inFlight = { promise, settle };
    return promise;
  };

  /** Let the page in flight arrive (or fail), and whoever waits on it run. */
  async arrive(ok = true) {
    this.#inFlight?.settle(ok);
    await new Promise((resolve) => setTimeout(resolve));
  }

  async arriveAll() {
    while (this.pending()) await this.arrive();
  }
}

describe("pageUntil", () => {
  it("pages in until the row exists", async () => {
    const list = new FakeList(1000);
    const done = pageUntil(250, list, () => true);
    await list.arriveAll();
    await expect(done).resolves.toBe(true);
    expect(list.rows).toBe(300);
    expect(list.requests).toBe(3);
  });

  it("waits for a page already in flight instead of taking it as the end", async () => {
    const list = new FakeList(1000);
    void list.loadMore(); // scroll paging got there first
    const done = pageUntil(150, list, () => true);
    await list.arriveAll();
    await expect(done).resolves.toBe(true);
    expect(list.rows).toBe(200);
    expect(list.requests).toBe(2);
  });

  it("hands over to a later letter, which lands where it was asked to", async () => {
    // "M", then "S" while M still pages in: M must stop and not scroll.
    const list = new FakeList(3000);
    let jumpSeq = 0;
    const jump = (index: number) => {
      const own = ++jumpSeq;
      return pageUntil(index, list, () => own === jumpSeq);
    };
    const m = jump(1500);
    await list.arrive();
    await list.arrive();
    const s = jump(1800);
    await list.arriveAll();
    await expect(m).resolves.toBe(false);
    await expect(s).resolves.toBe(true);
    expect(list.rows).toBe(1900);
  });

  it("stops at a failed page and leaves the scroll to the caller", async () => {
    const list = new FakeList(1000);
    const done = pageUntil(500, list, () => true);
    await list.arrive();
    await list.arrive(false);
    await expect(done).resolves.toBe(true);
    expect(list.rows).toBe(100);
    expect(list.pending()).toBeNull();
  });

  it("stops at the end of a list shorter than the row", async () => {
    const list = new FakeList(150);
    const done = pageUntil(400, list, () => true);
    await list.arriveAll();
    await expect(done).resolves.toBe(true);
    expect(list.rows).toBe(150);
  });
});
