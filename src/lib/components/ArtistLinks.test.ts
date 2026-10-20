import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The router is SvelteKit's; only whether the link hands it the path matters.
const goto = vi.hoisted(() => vi.fn(() => Promise.resolve()));
vi.mock("$app/navigation", () => ({ goto }));

import ArtistLinks from "#lib/components/ArtistLinks.svelte";

// An artist link sits inside a clickable track row. These tests pin that a
// click opens the artist in the app — not as a new document, which restarted
// the whole app — and never reaches the row.

let instance: Record<string, unknown> | null = null;
let host: HTMLElement;
let rowClick: ReturnType<typeof vi.fn<(event: MouseEvent) => void>>;

function render(props: { artists: { id: string; name: string }[]; fallback: string }) {
  instance = mount(ArtistLinks, { target: host, props });
  flushSync();
}

beforeEach(() => {
  goto.mockClear();
  rowClick = vi.fn<(event: MouseEvent) => void>();
  // The row's click handler sits above the links, as on the album page.
  const row = document.createElement("div");
  row.addEventListener("click", rowClick);
  host = document.createElement("p");
  row.appendChild(host);
  document.body.appendChild(row);
  render({
    artists: [
      { id: "a1", name: "Alpha" },
      { id: "b2", name: "Bravo" },
    ],
    fallback: "Alpha, Bravo",
  });
});

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  document.body.innerHTML = "";
});

const link = (name: string) => Array.from(host.querySelectorAll("a")).find((a) => a.textContent === name)!;

function click(target: HTMLElement, init: MouseEventInit = {}): MouseEvent {
  const event = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ...init });
  target.dispatchEvent(event);
  return event;
}

describe("ArtistLinks", () => {
  it("opens the artist in the app and keeps the click away from the row", () => {
    const event = click(link("Bravo"));
    expect(goto).toHaveBeenCalledWith("/artist/b2");
    // Handled here: the WebView must not follow the href as a new document.
    expect(event.defaultPrevented).toBe(true);
    expect(rowClick).not.toHaveBeenCalled();
  });

  it("leaves a modified click to the WebView, still away from the row", () => {
    // Runs after the component's own (delegated) handler on the same element;
    // records what it left behind, then keeps jsdom from following the link.
    let leftToWebView: boolean | null = null;
    host.addEventListener("click", (event) => {
      leftToWebView = !event.defaultPrevented;
      event.preventDefault();
    });
    click(link("Alpha"), { ctrlKey: true });
    expect(goto).not.toHaveBeenCalled();
    expect(leftToWebView).toBe(true);
    expect(rowClick).not.toHaveBeenCalled();
  });

  it("shows the plain names without linkable artists", () => {
    unmount(instance!);
    render({ artists: [], fallback: "Various" });
    expect(host.querySelector("a")).toBeNull();
    expect(host.textContent).toBe("Various");
  });
});
