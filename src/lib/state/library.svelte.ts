import { listen } from "@tauri-apps/api/event";
import { api } from "$lib/api";

/**
 * Tracks server-side music-library changes and the background sync status.
 * Rust polls the server every 30 s (and on window focus) and emits
 * `library:changed` when the library differs; each event bumps `revision` so
 * views that show library data refetch. `library:sync` carries the poll
 * lifecycle (`checking` / `idle` / `error`), surfaced as a status indicator.
 */
class LibraryStore {
  /** Incremented on every detected server-side library change. */
  revision = $state(0);
  /** True while a server check is actively running — debounced so the routine
   *  fast poll doesn't blink the spinner. */
  syncing = $state(false);
  /** Epoch ms of the last successful sync, or null before the first one. */
  lastSyncedAt = $state<number | null>(null);

  /** Force the views that watch `revision` to refetch (e.g. after reconnect). */
  poke() {
    this.revision++;
  }

  /** Trigger an immediate library check (e.g. clicking the status). */
  syncNow() {
    api.checkLibraryNow().catch(() => {});
  }

  #initialized = false;
  #spinnerTimer: ReturnType<typeof setTimeout> | undefined;

  async init() {
    if (this.#initialized) return;
    this.#initialized = true;

    await listen("library:changed", () => {
      this.revision++;
    });

    await listen<string>("library:sync", (event) => {
      if (event.payload === "checking") {
        // Only reveal the spinner if the check takes a real moment; a routine
        // poll resolves in a few ms and clears this timer before it fires.
        clearTimeout(this.#spinnerTimer);
        this.#spinnerTimer = setTimeout(() => (this.syncing = true), 300);
      } else {
        clearTimeout(this.#spinnerTimer);
        this.syncing = false;
        if (event.payload === "idle") this.lastSyncedAt = Date.now();
      }
    });

    // Poll immediately when the user returns to the app, so a change made
    // while away shows up right away instead of waiting for the next tick.
    window.addEventListener("focus", () => {
      api.checkLibraryNow().catch(() => {});
    });

    // Populate the status shortly after launch rather than waiting a full tick.
    api.checkLibraryNow().catch(() => {});
  }
}

export const library = new LibraryStore();
