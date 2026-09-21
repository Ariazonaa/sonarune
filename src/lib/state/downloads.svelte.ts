import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "$lib/api";
import type { DownloadRequest, DownloadTask } from "$lib/types";

class DownloadsState {
  tasks = $state<DownloadTask[]>([]);
  open = $state(false);
  error = $state<string | null>(null);
  private unlisten: UnlistenFn | null = null;
  private initializing: Promise<void> | null = null;

  get activeCount() {
    return this.tasks.filter((task) => task.status === "queued" || task.status === "downloading").length;
  }

  get finishedCount() {
    return this.tasks.length - this.activeCount;
  }

  init(): Promise<void> {
    if (this.initializing) return this.initializing;
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
    this.initializing = null;
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
