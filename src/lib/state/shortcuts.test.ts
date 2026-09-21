import { beforeEach, describe, expect, it } from "vitest";
import {
  isReservedShortcut,
  shortcutBindingFromEvent,
  shortcutSettings,
} from "./shortcuts.svelte";

function key(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent("keydown", init);
}

beforeEach(() => {
  localStorage.clear();
  shortcutSettings.resetAll();
});

describe("shortcutBindingFromEvent", () => {
  it("normalizes modifiers and named keys", () => {
    expect(shortcutBindingFromEvent(key({ key: "k", ctrlKey: true }))).toBe("Mod+K");
    expect(shortcutBindingFromEvent(key({ key: "ArrowRight", ctrlKey: true }))).toBe("Mod+ArrowRight");
    expect(shortcutBindingFromEvent(key({ key: " " }))).toBe("Space");
  });

  it("does not add Shift for a printable symbol that needs it", () => {
    expect(shortcutBindingFromEvent(key({ key: "?", shiftKey: true }))).toBe("?");
  });

  it("ignores bare modifier presses", () => {
    expect(shortcutBindingFromEvent(key({ key: "Control", ctrlKey: true }))).toBeNull();
  });
});

describe("isReservedShortcut", () => {
  it("guards OS/browser and editing keys", () => {
    expect(isReservedShortcut("F5")).toBe(true);
    expect(isReservedShortcut("Mod+A")).toBe(true);
    expect(isReservedShortcut("Escape")).toBe(true);
    expect(isReservedShortcut("N")).toBe(false);
  });
});

describe("shortcutSettings.addBinding", () => {
  it("rejects reserved bindings", () => {
    expect(shortcutSettings.addBinding("next", "F5")).toEqual({ kind: "reserved", binding: "F5" });
  });

  it("reports a conflict with another action's binding", () => {
    // Space is the default for playPause.
    const result = shortcutSettings.addBinding("next", "Space");
    expect(result).toEqual({ kind: "conflict", binding: "Space", action: "playPause" });
  });

  it("adds a free binding", () => {
    expect(shortcutSettings.addBinding("next", "G")).toEqual({ kind: "ok" });
    expect(shortcutSettings.bindings.next).toContain("G");
  });
});
