import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { flushSync, mount, unmount } from "svelte";
import { compile } from "svelte/compiler";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import PlaylistTrackList from "#lib/components/PlaylistTrackList.svelte";
import { LONG_LIST_ROWS } from "#lib/longList.js";
import {
  entryRowKeys,
  filterRows,
  searchTexts,
  type PlaylistRow,
} from "#lib/playlistEntries.js";
import { liveProps } from "#lib/testing/liveProps.svelte.js";
import type { PlaylistTrackDto, TrackDto } from "#lib/types.js";

// The playlist page's list in jsdom, both kinds. jsdom has no layout, so the
// sizes virtua measures and the boxes a drag measures are stand-ins: rows of
// ROW px under a HEADER px header, in a scroller VIEWPORT px high.

const ROW = 48;
const HEADER = 320;
const VIEWPORT = 480;

let scroller: HTMLElement;
let scrollTop = 0;
let instance: ReturnType<typeof mount> | null = null;
const calls = {
  play: [] as number[],
  move: [] as [number, number][],
  menu: [] as number[],
};

const entry = (n: number): PlaylistTrackDto => ({
  track: {
    id: `t${n}`,
    name: `Track ${n}`,
    artist: "Artist",
    album: "Album",
    durationMs: 200_000,
    imageItemId: null,
    imageTag: null,
    imageBlurHash: null,
  } as TrackDto,
  entryId: `t${n}`,
});

function rowsOf(count: number, query = ""): PlaylistRow[] {
  const entries = Array.from({ length: count }, (_, n) => entry(n));
  return filterRows(entries, entryRowKeys(entries), searchTexts(entries), query);
}

/** The element virtua positions a row in, if any. */
function itemOf(element: Element): HTMLElement | null {
  for (let node: Element | null = element; node; node = node.parentElement) {
    if (node instanceof HTMLElement && node.style.position === "absolute") return node;
  }
  return null;
}

function box(top: number, height: number): DOMRect {
  return { top, height, bottom: top + height, left: 0, right: 600, width: 600, x: 0, y: top, toJSON() {} };
}

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0)).then(() => flushSync());
}

async function render(rows: PlaylistRow[], { reorderable = true, total = rows.length } = {}) {
  const target = document.createElement("div");
  scroller.appendChild(target);
  // Live, so a test can hand the list new rows, as a reload of the page does.
  const props = liveProps({
    rows,
    total,
    currentTrackId: undefined as string | undefined,
    reorderable,
    onplay: (index: number) => calls.play.push(index),
    onmenu: (_event: MouseEvent, _entry: PlaylistTrackDto, index: number) => calls.menu.push(index),
    onaddto: () => {},
    onremove: () => {},
    onmove: (from: number, to: number) => calls.move.push([from, to]),
  });
  instance = mount(PlaylistTrackList, { target, props });
  flushSync();
  await settle();
  await settle();
  return props;
}

const row = (n: number) => scroller.querySelector<HTMLElement>(`[data-row-key="e:t${n}"]`);
const mounted = () => scroller.querySelectorAll("[data-row-key]").length;

async function scrollTo(top: number) {
  scrollTop = top;
  scroller.dispatchEvent(new Event("scroll"));
  flushSync();
  await settle();
}

function fire(target: EventTarget, type: string, clientY = 0): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clientY", { value: clientY });
  target.dispatchEvent(event);
  flushSync();
  return event;
}

/** A pointer over the lower half of the row: the slot behind it. */
const lowerHalf = (element: HTMLElement) => element.getBoundingClientRect().top + ROW * 0.75;

