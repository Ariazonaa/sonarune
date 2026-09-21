import { flushSync, mount, tick, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import Harness from "$lib/components/Popover.harness.svelte";

// The popover renders itself into <body> (see portal.ts) because every glass
// surface is a containing block for position: fixed. Moving a node out of
// Svelte's DOM is the risky part: these tests pin that it lands in <body>,
// dismisses like the menus it replaced, and leaves nothing behind — including
// when it shares its {#if} block with a sibling. Menu keyboard behaviour is
// covered end to end in ContextMenu.test.ts.

let instance: Record<string, unknown> | null = null;
let host: HTMLElement;

beforeEach(() => {
  // jsdom has no layout: give elements a size so the viewport clamp is real.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(100);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(50);
  host = document.createElement("div");
  host.id = "host";
  document.body.appendChild(host);
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

function render(props: { anchored?: boolean; role?: "menu" | "dialog"; onclose?: () => void } = {}) {
  instance = mount(Harness, { target: host, props });
  flushSync();
}

const menu = () => document.querySelector<HTMLElement>('[role="menu"]');
const byId = (id: string) => document.getElementById(id);

describe("Popover", () => {
  it("renders into <body>, outside the page that declares it", () => {
    render();
    expect(menu()?.parentElement).toBe(document.body);
    expect(host.contains(menu())).toBe(false);
    expect(host.contains(byId("sibling"))).toBe(true);
  });

  it("keeps a point-anchored popover inside the viewport", () => {
    render();
    // Opened at 5000/5000 with a 100x50 box in a 1024x768 jsdom window.
    expect(menu()?.style.left).toBe(`${window.innerWidth - 100 - 4}px`);
    expect(menu()?.style.top).toBe(`${window.innerHeight - 50 - 4}px`);
    expect(menu()?.style.visibility).toBe("visible");
  });

  it("closes on a press outside and removes every node it created", () => {
    const onclose = vi.fn();
    render({ onclose });
    byId("before")!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    flushSync();
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(menu()).toBeNull();
    expect(byId("inside")).toBeNull();
    // The sibling of the same block went with it; the nodes around it stay.
    expect(byId("sibling")).toBeNull();
    expect(byId("before")).not.toBeNull();
    expect(byId("after")).not.toBeNull();
    expect(host.querySelector(".contents")).toBeNull();
  });

  it("stays open for presses inside and scrolls inside", () => {
    const onclose = vi.fn();
    render({ onclose });
    byId("inside")!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    menu()!.dispatchEvent(new Event("scroll"));
    flushSync();
    expect(onclose).not.toHaveBeenCalled();
    expect(menu()).not.toBeNull();
  });

  it("closes on Escape and on a scroll elsewhere", () => {
    const onclose = vi.fn();
    render({ onclose });
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    flushSync();
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(menu()).toBeNull();

    const again = vi.fn();
    unmount(instance!);
    render({ onclose: again });
    host.dispatchEvent(new Event("scroll"));
    flushSync();
    expect(again).toHaveBeenCalledTimes(1);
  });

  it("ignores presses on its anchor, so the anchor's own toggle works", () => {
    const onclose = vi.fn();
    render({ anchored: true, onclose });
    byId("anchor")!.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    flushSync();
    expect(onclose).not.toHaveBeenCalled();
    expect(menu()).not.toBeNull();
  });

  it("unmounting the page also removes the popover from <body>", () => {
    render();
    unmount(instance!);
    instance = null;
    flushSync();
    expect(menu()).toBeNull();
    expect(host.innerHTML).toBe("");
  });

  it("a menu takes focus as it opens; a dialog popover leaves focus and keys alone", async () => {
    render();
    await tick();
    await tick();
    // No menuitems in the harness: the menu itself holds focus.
    expect(document.activeElement).toBe(menu());

    unmount(instance!);
    render({ role: "dialog" });
    await tick();
    await tick();
    const dialog = document.querySelector('[role="dialog"]');
    expect(dialog).not.toBeNull();
    expect(dialog?.contains(document.activeElement)).toBe(false);

    const pageHandler = vi.fn();
    window.addEventListener("keydown", pageHandler);
    try {
      byId("inside")!.focus();
      byId("inside")!.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }),
      );
    } finally {
      window.removeEventListener("keydown", pageHandler);
    }
    expect(pageHandler).toHaveBeenCalledTimes(1);
    expect(pageHandler.mock.calls[0][0].defaultPrevented).toBe(false);
  });
});
