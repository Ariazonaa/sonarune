import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import { page } from "$app/state";
import PlaylistPage from "./+page.svelte";
import { swrClear, swrGet } from "#lib/swr.js";
import type { PlaylistDetail, PlaylistDto, PlaylistTrackDto, TrackDto } from "#lib/types.js";

// The playlist page around its list, in jsdom (`$app/state` is a stand-in,
// src/lib/testing/appState.ts): what a drag & drop does to the playlist.

const invokeMock = vi.mocked(invoke);
let instance: ReturnType<typeof mount> | null = null;

const entry = (id: string): PlaylistTrackDto => ({
  track: {
    id,
    name: `Track ${id}`,
    artist: "Artist",
    album: "Album",
    durationMs: 200_000,
    imageItemId: null,
    imageTag: null,
    imageBlurHash: null,
  } as TrackDto,
  entryId: id,
});

const detail = (): PlaylistDetail => ({
  playlist: { id: "p1", name: "Mix", trackCount: 5 } as PlaylistDto,
  tracks: ["a", "b", "c", "d", "e"].map(entry),
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0)).then(() => flushSync());
const row = (id: string) => document.querySelector<HTMLElement>(`[data-row-key="e:${id}"]`)!;

function fire(target: EventTarget, type: string, clientY = 0) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clientY", { value: clientY });
  target.dispatchEvent(event);
  flushSync();
}

beforeEach(() => {
  swrClear();
  page.params.id = "p1";
  invokeMock.mockReset();
  invokeMock.mockImplementation((async (command: string) =>
    command === "get_playlist" ? detail() : undefined) as typeof invoke);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("the playlist page", () => {
  it("moves a dropped row on screen, in its cache and on the server", async () => {
    instance = mount(PlaylistPage, { target: document.body.appendChild(document.createElement("div")) });
    flushSync();
    await settle();
    expect(row("a")).not.toBeNull();

    // "a" dropped behind "d" (jsdom boxes have no height: any pointer below
    // a row's top is its lower half).
    fire(row("a"), "dragstart");
    fire(row("d"), "dragover", 10);
    fire(row("d"), "drop", 10);

    const order = ["b", "c", "d", "a", "e"];
    const shown = Array.from(document.querySelectorAll<HTMLElement>("[data-row-key]"), (r) =>
      r.dataset.rowKey!.slice(2),
    );
    expect(shown).toEqual(order);
    // The way back renders from the cache first: it has the new order too.
    const cached = swrGet<PlaylistDetail>("playlist:p1");
    expect(cached?.tracks.map((t) => t.entryId)).toEqual(order);
    expect(invokeMock).toHaveBeenCalledWith("playlist_move", {
      playlistId: "p1",
      entryId: "a",
      newIndex: 3,
    });
  });
});
