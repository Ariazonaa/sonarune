import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import PlaylistTransferDialog from "#lib/components/PlaylistTransferDialog.svelte";
import { LONG_LIST_ROWS } from "#lib/longList.js";
import { m } from "#lib/paraglide/messages.js";
import type { PlaylistDto, PlaylistTrackDto, TrackDto } from "#lib/types.js";

// The transfer dialog over a long playlist, in jsdom: virtua's sizes come
// from a stand-in ResizeObserver (rows of ROW px in a box VIEWPORT px high).

const ROW = 40;
const VIEWPORT = 400;
const invokeMock = vi.mocked(invoke);
let instance: ReturnType<typeof mount> | null = null;
let boxTop = 0;

const entry = (n: number, entryId = `e${n}`): PlaylistTrackDto => ({
  track: { id: `t${n}`, name: `Track ${n}`, artist: "Artist", album: "Album" } as TrackDto,
  entryId,
});
const entries = (count: number) => Array.from({ length: count }, (_, n) => entry(n));

const settle = () => new Promise((resolve) => setTimeout(resolve, 0)).then(() => flushSync());
const listBox = () => document.querySelector<HTMLElement>(".overflow-y-auto")!;
const boxes = () => document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
const rowFor = (n: number) =>
  Array.from(document.querySelectorAll<HTMLElement>("label")).find(
    (label) => label.querySelector("span span")?.textContent?.trim() === `Track ${n}`,
  ) ?? null;

async function render(tracks: PlaylistTrackDto[]) {
  instance = mount(PlaylistTransferDialog, {
    target: document.body.appendChild(document.createElement("div")),
    props: { sourcePlaylistId: "src", tracks, onclose: () => {}, oncomplete: () => {} },
  });
  flushSync();
  await settle();
  await settle();
}

async function scrollBox(top: number) {
  boxTop = top;
  listBox().dispatchEvent(new Event("scroll"));
  flushSync();
  await settle();
}

beforeEach(() => {
  boxTop = 0;
  invokeMock.mockReset();
  invokeMock.mockImplementation((async (command: string) => {
    if (command === "get_playlists") return [{ id: "dst", name: "Target" } as PlaylistDto];
    if (command === "transfer_playlist_entries") return { added: 1, removeError: null };
    return undefined;
  }) as typeof invoke);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private callback: ResizeObserverCallback) {}
      observe(target: Element) {
        const height = target.classList.contains("overflow-y-auto") ? VIEWPORT : ROW;
        queueMicrotask(() =>
          this.callback(
            [{ target, contentRect: { height, width: 600 } } as unknown as ResizeObserverEntry],
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
  Object.defineProperty(HTMLElement.prototype, "scrollTop", {
    configurable: true,
    get() {
      return this.classList?.contains("overflow-y-auto") ? boxTop : 0;
    },
    set(value: number) {
      if (this.classList?.contains("overflow-y-auto")) boxTop = value;
    },
  });
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  vi.unstubAllGlobals();
  delete (HTMLElement.prototype as { offsetParent?: unknown }).offsetParent;
  delete (HTMLElement.prototype as { scrollTop?: unknown }).scrollTop;
  document.body.innerHTML = "";
});

describe("the transfer dialog's list", () => {
  it("lists a playlist up to the limit in full, and a longer one only in view", async () => {
    await render(entries(LONG_LIST_ROWS));
    expect(boxes()).toHaveLength(LONG_LIST_ROWS);
    unmount(instance!);
    document.body.innerHTML = "";

    await render(entries(5000));
    expect(boxes().length).toBeGreaterThan(0);
    expect(boxes().length).toBeLessThan(40);
    await scrollBox(2500 * ROW);
    expect(rowFor(2500)).not.toBeNull();
    expect(rowFor(0)).toBeNull();
  });

  it("selects rows it has not mounted, and sends every one", async () => {
    const tracks = entries(5000);
    // A track that is in the playlist twice shares its entry id on 10.11.
    tracks.push(entry(7, "e7"));
    await render(tracks);
    rowFor(3)!.querySelector("input")!.click();
    flushSync();
    expect(document.body.textContent).toContain(m.playlist_transfer_selected({ count: 1 }));

    const selectAll = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === m.playlist_transfer_select_all(),
    )!;
    selectAll.click();
    flushSync();
    expect(document.body.textContent).toContain(m.playlist_transfer_selected({ count: 5000 }));
    // Rows mounted later show the selection too.
    await scrollBox(4000 * ROW);
    expect(rowFor(4000)!.querySelector("input")!.checked).toBe(true);

    const confirm = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === m.playlist_transfer_confirm_copy(),
    )!;
    confirm.click();
    await settle();
    const call = invokeMock.mock.calls.find(([command]) => command === "transfer_playlist_entries");
    const sent = (call?.[1] as { entryIds: string[] }).entryIds;
    expect(sent).toHaveLength(5000);
    expect(new Set(sent).size).toBe(5000);
  });

  it("keeps the focused row mounted while the list scrolls away from it", async () => {
    await render(entries(5000));
    const input = rowFor(2)!.querySelector("input")!;
    input.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    await scrollBox(3000 * ROW);
    expect(rowFor(3000)).not.toBeNull();
    expect(rowFor(2)).not.toBeNull();
  });

  it("filters the long list", async () => {
    await render(entries(5000));
    const search = document.querySelector<HTMLInputElement>('input[type="search"]')!;
    search.value = "track 4999";
    search.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    await settle();
    expect(boxes()).toHaveLength(1);
    expect(rowFor(4999)).not.toBeNull();
  });
});
