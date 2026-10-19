import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { m } from "#lib/paraglide/messages.js";
import { player } from "#lib/state/player.svelte.js";
import { toast } from "#lib/state/toast.svelte.js";
import type { SonicScanStatus } from "#lib/types.js";

/** What the backend says when a track has no sound vector yet. */
const NOT_ANALYSED = "sonic:not-analysed";

/** Steps between the two ends of a sonic path. */
export const PATH_STEPS = 10;

/**
 * Sonic similarity from the UI's side (`src-tauri/src/sonic.rs`): the
 * library scan's status, a radio from one track, and a path between two —
 * picked in two right-clicks, "from here" and then "to here".
 */
class SonicStore {
  status = $state<SonicScanStatus | null>(null);
  /** The first end of a path being picked. */
  pathStart = $state<{ id: string; name: string } | null>(null);
  #initialized = false;

  async init() {
    if (this.#initialized) return;
    this.#initialized = true;
    await listen<SonicScanStatus>("sonic:scan", (event) => {
      this.status = event.payload;
    });
    this.refresh();
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

  #fail(e: unknown) {
    if (String(e).includes(NOT_ANALYSED)) {
      toast.show(m.sonic_not_analysed(), { kind: "info", ms: 6000 });
    } else {
      player.error = String(e);
    }
  }

  async radio(itemId: string) {
    try {
      await invoke("sonic_radio", { itemId });
    } catch (e) {
      this.#fail(e);
    }
  }

  startPath(id: string, name: string) {
    this.pathStart = { id, name };
    toast.show(m.sonic_path_pick_target({ name }), { kind: "info", ms: 6000 });
  }

  cancelPath() {
    this.pathStart = null;
  }

  async finishPath(toId: string) {
    const start = this.pathStart;
    if (!start) return;
    this.pathStart = null;
    try {
      await invoke("sonic_path", { fromId: start.id, toId, steps: PATH_STEPS });
    } catch (e) {
      this.#fail(e);
    }
  }
}

export const sonic = new SonicStore();
