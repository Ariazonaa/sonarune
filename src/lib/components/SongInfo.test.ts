import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TrackInfoDto } from "#lib/types.js";

const info: TrackInfoDto = {
  codec: "flac",
  bitrateKbps: 912,
  sampleRateHz: 44_100,
  bitDepth: 16,
  channels: 2,
  container: "flac",
  sizeBytes: 12.5 * 1024 * 1024,
  path: null,
};
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((command: string) => Promise.resolve(command === "get_track_info" ? info : null)),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import SongInfo from "#lib/components/SongInfo.svelte";
import { setLocale } from "#lib/paraglide/runtime.js";

let instance: Record<string, unknown> | null = null;
let row: HTMLButtonElement;

beforeEach(() => {
  // Opened from a row's menu: the menu has just handed focus back to the row.
  row = document.createElement("button");
  document.body.appendChild(row);
  row.focus();
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  document.body.innerHTML = "";
});

async function render(onclose = () => {}) {
  instance = mount(SongInfo, { target: document.body, props: { itemId: "t1", name: "Track", onclose } });
  flushSync();
  // Let the mocked info request resolve.
  await Promise.resolve();
  await Promise.resolve();
  flushSync();
}

const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');

describe("SongInfo", () => {
  it("takes focus from the row behind it and gives it back on close", async () => {
    await render();
    expect(document.activeElement).toBe(dialog());
    // Tab stays in the dialog: its close button is the only control.
    const tab = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    dialog()!.dispatchEvent(tab);
    expect(tab.defaultPrevented).toBe(true);
    expect(dialog()!.contains(document.activeElement)).toBe(true);

    unmount(instance!);
    instance = null;
    flushSync();
    expect(document.activeElement).toBe(row);
  });

  it("writes numbers the way the UI language does", async () => {
    await render();
    expect(dialog()!.textContent).toContain("44.1 kHz");
    expect(dialog()!.textContent).toContain("12.5 MB");
    unmount(instance!);

    setLocale("de", { reload: false });
    try {
      await render();
      expect(dialog()!.textContent).toContain("44,1 kHz");
      expect(dialog()!.textContent).toContain("12,5 MB");
    } finally {
      setLocale("en", { reload: false });
    }
  });
});
