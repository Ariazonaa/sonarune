import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Keep the Tauri-backed store out: the shortcuts only call into it, so a
// recording stand-in shows which action ran.
const player = vi.hoisted(() => {
  const done = () => Promise.resolve();
  return {
    run: (_: Promise<unknown>) => {},
    toggle: vi.fn(done),
    next: vi.fn(done),
    prev: vi.fn(done),
    seekBy: vi.fn(done),
    nudgeVolume: vi.fn(done),
    toggleMute: vi.fn(done),
    toggleCurrentFavorite: vi.fn(),
    toggleShuffle: vi.fn(done),
    cycleRepeat: vi.fn(done),
    jumpToCurrent: vi.fn(),
  };
});
vi.mock("#lib/state/player.svelte.js", () => ({ player }));

import { blockedByOverlay, installShortcuts } from "./shortcuts";
import { isModalOpen } from "./modal";
import { confirm } from "#lib/state/confirm.svelte.js";
import { help } from "#lib/state/help.svelte.js";
import { palette } from "#lib/state/palette.svelte.js";
import { shortcutSettings } from "#lib/state/shortcuts.svelte.js";

function addDialog(modal: boolean): HTMLElement {
  const element = document.createElement("div");
  element.setAttribute("role", "dialog");
  if (modal) element.setAttribute("aria-modal", "true");
  document.body.append(element);
  return element;
}

function askConfirm() {
  void confirm.ask({ title: "t", body: "b", confirmLabel: "ok" });
}

afterEach(() => {
  confirm.answer(false);
  palette.close();
  help.close();
  document.body.replaceChildren();
});

describe("isModalOpen", () => {
  it("sees an aria-modal dialog but not a non-modal popover", () => {
    expect(isModalOpen()).toBe(false);
    addDialog(false);
    expect(isModalOpen()).toBe(false);
    addDialog(true);
    expect(isModalOpen()).toBe(true);
  });

  it("counts a pending confirmation", () => {
    askConfirm();
    expect(isModalOpen()).toBe(true);
  });
});

describe("blockedByOverlay", () => {
  it("lets shortcuts through with nothing open", () => {
    expect(blockedByOverlay("playPause")).toBe(false);
  });

  it("blocks every shortcut while a modal dialog is open", () => {
    addDialog(true);
    expect(blockedByOverlay("playPause")).toBe(true);
    expect(blockedByOverlay("palette")).toBe(true);
  });

  it("blocks even the palette toggle while a confirmation is pending", () => {
    palette.show();
    askConfirm();
    expect(blockedByOverlay("palette")).toBe(true);
  });

  it("leaves the palette and the help overlay their own toggle", () => {
    palette.show();
    addDialog(true);
    expect(blockedByOverlay("palette")).toBe(false);
    expect(blockedByOverlay("next")).toBe(true);

    palette.close();
    help.toggle();
    expect(blockedByOverlay("help")).toBe(false);
    expect(blockedByOverlay("mute")).toBe(true);
  });
});