beforeEach(() => {
  calls.play = [];
  calls.move = [];
  calls.menu = [];
  scrollTop = 0;
  scroller = document.createElement("div");
  scroller.style.overflowY = "auto";
  Object.defineProperty(scroller, "scrollTop", {
    configurable: true,
    get: () => scrollTop,
    set: (value: number) => (scrollTop = value),
  });
  document.body.appendChild(scroller);

  // virtua reads sizes from ResizeObserver entries, and only for elements in
  // the layout (with an offsetParent).
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private callback: ResizeObserverCallback) {}
      observe(target: Element) {
        const height = target === scroller ? VIEWPORT : ROW;
        queueMicrotask(() =>
          this.callback(
            [{ target, contentRect: box(0, height) } as unknown as ResizeObserverEntry],
            this as unknown as ResizeObserver,
          ),
        );
      }
      unobserve() {}
      disconnect() {}
    },
  );
  Object.defineProperty(HTMLElement.prototype, "offsetParent", {
    configurable: true,
    get() {
      return this.parentElement;
    },
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    if (this === scroller) return box(0, VIEWPORT);
    // The list starts below the header; a long list's row sits at virtua's
    // offset in it, a plain list's rows all at its top (good enough here).
    const item = itemOf(this);
    return box(HEADER + (item ? parseFloat(item.style.top) : 0) - scrollTop, ROW);
  });
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  delete (HTMLElement.prototype as { offsetParent?: unknown }).offsetParent;
  document.body.innerHTML = "";
});

describe("the kind of list", () => {
  it("lists a playlist up to the limit in full", async () => {
    await render(rowsOf(LONG_LIST_ROWS));
    expect(mounted()).toBe(LONG_LIST_ROWS);
  });

  it("mounts only the rows in view above it", async () => {
    await render(rowsOf(LONG_LIST_ROWS + 1));
    expect(mounted()).toBeGreaterThan(0);
    expect(mounted()).toBeLessThan(40);
    expect(row(0)).not.toBeNull();

    await render(rowsOf(8000));
    expect(mounted()).toBeLessThan(80);
  });

  it("goes by the size of the playlist, not of the filtered rows", async () => {
    // A filter that leaves 12 of 8000 rows keeps the long list.
    const rows = rowsOf(8000, "track 1").slice(0, 12);
    await render(rows, { total: 8000, reorderable: false });
    expect(scroller.querySelector('[style*="absolute"]')).not.toBeNull();
  });

  it("shows the rows where the page's scroller already stands", async () => {
    // The shell's scroller keeps its offset from the page before.
    scrollTop = HEADER + 3000 * ROW;
    await render(rowsOf(8000));
    expect(row(3000)).not.toBeNull();
    expect(row(0)).toBeNull();
  });
});

describe("rows", () => {
  it("play from their place in the playlist, filtered or not", async () => {
    await render(rowsOf(8000, "track 12"), { total: 8000, reorderable: false });
    // "Track 12", "Track 120", … — the third match is entry 121.
    const third = scroller.querySelectorAll<HTMLElement>("[data-row-key]")[2];
    expect(third.dataset.rowKey).toBe("e:t121");
    third.click();
    third.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    third.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    expect(calls.play).toEqual([121, 121]);
    expect(calls.menu).toEqual([121]);
  });

  it("keep the focused row mounted while the list scrolls away from it", async () => {
    await render(rowsOf(8000));
    row(3)!.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    await scrollTo(HEADER + 4000 * ROW);
    expect(row(4000)).not.toBeNull();
    expect(row(3)).not.toBeNull();

    row(3)!.dispatchEvent(new FocusEvent("focusout", { bubbles: true, relatedTarget: null }));
    flushSync();
    expect(row(3)).toBeNull();
  });
});

