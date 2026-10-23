import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (event: { payload: unknown }) => void;
const handlers = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: Handler) => {
    handlers.set(name, handler);
    return () => handlers.delete(name);
  }),
}));

import { invoke } from "@tauri-apps/api/core";
import { downloads, PANEL_ROWS, panelView } from "#lib/state/downloads.svelte.js";
import type { DownloadSnapshot, DownloadStatus, DownloadTask } from "#lib/types.js";

// The store keeps the newest revision it applied for the life of the module,
// as the app does: every test goes on from where the last one left it.
let revision = 0;
const next = () => ++revision;

const task = (id: string, status: DownloadStatus, extra: Partial<DownloadTask> = {}): DownloadTask => ({
  id,
  itemId: `item-${id}`,
  name: `${id}.flac`,
  status,
  receivedBytes: 0,
  totalBytes: null,
  path: null,
  error: null,
  revision: next(),
  ...extra,
});

const snapshot = (tasks: DownloadTask[]): DownloadSnapshot => ({ revision: next(), tasks });

/** What `get_downloads` answers. */
let backend: DownloadSnapshot = { revision: 0, tasks: [] };
let queued: DownloadTask[] = [];
const invokeMock = vi.mocked(invoke);
const answer = (async (command: string) => {
  if (command === "get_downloads") return backend;
  if (command === "queue_downloads") return queued;
  return undefined;
}) as typeof invoke;

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const emit = (name: string, payload: unknown) => handlers.get(name)!({ payload });
const list = (payload: DownloadSnapshot) => emit("download:state", payload);
const changed = (...tasks: DownloadTask[]) => emit("download:changed", tasks);
const asked = () => invokeMock.mock.calls.filter(([command]) => command === "get_downloads").length;

function windowComesBack() {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
  document.dispatchEvent(new Event("visibilitychange"));
}

async function start(tasks: DownloadTask[]) {
  backend = snapshot(tasks);
  await downloads.init();
  invokeMock.mockClear();
}

beforeEach(() => {
  invokeMock.mockImplementation(answer);
  downloads.reset();
});

afterEach(() => {
  downloads.destroy();
});

describe("downloads", () => {
  it("reads the list back when the window returns", async () => {
    await start([task("d1", "downloading")]);
    expect(downloads.activeCount).toBe(1);

    // Finished while the window was hidden; the events never arrived.
    backend = snapshot([task("d1", "completed")]);
    windowComesBack();
    await settle();
    expect(downloads.activeCount).toBe(0);
    expect(downloads.finishedCount).toBe(1);

    // The panel is gone: nothing listens any more.
    downloads.destroy();
    backend = snapshot([]);
    windowComesBack();
    await settle();
    expect(downloads.tasks.map((t) => t.status)).toEqual(["completed"]);
  });

  it("patches a changed task in place and leaves the others alone", async () => {
    const tasks = Array.from({ length: 2000 }, (_, n) => task(`t${n}`, "queued"));
    await start(tasks);
    const before = downloads.tasks;

    changed(task("t1", "downloading", { receivedBytes: 512, totalBytes: 4096 }));
    expect(downloads.tasks).not.toBe(before);
    expect(downloads.tasks[1]).toMatchObject({ status: "downloading", receivedBytes: 512 });
    // Every other task is the very object it was: its row is not redrawn.
    expect(downloads.tasks.filter((t, n) => t !== before[n])).toHaveLength(1);
    expect(downloads.activeCount).toBe(2000);
    expect(asked()).toBe(0);
  });

  it("keeps the newer copy of a task, whichever arrives first", async () => {
    await start([task("d1", "queued")]);
    const progress = task("d1", "downloading", { receivedBytes: 100 });
    const done = task("d1", "completed", { receivedBytes: 4096, path: "C:/d1.flac" });

    changed(done);
    changed(progress); // late
    expect(downloads.tasks[0]).toBe(done);
    expect(downloads.finishedCount).toBe(1);
  });

  it("drops a list older than the one it has", async () => {
    await start([task("d1", "queued"), task("d2", "queued")]);
    // A `get_downloads` answer taken before the list that overtook it.
    const stale = snapshot([task("d1", "queued"), task("d2", "queued")]);
    const fresh = snapshot([task("d1", "downloading"), task("d2", "queued")]);
    list(fresh);
    list(stale);
    expect(downloads.tasks.map((t) => t.status)).toEqual(["downloading", "queued"]);
  });

  it("keeps a change that overtook the list it came after", async () => {
    await start([task("d1", "queued"), task("d2", "queued")]);
    const taken = snapshot([task("d1", "downloading"), task("d2", "queued")]);
    const later = task("d2", "completed");
    changed(later);
    list(taken);
    expect(downloads.tasks[0].status).toBe("downloading");
    expect(downloads.tasks[1]).toBe(later);
  });

  it("reads the list back when a change names a task it never heard of", async () => {
    await start([task("d1", "queued")]);
    // The list that added d2 was lost (the window was hidden at the time).
    const d2 = task("d2", "downloading");
    backend = snapshot([task("d1", "queued"), d2]);
    changed(d2);
    await settle();
    expect(asked()).toBe(1);
    expect(downloads.tasks.map((t) => t.id)).toEqual(["d1", "d2"]);

    // A late change of a task cleared since asks nothing.
    changed(task("gone", "completed", { revision: 1 }));
    await settle();
    expect(asked()).toBe(1);
    expect(downloads.tasks.map((t) => t.id)).toEqual(["d1", "d2"]);
  });

  it("asks again when the answer is older than the change it was asked for", async () => {
    await start([task("d1", "queued")]);
    // The first answer was on its way before d3 was added.
    const before = snapshot([task("d1", "queued")]);
    const d3 = task("d3", "downloading");
    const after = snapshot([task("d1", "queued"), d3]);
    const answers = [before, after];
    invokeMock.mockImplementation((async (command: string) =>
      command === "get_downloads" ? answers.shift() : undefined) as typeof invoke);
    changed(d3);
    await settle();
    await settle();
    expect(asked()).toBe(2);
    expect(downloads.tasks.map((t) => t.id)).toEqual(["d1", "d3"]);
  });

  it("does not let the answer to queueing overwrite what the events said since", async () => {
    await start([]);
    const added = [task("n1", "queued"), task("n2", "queued")];
    // The list and the first change arrive before the command answers.
    invokeMock.mockImplementation((async (command: string) => {
      if (command !== "queue_downloads") return answer(command);
      list(snapshot(added));
      changed(task("n1", "downloading"));
      return added;
    }) as typeof invoke);
    await downloads.enqueue(added.map(({ itemId, name }) => ({ itemId, name })));
    expect(downloads.tasks.map((t) => [t.id, t.status])).toEqual([
      ["n1", "downloading"],
      ["n2", "queued"],
    ]);
    expect(downloads.open).toBe(true);
  });

  it("shows queued downloads at once when their list is not here yet", async () => {
    await start([]);
    queued = [task("q1", "queued")];
    await downloads.enqueue([{ itemId: "item-q1", name: "q1.flac" }]);
    expect(downloads.tasks.map((t) => t.id)).toEqual(["q1"]);
  });
});

