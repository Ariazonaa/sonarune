import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import { downloads } from "#lib/state/downloads.svelte.js";
import type { DownloadStatus, DownloadTask } from "#lib/types.js";

const task = (status: DownloadStatus): DownloadTask => ({
  id: "d1",
  itemId: "t1",
  name: "Track.flac",
  status,
  receivedBytes: 0,
  totalBytes: null,
  path: null,
  error: null,
});

let list: DownloadTask[] = [];
vi.mocked(invoke).mockImplementation((async (command: string) =>
  command === "get_downloads" ? list : undefined) as typeof invoke);

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function windowComesBack() {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("downloads", () => {
  it("reads the list back when the window returns", async () => {
    list = [task("downloading")];
    await downloads.init();
    expect(downloads.activeCount).toBe(1);

    // Finished while the window was hidden; the events never arrived.
    list = [task("completed")];
    windowComesBack();
    await settle();
    expect(downloads.activeCount).toBe(0);
    expect(downloads.finishedCount).toBe(1);

    // The panel is gone: nothing listens any more.
    downloads.destroy();
    list = [];
    windowComesBack();
    await settle();
    expect(downloads.tasks).toEqual([task("completed")]);
  });
});
