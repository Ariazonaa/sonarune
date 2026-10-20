import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "#lib/api.js";
import { onVisible } from "#lib/visibility.js";
import type { DownloadRequest, DownloadTask } from "#lib/types.js";

class DownloadsState {
  tasks = $state<DownloadTask[]>([]);
  open = $state(false);
  error = $state<string | null>(null);
  private unlisten: UnlistenFn | null = null;
  private stopResync: (() => void) | null = null;
  private initializing: Promise<void> | null = null;

  get activeCount() {
    return this.tasks.filter((task) => task.status === "queued" || task.status === "downloading").length;
  }

  get finishedCount() {
    return this.tasks.length - this.activeCount;
  }

  init(): Promise<void> {
    if (this.initializing) return this.initializing;
    // Progress and the end of a download arrive as events only, and those may
    // be lost while the window is hidden: read the list back when it returns.
    this.stopResync = onVisible(() => {
      api.getDownloads().then(
        (tasks) => (this.tasks = tasks),
        () => {},
      );
    });
    this.initializing = (async () => {
      this.unlisten = await listen<DownloadTask[]>("download:state", (event) => {
        this.tasks = event.payload;
      });
      this.tasks = await api.getDownloads();
    })().catch((error) => {
      this.error = String(error);
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
   *  cancelled and dropped the downloads themselves). */
  reset() {
    this.tasks = [];
    this.error = null;
    this.open = false;
  }

  async enqueue(requests: DownloadRequest[]) {
    if (requests.length === 0) return;
    this.error = null;
    this.open = true;
    try {
      await this.init();
      const queued = await api.queueDownloads(requests);
      const byId = new Map(this.tasks.map((task) => [task.id, task]));
      for (const task of queued) byId.set(task.id, task);
      this.tasks = [...byId.values()];
    } catch (error) {
      this.error = String(error);
      throw error;
    }
  }

  async cancel(id: string) {
    try {
      await api.cancelDownload(id);
    } catch (error) {
      this.error = String(error);
    }
  }

  async retry(id: string) {
    this.open = true;
    try {
      await api.retryDownload(id);
    } catch (error) {
      this.error = String(error);
    }
  }

  async clearFinished() {
    try {
      await api.clearFinishedDownloads();
    } catch (error) {
      this.error = String(error);
    }
  }

  async openFolder() {
    try {
      await api.openDownloadsFolder();
    } catch (error) {
      this.error = String(error);
    }
  }
}

export const downloads = new DownloadsState();
