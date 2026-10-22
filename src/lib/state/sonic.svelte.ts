import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorDetail, errorMessage } from "#lib/appErrors.js";
import { m } from "#lib/paraglide/messages.js";
import { player } from "#lib/state/player.svelte.js";
import { session } from "#lib/state/session.svelte.js";
import { toast } from "#lib/state/toast.svelte.js";
import { onVisible } from "#lib/visibility.js";
import type { SonicScanStatus } from "#lib/types.js";

/** What the backend says when a track has no sound vector yet… */
const NOT_ANALYSED = "sonic:not-analysed";
/** …and when it was analysed, but is too short or too quiet to have one. */
const NO_SOUND = "sonic:no-sound";
/** …and when too little of the library is analysed to fill a radio or path. */
const TOO_FEW_ANALYSED = "sonic:too-few";

/** Steps between the two ends of a sonic path. */
export const PATH_STEPS = 10;

/** The signed-in account, by server and user: what an item id belongs to. */
function account(): string | null {
  const info = session.info;
  return info ? `${info.serverUrl}\u0001${info.userId}` : null;
}

/**
 * Sonic similarity from the UI's side (`src-tauri/src/sonic.rs`): the
 * library scan's status, a radio from one track, and a path between two —
 * picked in two right-clicks, "from here" and then "to here".
 */
class SonicStore {
  status = $state<SonicScanStatus | null>(null);
  #pathStart = $state<{ id: string; name: string; account: string | null } | null>(null);
  #initialized = false;

  /**
   * The first end of a path being picked. Only in the session it was picked
   * in: after a sign-out or a switch to another server or account its item
   * id means nothing, and offering a path from it would fail as "not
   * analysed".
   */
  get pathStart(): { id: string; name: string } | null {
    const start = this.#pathStart;
    return start && start.account === account() ? { id: start.id, name: start.name } : null;
  }

  async init() {
    if (this.#initialized) return;
    this.#initialized = true;
    await listen<SonicScanStatus>("sonic:scan", (event) => {
      this.status = event.payload;
    });
    this.refresh();
    // A pass that ended while the window was hidden said so to a suspended
    // WebView: read the status back when the window returns.
    onVisible(() => this.refresh());
  }

  refresh() {
    invoke<SonicScanStatus>("sonic_status")
      .then((status) => (this.status = status))
      .catch(() => {});
  }

  async setScan(enabled: boolean) {
    await invoke("sonic_set_scan", { enabled });
    this.refresh();
  }

  /** `noSound`: what to say when a track it needs has no sound to compare. */
  #fail(e: unknown, noSound: () => string) {
    const message = errorDetail(e);
    if (message.includes(NO_SOUND)) {
      toast.show(noSound(), { kind: "info", ms: 6000 });
    } else if (message.includes(NOT_ANALYSED)) {
      toast.show(m.sonic_not_analysed(), { kind: "info", ms: 8000 });
    } else if (message.includes(TOO_FEW_ANALYSED)) {
      toast.show(m.sonic_too_few(), { kind: "info", ms: 6000 });
    } else {
      player.error = errorMessage(e);
    }
  }

  async radio(itemId: string) {
    try {
      await invoke("sonic_radio", { itemId });
    } catch (e) {
      this.#fail(e, () => m.sonic_no_sound());
    }
  }

  /** Pick the first end; a start picked before is replaced. */
  startPath(id: string, name: string) {
    this.#pathStart = { id, name, account: account() };
    toast.show(m.sonic_path_pick_target({ name }), { kind: "info", ms: 6000 });
  }

  cancelPath() {
    this.#pathStart = null;
  }

  async finishPath(toId: string) {
    const start = this.pathStart;
    this.#pathStart = null;
    if (!start) return;
    try {
      await invoke("sonic_path", { fromId: start.id, toId, steps: PATH_STEPS });
    } catch (e) {
      this.#fail(e, () => m.sonic_path_no_sound());
    }
  }
}

export const sonic = new SonicStore();