describe("installShortcuts", () => {
  let uninstall: () => void;

  beforeEach(() => {
    vi.clearAllMocks();
    uninstall = installShortcuts();
  });

  afterEach(() => uninstall());

  /** A control with focus, as after a click on it. */
  function focused(html: string): HTMLElement {
    document.body.insertAdjacentHTML("beforeend", html);
    const element = document.body.lastElementChild as HTMLElement;
    element.focus();
    return element;
  }

  /** Presses a key on whatever has focus, as a real keyboard would. */
  function press(key: string, init: KeyboardEventInit = {}): KeyboardEvent {
    const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
    (document.activeElement ?? document.body).dispatchEvent(event);
    return event;
  }

  it("runs the bound action and takes the key from the page", () => {
    const event = press(" ");
    expect(player.toggle).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves a focused slider its own keys and nothing more", () => {
    // A dragged volume slider keeps focus; Space, letters and chords must
    // still reach the shortcuts.
    focused('<input type="range" min="0" max="1" step="0.005">');
    expect(press(" ").defaultPrevented).toBe(true);
    expect(player.toggle).toHaveBeenCalledTimes(1);
    press("m");
    expect(player.toggleMute).toHaveBeenCalledTimes(1);
    press("ArrowRight", { ctrlKey: true });
    expect(player.next).toHaveBeenCalledTimes(1);

    // The arrows move the slider, as they natively do.
    expect(press("ArrowUp").defaultPrevented).toBe(false);
    expect(press("ArrowRight").defaultPrevented).toBe(false);
    expect(player.nudgeVolume).not.toHaveBeenCalled();
    expect(player.seekBy).not.toHaveBeenCalled();
  });

  it("leaves a checkbox its Space, and a text field every key", () => {
    focused('<input type="checkbox">');
    expect(press(" ").defaultPrevented).toBe(false);
    press("n");
    expect(player.toggle).not.toHaveBeenCalled();
    expect(player.next).toHaveBeenCalledTimes(1);

    focused('<input type="text">');
    for (const key of ["n", " ", "ArrowRight", "?"]) expect(press(key).defaultPrevented).toBe(false);
    expect(player.next).toHaveBeenCalledTimes(1);
    expect(player.toggle).not.toHaveBeenCalled();
    expect(player.seekBy).not.toHaveBeenCalled();
    expect(help.open).toBe(false);
  });

  it("opens and closes the palette with its chord while typing", () => {
    // The palette focuses its own field: Ctrl+K has to close it from there.
    focused('<input type="search">');
    const open = press("k", { ctrlKey: true });
    expect(palette.open).toBe(true);
    expect(open.defaultPrevented).toBe(true);
    // Ctrl+P would otherwise reach the WebView's print shortcut.
    expect(press("p", { ctrlKey: true }).defaultPrevented).toBe(true);
    expect(palette.open).toBe(false);
  });

  it("leaves AltGr to a text field even when the palette is bound to Ctrl+Alt", () => {
    // AltGr arrives as Ctrl+Alt and types characters (@, €, { on a German
    // keyboard). Unbound, such a chord never reaches the text-field check, so
    // bind one to the palette.
    expect(shortcutSettings.addBinding("palette", "Mod+Alt+K")).toEqual({ kind: "ok" });
    try {
      const field = focused('<input type="search">');
      expect(press("k", { ctrlKey: true, altKey: true }).defaultPrevented).toBe(false);
      expect(palette.open).toBe(false);

      // Away from a text field the same chord is an ordinary shortcut.
      field.blur();
      expect(press("k", { ctrlKey: true, altKey: true }).defaultPrevented).toBe(true);
      expect(palette.open).toBe(true);
    } finally {
      shortcutSettings.removeBinding("palette", "Mod+Alt+K");
    }
  });

  it("keeps the palette chord from the WebView while a dialog keeps the palette shut", () => {
    // Typing in the metadata editor: Ctrl+P must neither open the palette over
    // the dialog nor reach the WebView's print shortcut.
    const dialog = addDialog(true);
    dialog.innerHTML = '<input type="text"><button type="button">Cancel</button>';
    dialog.querySelector("input")!.focus();
    expect(press("p", { ctrlKey: true }).defaultPrevented).toBe(true);
    expect(palette.open).toBe(false);
    // Typing a "p" (bound to "previous") stays typing.
    expect(press("p").defaultPrevented).toBe(false);

    // The same behind a confirmation and the shortcut help.
    dialog.remove();
    askConfirm();
    expect(press("k", { ctrlKey: true }).defaultPrevented).toBe(true);
    confirm.answer(false);
    help.toggle();
    expect(press("p", { ctrlKey: true }).defaultPrevented).toBe(true);
    expect(palette.open).toBe(false);
    expect(help.open).toBe(true);
  });

  it("leaves a dialog's button the plain key the palette is bound to", () => {
    // Only the chord is taken from a dialog: Space rebound to the palette must
    // still press the focused Cancel button.
    shortcutSettings.removeBinding("playPause", "Space");
    expect(shortcutSettings.addBinding("palette", "Space")).toEqual({ kind: "ok" });
    try {
      const dialog = addDialog(true);
      dialog.innerHTML = '<button type="button">Cancel</button>';
      dialog.querySelector("button")!.focus();
      expect(press(" ").defaultPrevented).toBe(false);
      expect(palette.open).toBe(false);
    } finally {
      shortcutSettings.removeBinding("palette", "Space");
      shortcutSettings.addBinding("playPause", "Space");
    }
  });

  it("does not repeat a toggle while its key is held, but keeps seeking and volume going", () => {
    press("m");
    const repeat = press("m", { repeat: true });
    expect(player.toggleMute).toHaveBeenCalledTimes(1);
    // Swallowed all the same, so a focused button cannot act on it either.
    expect(repeat.defaultPrevented).toBe(true);
    press("l", { repeat: true });
    expect(player.toggleCurrentFavorite).not.toHaveBeenCalled();

    press("ArrowRight");
    press("ArrowRight", { repeat: true });
    expect(player.seekBy).toHaveBeenCalledTimes(2);
    press("ArrowUp", { repeat: true });
    expect(player.nudgeVolume).toHaveBeenCalledWith(0.05);
  });
});
