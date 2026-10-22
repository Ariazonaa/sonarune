import { listen } from "@tauri-apps/api/event";
import { api } from "#lib/api.js";
import { onVisible } from "#lib/visibility.js";
import type { UpdateInfo } from "#lib/types.js";

interface Progress {
  downloaded: number;
  total: number | null;
}

/**
 * In-app updates (src-tauri/src/updater.rs). Rust checks the release endpoint
 * once a while after start — unless that is switched off — and emits
 * `updater:available` when something newer exists; the settings page checks on
 * demand through the same store, so both paths show one state. That event is
 * a one-off, and with the window hidden or minimized at that moment it goes
 * to a suspended WebView, so Rust keeps the answer and the store reads it back
 * when it starts and whenever the window comes back.
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
  /** Why the last check or install failed, as it was rejected; shown next
   *  to the button in the words of `updateErrors.ts`. */
  error = $state<unknown>(null);
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

    void this.#readBack();
    onVisible(() => void this.#readBack());
  }

  /** What the last check found, without asking the endpoint again. */
  async #readBack() {
    try {
      const known = await api.getKnownUpdate();
      // A check running right now answers for itself.
      if (known && !this.checking) this.info = known;
    } catch {
      /* the announcement is a courtesy; Settings can still check */
    }
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
      this.error = e;
    } finally {
      this.checking = false;
    }
  }

  /** Drop what the last check said, after a switch of the channel: it was
   *  the other channel's answer. Rust keeps its copy per channel. */
  forget() {
    if (this.checking || this.installing) return;
    this.info = null;
    this.error = null;
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
      this.error = e;
      this.installing = false;
      this.progress = null;
    }
  }
}

export const updates = new UpdateStore();
