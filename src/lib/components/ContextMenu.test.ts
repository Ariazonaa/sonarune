import { flushSync, mount, tick, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ContextMenu from "#lib/components/ContextMenu.svelte";
import Harness from "#lib/components/ContextMenu.harness.svelte";

// Keyboard access to context menus, end to end through a row with
// `contextMenuKey`, ContextMenu and Popover: opening from the keyboard or the
// pointer, the WAI-ARIA menu keys, and where focus goes when the menu closes.
// The harness menu is Alpha (disabled), Bravo, Charlie (disabled), Delta.

let instance: Record<string, unknown> | null = null;
let host: HTMLElement;

beforeEach(() => {
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

function render(onpick: (label: string) => void = () => {}, opensDialog = false) {
  instance = mount(Harness, { target: host, props: { onpick, opensDialog } });
  flushSync();
}

const menu = () => document.querySelector<HTMLElement>('[role="menu"]');
const row = () => document.getElementById("row")!;
const focusedLabel = () => document.activeElement?.textContent?.trim();

/** Presses a key on whatever has focus, as a real keyboard would. */
function press(key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  (document.activeElement ?? document.body).dispatchEvent(event);
  flushSync();
  return event;
}

/** A menu takes focus a tick after it opens (once measured and visible). */
async function settle() {
  await tick();
  await tick();
}

async function openWithKeyboard(key = "ContextMenu", init: KeyboardEventInit = {}) {
  row().focus();
  const event = press(key, init);
  await settle();
  return event;
}

describe("ContextMenu keyboard access", () => {
  it("opens from the Menu key under the focused row, on its first enabled item", async () => {
    render();
    vi.spyOn(row(), "getBoundingClientRect").mockReturnValue({
      left: 120,
      top: 280,
      right: 320,
      bottom: 300,
      width: 200,
      height: 20,
      x: 120,
      y: 280,
      toJSON: () => ({}),
    });
    const event = await openWithKeyboard();
    expect(event.defaultPrevented).toBe(true);
    expect(menu()?.style.left).toBe("120px");
    expect(menu()?.style.top).toBe("304px");
    expect(focusedLabel()).toBe("Bravo");
  });

  it("opens on Shift+F10 too, with disabled items marked aria-disabled", async () => {
    render();
    await openWithKeyboard("F10", { shiftKey: true });
    const items = Array.from(menu()?.querySelectorAll('[role="menuitem"]') ?? []);
    expect(items.map((item) => item.getAttribute("aria-disabled"))).toEqual(["true", null, "true", null]);
    expect(focusedLabel()).toBe("Bravo");
  });

  it("moves with the arrow keys, wrapping and skipping disabled items; Home/End and type-ahead jump", async () => {
    render();
    await openWithKeyboard();
    press("ArrowDown");
    expect(focusedLabel()).toBe("Delta");
    press("ArrowDown");
    expect(focusedLabel()).toBe("Bravo");
    press("ArrowUp");
    expect(focusedLabel()).toBe("Delta");
    press("Home");
    expect(focusedLabel()).toBe("Bravo");
    press("End");
    expect(focusedLabel()).toBe("Delta");
    press("b");
    expect(focusedLabel()).toBe("Bravo");
  });

  it("activates the focused item on Enter, closes, and returns focus to the row", async () => {
    const onpick = vi.fn();
    render(onpick);
    await openWithKeyboard();
    press("ArrowDown");
    const event = press("Enter");
    expect(event.defaultPrevented).toBe(true);
    expect(onpick).toHaveBeenCalledTimes(1);
    expect(onpick).toHaveBeenCalledWith("Delta");
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(row());
  });

  it("activates on Space; a disabled item does nothing", async () => {
    const onpick = vi.fn();
    render(onpick);
    await openWithKeyboard();
    menu()?.querySelector<HTMLElement>('[aria-disabled="true"]')?.click();
    flushSync();
    expect(onpick).not.toHaveBeenCalled();
    expect(menu()).not.toBeNull();
    press(" ");
    expect(onpick).toHaveBeenCalledWith("Bravo");
  });

  it("closes on Escape with focus back on the row", async () => {
    render();
    await openWithKeyboard();
    press("ArrowDown");
    press("Escape");
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(row());
  });

  it("closes on Tab, handing focus to the row for Tab to move on from", async () => {
    render();
    await openWithKeyboard();
    const event = press("Tab");
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(row());
    expect(event.defaultPrevented).toBe(false);
  });

  it("opened from the keyboard, closes when the row's list scrolls, whatever lies below the row", async () => {
    // The menu opens just below the row; below the last visible row of the
    // page that point is on the player bar. The page's scroll must still
    // close it rather than leave it floating while the row scrolls away.
    const playerBar = document.createElement("div");
    document.body.appendChild(playerBar);
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: () => playerBar });
    try {
      render();
      await openWithKeyboard();
      playerBar.dispatchEvent(new Event("scroll"));
      flushSync();
      expect(menu()).not.toBeNull();

      host.dispatchEvent(new Event("scroll"));
      flushSync();
      expect(menu()).toBeNull();
    } finally {
      delete (document as { elementFromPoint?: unknown }).elementFromPoint;
    }
  });

  it("opened with the pointer, focuses the menu itself so ArrowUp starts from the bottom", async () => {
    render();
    row().focus(); // the WebView focuses a pressed button
    row().dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 2 }));
    row().dispatchEvent(
      new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 300, clientY: 200 }),
    );
    flushSync();
    await settle();
    expect(menu()?.style.left).toBe("300px");
    expect(menu()?.style.top).toBe("200px");
    expect(document.activeElement).toBe(menu());
    press("ArrowUp");
    expect(focusedLabel()).toBe("Delta");
  });

  it("does not pull focus back to the row when a press elsewhere closes it", async () => {
    render();
    await openWithKeyboard();
    document.getElementById("elsewhere")?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    flushSync();
    expect(menu()).toBeNull();
    expect(document.activeElement).not.toBe(row());
  });

  it("hands focus to a dialog an item opens, and back to the row when it closes", async () => {
    // The menu returns focus to the row as it closes; a dialog opened by the
    // item in the same update must still end up with it — otherwise Space or
    // Enter would play the row behind the backdrop.
    render(() => {}, true);
    await openWithKeyboard();
    press("Enter");
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(document.getElementById("dialog"));

    document.getElementById("dialog-close")!.click();
    flushSync();
    expect(document.getElementById("dialog")).toBeNull();
    expect(document.activeElement).toBe(row());
  });

  it("lets the button that opened it close it again", async () => {
    render();
    const more = document.getElementById("more")!;
    more.click();
    flushSync();
    await settle();
    expect(menu()).not.toBeNull();
    // A press on the trigger is not "outside": its click toggles instead of
    // the press closing the menu and the click opening it again.
    more.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    flushSync();
    expect(menu()).not.toBeNull();
    more.click();
    flushSync();
    expect(menu()).toBeNull();
  });

  it("keeps its keys away from page handlers and global shortcuts", async () => {
    render();
    await openWithKeyboard();
    const shortcut = vi.fn();
    window.addEventListener("keydown", shortcut);
    try {
      for (const key of ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End", "m", " "]) {
        press(key);
      }
    } finally {
      window.removeEventListener("keydown", shortcut);
    }
    expect(shortcut).not.toHaveBeenCalled();
  });
});

describe("ContextMenu items", () => {
  it("renders items whose labels repeat", () => {
    // One "Go to artist" entry per credit: a name credited twice gives two
    // equal labels, which as keys made Svelte throw and no menu open at all.
    const action = vi.fn();
    instance = mount(ContextMenu, {
      target: host,
      props: {
        x: 10,
        y: 10,
        items: [
          { label: "Go to artist: Foo", action },
          { label: "Go to artist: Bar", action },
          { label: "Go to artist: Foo", action },
        ],
        onclose: () => {},
      },
    });
    flushSync();
    const labels = Array.from(menu()?.querySelectorAll('[role="menuitem"]') ?? []).map((item) =>
      item.textContent?.trim(),
    );
    expect(labels).toEqual(["Go to artist: Foo", "Go to artist: Bar", "Go to artist: Foo"]);
  });
});
