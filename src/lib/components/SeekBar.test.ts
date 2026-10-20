import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import SeekBar from "#lib/components/SeekBar.svelte";
import { SEEK_STEP_MS } from "#lib/seek.js";

let instance: Record<string, unknown> | null = null;

function render(onseek: (positionMs: number) => void) {
  instance = mount(SeekBar, {
    target: document.body,
    props: { positionMs: 60_000, durationMs: 200_000, label: "Seek", onseek },
  });
  flushSync();
  return document.querySelector<HTMLElement>('[role="slider"]')!;
}

function press(target: HTMLElement, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  document.body.innerHTML = "";
});

describe("SeekBar keys", () => {
  it("seeks with the slider keys and keeps them from the global shortcuts", () => {
    const onseek = vi.fn();
    const rail = render(onseek);
    const event = press(rail, "ArrowRight");
    expect(onseek).toHaveBeenCalledWith(60_000 + SEEK_STEP_MS);
    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves chords to the shortcuts: Ctrl+→ is the next track, not a seek", () => {
    // A mouse seek leaves the bar focused, so this is what a click then
    // Ctrl+→ hits.
    const onseek = vi.fn();
    const rail = render(onseek);
    for (const init of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }, { shiftKey: true }]) {
      expect(press(rail, "ArrowRight", init).defaultPrevented).toBe(false);
      expect(press(rail, "ArrowLeft", init).defaultPrevented).toBe(false);
    }
    expect(onseek).not.toHaveBeenCalled();
  });
});
