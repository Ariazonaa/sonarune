import { listen } from "@tauri-apps/api/event";
import { api } from "$lib/api";
import type { UpdateInfo } from "$lib/types";

interface Progress {
  downloaded: number;
  total: number | null;
}

/**
 * In-app updates (src-tauri/src/updater.rs). Rust checks the release endpoint
 * once a while after start — unless that is switched off — and emits
 * `updater:available` when something newer exists; the settings page checks on
 * demand through the same store, so both paths show one state.
 *
 * The install itself closes the app, so it is never automatic and never runs
 * while a track is playing. Rust enforces that; the UI only explains it.
 */
class UpdateStore {
  /** Null until a check ran (or one was announced). */
  info = $state<UpdateInfo | null>(null);
  checking = $state(false);
  installing = $state(false);
  progress = $state<Progress | null>(null);
  /** Message of the last failed check or install, shown next to the button. */
  error = $state<string | null>(null);
  /** Set once the user has seen the announcement, so it does not reappear on
   *  every navigation — a new version resets it. */
  announced = $state<string | null>(null);

  get available(): boolean {
    return this.info?.version != null;
  }

  #initialized = false;

  async init() {
    if (this.#initialized) return;
    this.#initialized = true;

    await listen<UpdateInfo>("updater:available", (event) => {
      this.info = event.payload;
    });

    await listen<Progress>("updater:progress", (event) => {
      this.progress = event.payload;
    });
  }

  /** Check on demand. Errors stay in the store; a failed check changes nothing
   *  else, the app does not depend on the update server. */
  async check(): Promise<void> {
    if (this.checking) return;
    this.checking = true;
    this.error = null;
    try {
      this.info = await api.checkForUpdate();
    } catch (e) {
      this.error = String(e);
    } finally {
      this.checking = false;
    }
  }

  /**
   * Download and install. On success this does not return — the installer
   * takes over and closes the app.
   */
  async install(): Promise<void> {
    if (this.installing) return;
    this.installing = true;
    this.error = null;
    this.progress = null;
    try {
      await api.installUpdate();
    } catch (e) {
      this.error = String(e);
      this.installing = false;
      this.progress = null;
    }
  }
}

export const updates = new UpdateStore();
