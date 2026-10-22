import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "#lib/api.js";
import { errorMessage } from "#lib/appErrors.js";
import { onVisible } from "#lib/visibility.js";
import type { DownloadRequest, DownloadSnapshot, DownloadTask } from "#lib/types.js";

/**
 * How many rows of each kind the panel lists — waiting, failed or
 * cancelled, completed. A batch of thousands of tracks would otherwise put
 * thousands of rows into it; what lies beyond is a count.
 */
export const PANEL_ROWS = 50;

export interface DownloadPanelView {
  /** The running downloads, then the next ones to run. */
  active: DownloadTask[];
  /** Waiting downloads beyond those in `active`. */
  moreQueued: number;
  /** Failed and cancelled downloads, the latest first: they wait for a
   *  retry, so they come before the completed ones. */
  stopped: DownloadTask[];
  /** Failed and cancelled downloads beyond those in `stopped`. */
  moreStopped: number;
  /** The last downloads to complete, the latest first. */
  completed: DownloadTask[];
  /** Downloads that completed before those. */
  moreCompleted: number;
}

function isActive(task: DownloadTask): boolean {
  return task.status === "queued" || task.status === "downloading";
}

function latestFirst(a: DownloadTask, b: DownloadTask): number {
  return b.revision - a.revision;
}

/**
 * What the panel shows of the list: every running download, the next `rows`
 * waiting ones, the last `rows` that failed or were cancelled and the last
 * `rows` that completed. A finished task changes no more, so its revision
 * says when it finished.
 */
export function panelView(tasks: readonly DownloadTask[], rows = PANEL_ROWS): DownloadPanelView {
  const running: DownloadTask[] = [];
  const queued: DownloadTask[] = [];
  const stopped: DownloadTask[] = [];
  const completed: DownloadTask[] = [];
  for (const task of tasks) {
    if (task.status === "downloading") running.push(task);
    else if (task.status === "queued") queued.push(task);
    else if (task.status === "completed") completed.push(task);
    else stopped.push(task);
  }
  stopped.sort(latestFirst);
  completed.sort(latestFirst);
  return {
    active: [...running, ...queued.slice(0, rows)],
    moreQueued: Math.max(0, queued.length - rows),
    stopped: stopped.slice(0, rows),
    moreStopped: Math.max(0, stopped.length - rows),
    completed: completed.slice(0, rows),
    moreCompleted: Math.max(0, completed.length - rows),
  };
}

function countTasks(tasks: readonly DownloadTask[]) {
  let active = 0;
  let stopped = 0;
  for (const task of tasks) {
    if (isActive(task)) active++;
    else if (task.status !== "completed") stopped++;
  }
  return { active, stopped, finished: tasks.length - active };
}

/**
 * The downloads, as Rust reports them: the whole list with `download:state`
 * when tasks come or go (and from `get_downloads` on start and when the
 * window returns), single tasks with `download:changed` — progress at most
 * twice a second, a change of status at once.
 *
 * Every copy of a task carries the revision of its last change, and a list
 * the revision it was taken at. Whatever order copies arrive in, the newer
 * one wins, and a list older than the one applied is dropped — a
 * `get_downloads` answer can be overtaken by events sent after it.
 */
class DownloadsState {
  /** In the backend's order. Replaced, never changed in place: a change to
   *  one task swaps just that object, so only its row is drawn again. */
  tasks = $state.raw<DownloadTask[]>([]);
  open = $state(false);
  error = $state<string | null>(null);
  #counts = $derived(countTasks(this.tasks));
  /** What the panel lists (see `panelView`); worked out only while shown. */
  panel = $derived(panelView(this.tasks));
  private unlisten: UnlistenFn | null = null;
  private stopResync: (() => void) | null = null;
  private initializing: Promise<void> | null = null;
  /** The revision of the newest whole list applied. */
  #revision = -1;
  /** The newest revision a change of a task this store does not know had:
   *  the list that added the task never arrived. */
  #wanted = -1;
  #syncing: Promise<void> | null = null;
  /** Positions by task id, in `#indexed` (the list they were taken of). */
  #index = new Map<string, number>();
  #indexed: readonly DownloadTask[] | null = null;

  get activeCount() {
    return this.#counts.active;
  }

  get finishedCount() {
    return this.#counts.finished;
  }

  /** Failed and cancelled downloads, which "Retry all" puts back. */
  get stoppedCount() {
    return this.#counts.stopped;
  }

