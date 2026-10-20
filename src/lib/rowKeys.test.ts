import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import Harness from "#lib/rowKeys.harness.svelte";
import { activatesRow } from "./rowKeys";

// A clickable track row (album, playlist, smart view) plays on Enter, but
// leaves Space to the global play/pause shortcut and leaves the keys of the
// link and button inside it to them. Through a mounted row, so the check
// runs in Svelte's delegated keydown handler as it does on the pages.

let instance: Record<string, unknown> | null = null;
let onplay: Mock<() => void>;
/** Whether the keydown reached the window still uncancelled — where the
 *  global shortcut handler (`shortcuts.ts`) would act on it. */
let reachedWindow: boolean[] = [];
const onWindowKeydown = (event: KeyboardEvent) => reachedWindow.push(!event.defaultPrevented);

beforeEach(() => {
  onplay = vi.fn<() => void>();
  reachedWindow = [];
  const host = document.createElement("div");
  document.body.appendChild(host);
  instance = mount(Harness, { target: host, props: { onplay } });
  flushSync();
  window.addEventListener("keydown", onWindowKeydown);
});

afterEach(() => {
  window.removeEventListener("keydown", onWindowKeydown);
  if (instance) {
    unmount(instance);
    instance = null;
  }
  document.body.innerHTML = "";
});

const byId = (id: string) => document.getElementById(id)!;

/** Presses a key on `target`, as a real keyboard would with it focused. */
function press(target: HTMLElement, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  target.focus();
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  flushSync();
  return event;
}

describe("track row keys", () => {
  it("plays the row on Enter and claims the key", () => {
    const event = press(byId("row"), "Enter");
    expect(onplay).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves Space to the global play/pause shortcut", () => {
    // A click focuses the row; the next Space must pause, not restart it.
    const event = press(byId("row"), " ");
    expect(onplay).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
    expect(reachedWindow).toEqual([true]);
  });

  it("leaves Enter on a link or button inside the row to that control", () => {
    for (const id of ["link", "heart"]) {
      const event = press(byId(id), "Enter");
      expect(event.defaultPrevented, id).toBe(false);
    }
    expect(onplay).not.toHaveBeenCalled();
  });

  it("ignores Enter with a modifier and a held-down Enter", () => {
    for (const init of [{ ctrlKey: true }, { altKey: true }, { shiftKey: true }, { metaKey: true }, { repeat: true }]) {
      expect(press(byId("row"), "Enter", init).defaultPrevented, JSON.stringify(init)).toBe(false);
    }
    expect(onplay).not.toHaveBeenCalled();
  });
});

describe("activatesRow", () => {
  it("reads the row from currentTarget, not from where the key landed", () => {
    const row = document.createElement("div");
    const child = document.createElement("span");
    row.append(child);
    const seen: boolean[] = [];
    row.addEventListener("keydown", (event) => seen.push(activatesRow(event)));
    row.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    child.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(seen).toEqual([true, false]);
  });
});
