import { flushSync, mount, unmount } from "svelte";
import { afterEach, describe, expect, it } from "vitest";
import BarList from "#lib/components/BarList.svelte";

let instance: Record<string, unknown> | null = null;

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  document.body.innerHTML = "";
});

describe("BarList", () => {
  it("shows two rows that share a label", () => {
    // Two artists of one name (two linked ones, or a linked one and the
    // album-artist name of an untagged track): a label is no identity.
    const host = document.createElement("div");
    document.body.appendChild(host);
    instance = mount(BarList, {
      target: host,
      props: {
        items: [
          { label: "John Williams", value: 3, valueLabel: "3 plays", href: "/artist/j1" },
          { label: "John Williams", value: 1, valueLabel: "1 play", href: "/artist/j2" },
        ],
      },
    });
    flushSync();
    const links = [...host.querySelectorAll("a")].map((a) => a.getAttribute("href"));
    expect(links).toEqual(["/artist/j1", "/artist/j2"]);
  });
});