  init(): Promise<void> {
    if (this.initializing) return this.initializing;
    // Progress and the end of a download arrive as events only, and those may
    // be lost while the window is hidden: read the list back when it returns.
    this.stopResync = onVisible(() => {
      this.#resync().catch(() => {});
    });
    this.initializing = (async () => {
      const stops = await Promise.all([
        listen<DownloadSnapshot>("download:state", (event) => this.#applyList(event.payload)),
        listen<DownloadTask[]>("download:changed", (event) => this.#applyChanges(event.payload)),
      ]);
      this.unlisten = () => stops.forEach((stop) => stop());
      await this.#resync();
    })().catch((error) => {
      this.error = errorMessage(error);
    });
    return this.initializing;
  }

  destroy() {
    this.unlisten?.();
    this.unlisten = null;
    this.stopResync?.();
    this.stopResync = null;
    this.initializing = null;
  }

  /** On sign-out: the next account starts with an empty panel (Rust has
   *  cancelled and dropped the downloads themselves). The revision stays: a
   *  late word about the old list must not bring it back. */
  reset() {
    this.tasks = [];
    this.error = null;
    this.open = false;
  }

  #positions(): Map<string, number> {
    if (this.#indexed !== this.tasks) {
      this.#index = new Map(this.tasks.map((task, position) => [task.id, position]));
      this.#indexed = this.tasks;
    }
    return this.#index;
  }

  #applyList(list: DownloadSnapshot) {
    // Older than a list applied already: an answer that newer events overtook.
    if (list.revision < this.#revision) return;
    const held = this.#positions();
    const current = this.tasks;
    this.tasks = list.tasks.map((task) => {
      const position = held.get(task.id);
      const mine = position === undefined ? undefined : current[position];
      // A change sent after this list was taken, and here already.
      return mine && mine.revision > task.revision ? mine : task;
    });
    this.#revision = list.revision;
  }

  #applyChanges(changes: readonly DownloadTask[]) {
    const held = this.#positions();
    let next: DownloadTask[] | null = null;
    for (const change of changes) {
      const position = held.get(change.id);
      if (position === undefined) {
        // Newer than the list: the list that added it went missing. Older:
        // the task was cleared since.
        if (change.revision > this.#revision) this.#wanted = Math.max(this.#wanted, change.revision);
        continue;
      }
      if ((next ?? this.tasks)[position].revision >= change.revision) continue;
      next ??= [...this.tasks];
      next[position] = change;
    }
    if (next) {
      this.tasks = next;
      // The same tasks in the same places.
      this.#indexed = next;
    }
    if (this.#wanted > this.#revision) this.#resync().catch(() => {});
  }

  /** Read the whole list back — once more should a change seen meanwhile be
   *  newer than the answer, which is taken when asked for, not when it lands. */
  #resync(): Promise<void> {
    this.#syncing ??= (async () => {
      try {
        do this.#applyList(await api.getDownloads());
        while (this.#wanted > this.#revision);
      } finally {
        this.#syncing = null;
      }
    })();
    return this.#syncing;
  }

  async enqueue(requests: DownloadRequest[]) {
    if (requests.length === 0) return;
    this.error = null;
    this.open = true;
    try {
      await this.init();
      const queued = await api.queueDownloads(requests);
      // The list with them is usually here already. If not, show them now —
      // never over a newer copy, and not once a newer list left them out.
      const held = this.#positions();
      const fresh = queued.filter((task) => !held.has(task.id) && task.revision > this.#revision);
      if (fresh.length > 0) this.tasks = [...this.tasks, ...fresh];
    } catch (error) {
      this.error = errorMessage(error);
      throw error;
    }
  }

  async cancel(id: string) {
    try {
      await api.cancelDownload(id);
    } catch (error) {
      this.error = errorMessage(error);
    }
  }

  async retry(id: string) {
    this.open = true;
    try {
      await api.retryDownload(id);
    } catch (error) {
      this.error = errorMessage(error);
    }
  }

  /** Retry every failed and cancelled download — one call for any number. */
  async retryAll() {
    this.open = true;
    try {
      await api.retryFailedDownloads();
    } catch (error) {
      this.error = errorMessage(error);
    }
  }

  /** Cancel every queued and running download — also the waiting ones the
   *  panel does not list. */
  async cancelAll() {
    try {
      await api.cancelAllDownloads();
    } catch (error) {
      this.error = errorMessage(error);
    }
  }

  async clearFinished() {
    try {
      await api.clearFinishedDownloads();
    } catch (error) {
      this.error = errorMessage(error);
    }
  }

  async openFolder() {
    try {
      await api.openDownloadsFolder();
    } catch (error) {
      this.error = errorMessage(error);
    }
  }
}

export const downloads = new DownloadsState();