describe("the panel's share of a long list", () => {
  it("lists the running downloads, the next waiting ones and the last to finish", () => {
    const finished = Array.from({ length: 120 }, (_, n) => task(`f${n}`, "completed"));
    const waiting = Array.from({ length: 900 }, (_, n) => task(`w${n}`, "queued"));
    const running = [task("r0", "downloading"), task("r1", "downloading")];
    // In the backend's order: as the batch was queued.
    const view = panelView([...finished, running[0], ...waiting, running[1]]);

    expect(view.active.slice(0, 2)).toEqual(running);
    expect(view.active.slice(2).map((t) => t.id)).toEqual(
      waiting.slice(0, PANEL_ROWS).map((t) => t.id),
    );
    expect(view.moreQueued).toBe(900 - PANEL_ROWS);
    // The last to complete first.
    expect(view.completed.map((t) => t.id)).toEqual(
      finished.slice(-PANEL_ROWS).reverse().map((t) => t.id),
    );
    expect(view.moreCompleted).toBe(120 - PANEL_ROWS);
    expect(view.stopped).toEqual([]);
  });

  it("keeps the failed and cancelled apart, so many completed ones cannot hide them", () => {
    // 2000 tracks: about 100 fail early, the rest completes after them.
    const failed = Array.from({ length: 100 }, (_, n) =>
      task(`x${n}`, n % 4 ? "failed" : "cancelled", { error: n % 4 ? { code: "server", status: 500, detail: "server returned 500" } : null }),
    );
    const done = Array.from({ length: 1900 }, (_, n) => task(`c${n}`, "completed"));
    const view = panelView([...failed, ...done]);

    expect(view.stopped).toHaveLength(PANEL_ROWS);
    expect(view.stopped.map((t) => t.id)).toEqual(
      failed.slice(-PANEL_ROWS).reverse().map((t) => t.id),
    );
    expect(view.moreStopped).toBe(100 - PANEL_ROWS);
    expect(view.completed.every((t) => t.status === "completed")).toBe(true);
    expect(view.moreCompleted).toBe(1900 - PANEL_ROWS);
  });

  it("lists a short batch in full", () => {
    const view = panelView([task("a", "completed"), task("b", "queued"), task("c", "cancelled")]);
    expect(view.active.map((t) => t.id)).toEqual(["b"]);
    expect(view.stopped.map((t) => t.id)).toEqual(["c"]);
    expect(view.completed.map((t) => t.id)).toEqual(["a"]);
    expect(view.moreQueued + view.moreStopped + view.moreCompleted).toBe(0);
  });
});

describe("cancelling", () => {
  it("cancels every queued and running download with one call", async () => {
    await start([task("a", "downloading"), task("b", "queued"), task("c", "completed")]);
    await downloads.cancelAll();
    expect(invokeMock.mock.calls.filter(([command]) => command === "cancel_all_downloads")).toHaveLength(1);
    expect(invokeMock.mock.calls.some(([command]) => command === "cancel_download")).toBe(false);
  });
});

describe("retrying", () => {
  it("retries every failed and cancelled download with one call", async () => {
    await start([task("a", "failed"), task("b", "cancelled"), task("c", "completed")]);
    expect(downloads.stoppedCount).toBe(2);
    downloads.open = false;
    await downloads.retryAll();
    expect(invokeMock.mock.calls.filter(([command]) => command === "retry_failed_downloads")).toHaveLength(1);
    expect(invokeMock.mock.calls.some(([command]) => command === "retry_download")).toBe(false);
    expect(downloads.open).toBe(true);
  });
});
