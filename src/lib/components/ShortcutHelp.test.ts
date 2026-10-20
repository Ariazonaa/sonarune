import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it } from "vitest";
import ShortcutHelp from "#lib/components/ShortcutHelp.svelte";

let instance: Record<string, unknown> | null = null;

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  document.body.innerHTML = "";
});

describe("ShortcutHelp", () => {
  it("takes focus from the page while open and gives it back after", () => {
    // "?" opens it from wherever focus was — an album card's play button,
    // which Space or Enter would otherwise press behind the backdrop.
    const card = document.createElement("button");
    document.body.appendChild(card);
    card.focus();

    instance = mount(ShortcutHelp, { target: document.body });
    flushSync();
    const dialog = document.querySelector('[role="dialog"]');
    expect(document.activeElement).toBe(dialog);

    unmount(instance);
    instance = null;
    flushSync();
    expect(document.activeElement).toBe(card);
  });
});
