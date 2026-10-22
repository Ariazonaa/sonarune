import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

// The store only talks to Tauri from `init()`, which these tests never call.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

import Harness from "./library.harness.svelte";
import { library } from "./library.svelte.js";

let instance: Record<string, unknown> | null = null;

function render(onchange: () => void) {
  instance = mount(Harness, { target: document.body, props: { onchange } });
  flushSync();
}

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  document.body.innerHTML = "";
});

describe("onLibraryChange", () => {
  it("leaves the revision a view mounts at to the view's own load", () => {
    // Once anything changed this session the revision stays above 0. A view
    // that reloaded for it on mount fetched everything twice on every visit.
    library.revision = 3;
    const onchange = vi.fn();
    render(onchange);
    expect(onchange).not.toHaveBeenCalled();
  });

  it("runs once for every change after the mount", () => {
    library.revision = 0;
    const onchange = vi.fn();
    render(onchange);
    library.poke();
    flushSync();
    expect(onchange).toHaveBeenCalledTimes(1);
    library.poke();
    flushSync();
    expect(onchange).toHaveBeenCalledTimes(2);
  });

  it("stops with the view", () => {
    library.revision = 0;
    const onchange = vi.fn();
    render(onchange);
    unmount(instance!);
    instance = null;
    library.poke();
    flushSync();
    expect(onchange).not.toHaveBeenCalled();
  });
});