describe("reordering a long list", () => {
  it("moves a row dragged far down to the slot marked on the row it is dropped on", async () => {
    await render(rowsOf(8000));
    const source = row(2)!;
    fire(source, "dragstart");

    // Dragging near the edge scrolls the page far down; the source stays.
    await scrollTo(HEADER + 1000 * ROW);
    expect(row(2)).toBe(source);
    // virtua takes pointer events off its rows while the page scrolls; the
    // list's own rule gives them back during a drag, or the drop would miss.
    const disabled = scroller.querySelector<HTMLElement>('[style*="pointer-events: none"]');
    expect(disabled?.matches(".dragging > div")).toBe(true);
    const { css } = compile(
      readFileSync(resolve("src/lib/components/PlaylistTrackList.svelte"), "utf8"),
      { filename: "PlaylistTrackList.svelte", css: "external" },
    );
    expect(css?.code).toMatch(/\.dragging\.svelte-[\w-]+ > div\s*\{\s*pointer-events: auto !important;\s*\}/);
    const target = row(1005)!;
    // The row that slides under the pointer takes the drag on entering, or
    // Chromium drops nothing when it is let go there.
    expect(fire(target, "dragenter").defaultPrevented).toBe(true);
    const over = fire(target, "dragover", lowerHalf(target));
    expect(over.defaultPrevented).toBe(true);
    // The slot is marked between the row hovered and the next one, nowhere
    // else.
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(2);
    expect(target.querySelector(".bottom-0.bg-accent")).not.toBeNull();
    expect(row(1006)!.querySelector(".top-0.bg-accent")).not.toBeNull();
    expect(scroller.querySelector(".dragging")).not.toBeNull();

    fire(target, "drop", lowerHalf(target));
    fire(source, "dragend");
    // Behind row 1005 is slot 1006; the row leaves 2 first, so it lands at 1005.
    expect(calls.move).toEqual([[2, 1005]]);
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(0);
    expect(scroller.querySelector(".dragging")).toBeNull();
    expect(scroller.querySelector(".opacity-40")).toBeNull();
  });

  it("takes no drag it did not start", async () => {
    await render(rowsOf(20));
    // A file or a link from outside: no row accepts it, entering or over.
    expect(fire(row(3)!, "dragenter").defaultPrevented).toBe(false);
    expect(fire(row(3)!, "dragover", lowerHalf(row(3)!)).defaultPrevented).toBe(false);
  });

  it("marks the slot in front of a row over its upper half", async () => {
    await render(rowsOf(8000));
    fire(row(10)!, "dragstart");
    const target = row(4)!;
    fire(target, "dragover", target.getBoundingClientRect().top + 2);
    expect(target.querySelector(".top-0.bg-accent")).not.toBeNull();
    fire(target, "drop", target.getBoundingClientRect().top + 2);
    expect(calls.move).toEqual([[10, 4]]);
  });

  it("leaves nothing behind when a drag ends outside the window or off the rows", async () => {
    await render(rowsOf(8000));
    const source = row(5)!;
    fire(source, "dragstart");
    fire(row(8)!, "dragover", lowerHalf(row(8)!));
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(2);
    expect(source.classList.contains("opacity-40")).toBe(true);

    // Released outside the window: only the source's dragend comes, at the
    // window.
    fire(window, "dragend");
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(0);
    expect(source.classList.contains("opacity-40")).toBe(false);
    expect(scroller.querySelector(".dragging")).toBeNull();

    // Dropped on something that is not a row.
    fire(source, "dragstart");
    fire(row(8)!, "dragover", lowerHalf(row(8)!));
    fire(document.body, "drop");
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(0);

    // A drag nobody reported the end of: the next press clears it.
    fire(source, "dragstart");
    fire(window, "pointerdown");
    expect(scroller.querySelector(".dragging")).toBeNull();
    expect(calls.move).toEqual([]);
  });

  it("dropping a row next to itself moves nothing", async () => {
    await render(rowsOf(8000));
    fire(row(6)!, "dragstart");
    fire(row(6)!, "dragover", lowerHalf(row(6)!));
    fire(row(6)!, "drop", lowerHalf(row(6)!));
    fire(row(5)!, "dragover", lowerHalf(row(5)!));
    expect(calls.move).toEqual([]);
  });

  it("keeps the marker from row to row, and moves to where the row is let go", async () => {
    await render(rowsOf(8000));
    fire(row(1)!, "dragstart");
    fire(row(8)!, "dragover", lowerHalf(row(8)!));
    // Into the next row: the list is not left, the marker stays.
    const leave = new Event("dragleave", { bubbles: true });
    Object.defineProperty(leave, "relatedTarget", { value: row(9) });
    row(8)!.dispatchEvent(leave);
    flushSync();
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(2);
    // Let go over row 9's upper half before its first dragover: slot 9.
    fire(row(9)!, "drop", row(9)!.getBoundingClientRect().top + 2);
    expect(calls.move).toEqual([[1, 8]]);

    // Out of the list: no marker.
    fire(row(1)!, "dragstart");
    fire(row(8)!, "dragover", lowerHalf(row(8)!));
    row(8)!.dispatchEvent(new Event("dragleave", { bubbles: true }));
    flushSync();
    expect(scroller.querySelectorAll(".bg-accent")).toHaveLength(0);
  });

  it("takes no drag from elsewhere, and none while reordering is off", async () => {
    await render(rowsOf(8000));
    // A file or a track from another page: no dragstart here.
    expect(fire(row(3)!, "dragover", lowerHalf(row(3)!)).defaultPrevented).toBe(false);

    if (instance) unmount(instance);
    await render(rowsOf(8000), { reorderable: false });
    expect(row(3)!.getAttribute("draggable")).toBe("false");
    fire(row(3)!, "dragstart");
    expect(fire(row(7)!, "dragover", lowerHalf(row(7)!)).defaultPrevented).toBe(false);
    expect(scroller.querySelector(".dragging")).toBeNull();
  });
});

