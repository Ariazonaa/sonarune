import { untrack } from "svelte";
import { listen } from "@tauri-apps/api/event";
import { api } from "#lib/api.js";
import { onVisible } from "#lib/visibility.js";

/**
 * Tracks server-side music-library changes and the background sync status.
 * Rust polls the server every 30 s (and on window focus) and emits
 * `library:changed` when the library differs; each change bumps `revision` so
 * views that show library data refetch. `library:sync` carries the poll
 * lifecycle (`checking` / `idle` / `error`), surfaced as a status indicator.
 *
 * The event carries the watcher's running count of changes, which
 * `check_library_now` returns too. The poll goes on while the window is
 * hidden, and what it announces then may never reach the suspended WebView,
 * while its baseline moves on regardless: on focus and when the window comes
 * back, a count ahead of the last one seen here bumps `revision` all the same.
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
    this.#check();
  }

  #initialized = false;
  #spinnerTimer: ReturnType<typeof setTimeout> | undefined;
  /** The watcher's change count as last seen here. */
  #seen = 0;

  /** Poll now, and catch up on a change this page never heard of. */
  #check() {
    api.checkLibraryNow().then(
      (count) => this.#catchUp(count),
      () => {},
    );
  }

  #catchUp(count: number) {
    if (typeof count !== "number" || count <= this.#seen) return;
    this.#seen = count;
    this.revision++;
  }

  async init() {
    if (this.#initialized) return;
    this.#initialized = true;

    await listen<number>("library:changed", (event) => {
      this.#catchUp(event.payload);
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
    window.addEventListener("focus", () => this.#check());
    onVisible(() => this.#check());

    // Populate the status shortly after launch rather than waiting a full
    // tick. What the watcher found before this page existed is in the data
    // the views load now, so its count is the starting point, not news.
    api.checkLibraryNow().then(
      (count) => (this.#seen = Math.max(this.#seen, count)),
      () => {},
    );
  }
}

export const library = new LibraryStore();

/**
 * Run `fn` on every library change after the calling component was created.
 * Call it during component initialisation.
 *
 * The revision current at that moment is skipped: the view loads its data on
 * mount anyway, and `revision` never drops back to 0, so reloading for it too
 * would fetch everything twice on every visit once anything changed this
 * session. `fn` runs untracked; `revision` is the only dependency.
 */
export function onLibraryChange(fn: () => void): void {
  let seen = untrack(() => library.revision);
  $effect(() => {
    const revision = library.revision;
    if (revision === seen) return;
    seen = revision;
    untrack(fn);
  });
}
