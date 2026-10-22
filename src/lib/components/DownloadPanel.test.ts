import { flushSync, mount, tick, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const handlers = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    handlers.set(name, handler);
    return () => handlers.delete(name);
  }),
}));

import { invoke } from "@tauri-apps/api/core";
import DownloadPanel from "#lib/components/DownloadPanel.svelte";
import { m } from "#lib/paraglide/messages.js";
import { downloads, PANEL_ROWS } from "#lib/state/downloads.svelte.js";
import type { DownloadSnapshot, DownloadStatus, DownloadTask } from "#lib/types.js";

// The panel over a batch of thousands, in jsdom: what it lists, and that it
// follows the events whether it is open or not.

let revision = 0;
const task = (id: string, status: DownloadStatus, extra: Partial<DownloadTask> = {}): DownloadTask => ({
  id,
  itemId: `item-${id}`,
  name: `${id}.flac`,
  status,
  receivedBytes: 0,
  totalBytes: null,
  path: null,
  error: null,
  revision: ++revision,
  ...extra,
});

let backend: DownloadSnapshot;
let instance: Record<string, unknown> | null = null;
let host: HTMLElement;

const rows = () => host.querySelectorAll("li");
const changed = (...tasks: DownloadTask[]) => {
  handlers.get("download:changed")!({ payload: tasks });
  flushSync();
};

async function render() {
  instance = mount(DownloadPanel, { target: host });
  flushSync();
  await new Promise((resolve) => setTimeout(resolve, 0));
  flushSync();
  await tick();
}

beforeEach(() => {
  const batch = Array.from({ length: 3000 }, (_, n) => task(`t${n}`, "queued"));
  backend = { revision: ++revision, tasks: batch };
  vi.mocked(invoke).mockImplementation((async (command: string) =>
    command === "get_downloads" ? backend : undefined) as typeof invoke);
  downloads.reset();
  host = document.createElement("div");
  document.body.appendChild(host);
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  document.body.innerHTML = "";
});

describe("the download panel", () => {
  it("lists a few dozen rows of a batch of thousands", async () => {
    downloads.open = true;
    await render();
    // The next waiting ones, then a line for the rest.
    expect(rows()).toHaveLength(PANEL_ROWS + 1);
    expect(host.textContent).toContain(m.download_more_queued_many({ count: 3000 - PANEL_ROWS }));

    changed(task("t0", "downloading", { receivedBytes: 1024, totalBytes: 4096 }));
    expect(rows()[0].textContent).toContain("t0.flac");
    expect(rows()[0].querySelector('[style*="transform"]')?.getAttribute("style")).toContain("scaleX(0.25)");
  });

  it("cancels the whole batch, also what it does not list, at once", async () => {
    downloads.open = true;
    await render();
    const cancelAll = Array.from(host.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === m.download_cancel_all(),
    )!;
    cancelAll.click();
    await tick();
    const calls = vi.mocked(invoke).mock.calls.map(([command]) => command);
    expect(calls.filter((command) => command === "cancel_all_downloads")).toHaveLength(1);
    expect(calls).not.toContain("cancel_download");
  });

  it("follows the events while it is closed", async () => {
    downloads.open = false;
    await render();
    expect(host.textContent).toContain(m.downloads_title());
    expect(rows()).toHaveLength(0);

    changed(task("t0", "downloading"));
    changed(task("t0", "completed", { path: "C:/t0.flac" }), task("t1", "downloading"));
    changed(task("t1", "failed", { error: "server returned 500" }));
    // The collapsed button counts what is still to do.
    expect(host.textContent).toContain(String(3000 - 2));

    downloads.open = true;
    flushSync();
    const texts = Array.from(rows(), (row) => row.textContent ?? "");
    // Waiting first, then the last to finish: the failed one on top.
    const failed = texts.findIndex((text) => text.includes("t1.flac"));
    const completed = texts.findIndex((text) => text.includes("t0.flac"));
    expect(failed).toBeGreaterThan(PANEL_ROWS);
    expect(completed).toBe(failed + 1);
    expect(texts[failed]).toContain("server returned 500");
    expect(texts[completed]).toContain(m.download_status_completed());
  });

  it("shows what failed early in a long batch, and retries all of it at once", async () => {
    // 100 fail at the start, the other 2900 complete after them.
    backend = {
      revision: ++revision,
      tasks: [
        ...Array.from({ length: 100 }, (_, n) => task(`x${n}`, "failed", { error: `error ${n}` })),
        ...Array.from({ length: 2900 }, (_, n) => task(`c${n}`, "completed")),
      ],
    };
    downloads.open = true;
    await render();
    expect(host.textContent).toContain(m.download_stopped_many({ count: 100 }));
    expect(host.textContent).toContain(m.download_more_stopped_many({ count: 100 - PANEL_ROWS }));
    expect(host.textContent).toContain(m.download_more_completed_many({ count: 2900 - PANEL_ROWS }));
    // The failed ones come first, each with its retry.
    const texts = Array.from(rows(), (row) => row.textContent ?? "");
    expect(texts.findIndex((text) => text.includes("x99.flac"))).toBeLessThan(
      texts.findIndex((text) => text.includes("c2899.flac")),
    );
    expect(host.querySelectorAll("li button").length).toBeGreaterThanOrEqual(PANEL_ROWS + 1);

    const retryAll = Array.from(host.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === m.download_retry_all(),
    )!;
    retryAll.click();
    await tick();
    const calls = vi.mocked(invoke).mock.calls.map(([command]) => command);
    expect(calls.filter((command) => command === "retry_failed_downloads")).toHaveLength(1);
    expect(calls).not.toContain("retry_download");
  });
});
