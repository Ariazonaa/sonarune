import { api } from "$lib/api";
import { library } from "$lib/state/library.svelte";

/**
 * Server reachability, for the offline banner. A cheap `check_server` ping
 * runs on window focus, after a failed action, and after a *successful* action
 * while offline (to recover). The Retry button re-checks and, on success,
 * pokes the library so the current view refetches.
 */
class NetStore {
  online = $state(true);
  /** Coalesces concurrent checks so a Retry during an in-flight focus check
   *  still resolves to the real result (not a stale field). */
  #inflight: Promise<boolean> | null = null;
  #initialized = false;

  init() {
    if (this.#initialized) return;
    this.#initialized = true;
    window.addEventListener("focus", () => void this.check());
  }

  check(): Promise<boolean> {
    if (this.#inflight) return this.#inflight;
    const p = (async () => {
      try {
        return (this.online = await api.checkServer());
      } catch {
        return (this.online = false);
      } finally {
        this.#inflight = null;
      }
    })();
    this.#inflight = p;
    return p;
  }

  /** Ping only when we currently believe we're offline (cheap recovery path). */
  recheckIfOffline() {
    if (!this.online) void this.check();
  }

  async retry() {
    if (await this.check()) library.poke();
  }
}

export const net = new NetStore();
