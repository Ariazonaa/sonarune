import { api } from "#lib/api.js";
import { library } from "#lib/state/library.svelte.js";

/**
 * Server reachability, for the offline banner. A cheap `check_server` ping
 * runs once the session is restored (an unreachable server restores like any
 * other), on window focus, after a failed action, and after a *successful*
 * action while offline (to recover). Whichever check finds the server back —
 * those, or the banner's Retry — pokes the library so the views that failed
 * meanwhile refetch.
 */
class NetStore {
  online = $state(true);
  /** The certificate the server presents when the session does not trust it
   *  (a pinned one renewed, or something on the network presenting its own).
   *  Its own banner: a Retry alone never gets past it. */
  untrustedCertificate = $state<string | null>(null);
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
      let online: boolean;
      let untrusted: string | null;
      try {
        const result = await api.checkServer();
        online = result.online;
        untrusted = result.untrustedCertificate;
      } catch {
        online = false;
        untrusted = null;
      } finally {
        this.#inflight = null;
      }
      this.untrustedCertificate = untrusted;
      if (online && !this.online) library.poke();
      return (this.online = online);
    })();
    this.#inflight = p;
    return p;
  }

  /** A session that ends takes its banner along. The next one starts from a
   *  server it has just reached — not, until the next check, with the last
   *  one's outage or with a fingerprint to trust that it never saw. */
  reset() {
    this.online = true;
    this.untrustedCertificate = null;
  }

  /** Ping only when we currently believe we're offline (cheap recovery path). */
  recheckIfOffline() {
    if (!this.online) void this.check();
  }

  async retry() {
    await this.check();
  }
}

export const net = new NetStore();
