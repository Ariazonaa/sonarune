import { describe, expect, it, vi } from "vitest";

const { listeners } = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(name, handler);
    return () => listeners.delete(name);
  }),
}));

import { invoke } from "@tauri-apps/api/core";
import { library } from "#lib/state/library.svelte.js";

/** The watcher's change count, as `check_library_now` reports it. */
let watcherCount = 0;
vi.mocked(invoke).mockImplementation((async (command: string) =>
  command === "check_library_now" ? watcherCount : undefined) as typeof invoke);

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function windowComesBack() {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
  document.dispatchEvent(new Event("visibilitychange"));
}

function changed(count: number) {
  listeners.get("library:changed")!({ payload: count });
}

describe("library revision", () => {
  it("catches up on a change the window never heard of", async () => {
    // Changes found before this page existed are in what it loads now.
    watcherCount = 2;
    await library.init();
    await settle();
    const start = library.revision;

    watcherCount = 3;
    changed(3);
    expect(library.revision).toBe(start + 1);

    // Found while the window was hidden: the event went to a suspended
    // WebView, the watcher moved on.
    watcherCount = 4;
    windowComesBack();
    await settle();
    expect(library.revision).toBe(start + 2);

    // Nothing new: no refetch, and a late event for a count already caught
    // up on changes nothing either.
    windowComesBack();
    await settle();
    changed(4);
    expect(library.revision).toBe(start + 2);

    // Focus asks the same way.
    watcherCount = 5;
    window.dispatchEvent(new Event("focus"));
    await settle();
    expect(library.revision).toBe(start + 3);
  });
});
