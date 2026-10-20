import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { flushSync, mount, tick, unmount } from "svelte";
import { compile } from "svelte/compiler";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));

import { invoke } from "@tauri-apps/api/core";
import QueuePanel from "#lib/components/QueuePanel.svelte";
import { FLIP_ROW_LIMIT } from "#lib/motion.js";
import { m } from "#lib/paraglide/messages.js";
import { player } from "#lib/state/player.svelte.js";
import type { AutoDjReason, PlayerState, QueueTrack } from "#lib/types.js";

// The queue panel end to end in jsdom: what it scrolls to reveal the playing
// row, how it lists short and long queues, and the inline playlist naming.
// jsdom has no layout, so the observers and scroll methods are stand-ins.

const invokeMock = vi.mocked(invoke);
let instance: Record<string, unknown> | null = null;
let host: HTMLElement;
let scrollIntoView: ReturnType<typeof vi.fn>;
let scrollTo: ReturnType<typeof vi.fn>;

const track = (itemId: string, autoDjReason: AutoDjReason | null = null) =>
  ({
    itemId,
    entryId: `entry-${itemId}`,
    name: `Track ${itemId}`,
    artist: "Artist",
    album: "Album",
    albumId: "album",
    trackNumber: null,
    discNumber: null,
    durationMs: 200_000,
    imageItemId: null,
    imageTag: null,
    imageBlurHash: null,
    streamUrl: "",
    normalizationGain: null,
    albumNormalizationGain: null,
    artists: [],
    genres: [],
    sourcePlaylistId: null,
    autoDjReason,
    isFavorite: false,
  }) satisfies QueueTrack;

function setQueue(tracks: QueueTrack[], index: number, order: number[] = []) {
  player.queue = {
    tracks,
    index,
    order,
    canUndo: false,
    playedCount: 0,
    duplicateCount: 0,
    revision: 0,
  };
  player.state = {
    ...player.state,
    status: "playing",
    current: tracks[index] ?? null,
    index,
    queueLen: tracks.length,
    stopAfter: "off",
    stopAfterItem: null,
    stopAfterEntry: null,
  } satisfies PlayerState;
}

const tracksNamed = (count: number) => Array.from({ length: count }, (_, i) => track(`t${i}`));

async function render() {
  instance = mount(QueuePanel, { target: host });
  flushSync();
  await tick();
  await tick();
}

const list = () => host.querySelector<HTMLElement>('[role="list"]');
const rows = () => host.querySelectorAll('[role="listitem"]');

beforeEach(() => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  scrollIntoView = vi.fn();
  scrollTo = vi.fn();
  Element.prototype.scrollIntoView = scrollIntoView as unknown as Element["scrollIntoView"];
  Element.prototype.scrollTo = scrollTo as unknown as Element["scrollTo"];
  invokeMock.mockClear();
  host = document.createElement("div");
  document.body.appendChild(host);
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("revealing the playing track", () => {
  it("scrolls the list itself, never the containers around it", async () => {
    setQueue(tracksNamed(12), 9);
    await render();
    // scrollIntoView would scroll the app shell as well (it is clipped, but
    // every box above the list counts).
    expect(scrollIntoView).not.toHaveBeenCalled();
    expect(scrollTo).toHaveBeenCalled();
    expect(scrollTo.mock.contexts[0]).toBe(list());
  });
});

describe("rows", () => {
  it("lists a short queue in full, as a plain list", async () => {
    setQueue(tracksNamed(10), 3);
    await render();
    expect(rows()).toHaveLength(10);
  });

  it("renders only part of a long queue, and keeps the playing row", async () => {
    const count = FLIP_ROW_LIMIT + 600;
    setQueue(tracksNamed(count), 700);
    await render();
    expect(rows().length).toBeGreaterThan(0);
    expect(rows().length).toBeLessThan(count);
    const current = host.querySelector('[data-current="true"]');
    expect(current?.textContent).toContain("Track t700");
  });

  it("keeps the rows of a long queue droppable while a drag scrolls it", async () => {
    setQueue(tracksNamed(FLIP_ROW_LIMIT + 600), 0);
    await render();
    const wrapper = list()!;
    const viewport = wrapper.firstElementChild as HTMLElement; // VList's scroller
    const rowsBox = viewport.firstElementChild as HTMLElement;
    const scrollTo = (top: number) => {
      Object.defineProperty(viewport, "scrollTop", { configurable: true, writable: true, value: top });
      viewport.dispatchEvent(new Event("scroll"));
      flushSync();
    };
    // The reveal on open is virtua's own scroll; it ends once a scroll has
    // settled (150 ms on virtua's clock, a real timer).
    scrollTo(10);
    await new Promise((resolve) => setTimeout(resolve, 200));

    // A scroll — a drag near the edge scrolls too: virtua takes pointer
    // events off the rows until it settles, so they would refuse the drop.
    scrollTo(4_000);
    expect(rowsBox.style.pointerEvents).toBe("none");

    // While a drag runs, the panel's own rule gives them back; it outranks
    // virtua's inline style only with !important.
    rows()[0].dispatchEvent(new Event("dragstart", { bubbles: true }));
    flushSync();
    expect(rowsBox.matches(".dragging > div > div")).toBe(true);
    const { css } = compile(readFileSync(resolve("src/lib/components/QueuePanel.svelte"), "utf8"), {
      filename: "QueuePanel.svelte",
      css: "external",
    });
    expect(css?.code).toMatch(
      /\.dragging\.svelte-[\w-]+ > div > div\s*\{\s*pointer-events: auto !important;\s*\}/,
    );

    rows()[0].dispatchEvent(new Event("dragend", { bubbles: true }));
    flushSync();
    expect(wrapper.classList.contains("dragging")).toBe(false);
  });

  it("names the sound seed of an Auto-DJ track", async () => {
    setQueue([track("a"), track("b", { kind: "sonic", label: "Teardrop" })], 0);
    await render();
    const caption = m.queue_autodj_reason_detail({
      kind: m.queue_autodj_kind_sonic(),
      label: "Teardrop",
    });
    expect(host.textContent).toContain(caption);
    expect(host.textContent).not.toContain("undefined");
  });
});

describe("saving the queue as a playlist", () => {
  function startNaming(): HTMLInputElement {
    host.querySelector<HTMLButtonElement>(`[aria-label="${m.queue_save_playlist()}"]`)!.click();
    flushSync();
    return host.querySelector<HTMLInputElement>("input")!;
  }

  function press(input: HTMLInputElement, key: string): KeyboardEvent {
    const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
    input.dispatchEvent(event);
    flushSync();
    return event;
  }

  it("claims Escape, so the view behind the panel does not close too", async () => {
    setQueue(tracksNamed(3), 0);
    await render();
    const input = startNaming();
    const event = press(input, "Escape");
    expect(event.defaultPrevented).toBe(true);
    expect(host.querySelector("input")).toBeNull();
  });

  it("saves the order the list shows, shuffled as it is", async () => {
    setQueue([track("a"), track("b"), track("c"), track("d")], 0, [2, 0, 1, 3]);
    await render();
    const input = startNaming();
    input.value = "Evening";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    const event = press(input, "Enter");
    expect(event.defaultPrevented).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("create_playlist", {
      name: "Evening",
      trackIds: ["c", "a", "b", "d"],
    });
  });
});