describe("a reload during a drag", () => {
  function reordered(count: number, order: (ids: number[]) => number[]): PlaylistRow[] {
    const entries = order(Array.from({ length: count }, (_, n) => n)).map(entry);
    return filterRows(entries, entryRowKeys(entries), searchTexts(entries), "");
  }

  it("moves the dragged entry from where it stands now", async () => {
    const props = await render(rowsOf(400));
    fire(row(2)!, "dragstart");
    // The page fetched the playlist again meanwhile: entry 2 is now first.
    props.rows = reordered(400, (ids) => [2, ...ids.filter((n) => n !== 2)]);
    flushSync();
    await settle();
    const target = row(10)!;
    fire(target, "dragover", lowerHalf(target));
    fire(target, "drop", lowerHalf(target));
    // Behind entry 10, which now stands at 10: from 0, so it lands at 10.
    expect(calls.move).toEqual([[0, 10]]);
  });

  it("moves nothing once the dragged entry is gone", async () => {
    const props = await render(rowsOf(400));
    fire(row(2)!, "dragstart");
    props.rows = reordered(400, (ids) => ids.filter((n) => n !== 2));
    flushSync();
    await settle();
    fire(row(10)!, "drop", lowerHalf(row(10)!));
    expect(calls.move).toEqual([]);
    expect(scroller.querySelector(".dragging")).toBeNull();
  });
});

describe("reordering a short list", () => {
  it("moves by the same slots", async () => {
    await render(rowsOf(40));
    fire(row(1)!, "dragstart");
    const target = row(30)!;
    fire(target, "dragover", lowerHalf(target));
    expect(target.querySelector(".bottom-0.bg-accent")).not.toBeNull();
    fire(target, "drop", lowerHalf(target));
    expect(calls.move).toEqual([[1, 30]]);
    expect(scroller.querySelector(".dragging")).toBeNull();
  });
});

describe("the way back", () => {
  it("says which row the long list was scrolled to, and goes back there", async () => {
    await render(rowsOf(8000));
    await scrollTo(HEADER + 1000 * ROW + 10);
    const exports = instance as unknown as {
      position(): { top: number; index: number; within: number };
      restore(at: { top: number; index: number; within: number }): void;
    };
    const at = exports.position();
    expect(at).toEqual({ top: HEADER + 1000 * ROW + 10, index: 1000, within: 10 });

    // Opened again, nothing measured above.
    unmount(instance!);
    scrollTop = 0;
    await render(rowsOf(8000));
    (instance as unknown as typeof exports).restore(at);
    flushSync();
    await settle();
    expect(scrollTop).toBe(HEADER + 1000 * ROW + 10);
    // The browser follows a new offset with a scroll event; jsdom does not.
    await scrollTo(scrollTop);
    expect(row(1000)).not.toBeNull();
  });

  it("goes back to an offset in the header, and in a short list", async () => {
    await render(rowsOf(8000));
    await scrollTo(100);
    const exports = instance as unknown as { position(): unknown };
    expect(exports.position()).toEqual({ top: 100, index: -1, within: 0 });

    unmount(instance!);
    scrollTop = 0;
    await render(rowsOf(40));
    (instance as unknown as { restore(at: unknown): void }).restore({ top: 777, index: 12, within: 3 });
    flushSync();
    expect(scrollTop).toBe(777);
  });
});
