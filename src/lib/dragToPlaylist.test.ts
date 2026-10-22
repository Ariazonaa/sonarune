import { describe, expect, it } from "vitest";
import { DRAG_MIME, decodeDrag, encodeDrag, isOurDrag, readDrag, startDrag, type DragPayload } from "./dragToPlaylist";

describe("the drag payload", () => {
  it("survives the round trip", () => {
    for (const payload of [
      { trackIds: ["a", "b"] },
      { albumId: "album-1" },
      { trackIds: ["a"], label: "One Track" },
    ]) {
      expect(decodeDrag(encodeDrag(payload))).toEqual(payload);
    }
  });

  it("refuses anything that is not one of ours", () => {
    // Text dragged in from outside, an empty drag, a payload that names
    // nothing to add: all of them must leave the drop target cold.
    for (const text of [
      null,
      undefined,
      "",
      "https://example.com",
      "{",
      "[]",
      "42",
      JSON.stringify({}),
      JSON.stringify({ trackIds: [] }),
      JSON.stringify({ trackIds: "nope" }),
      JSON.stringify({ albumId: 7 }),
    ]) {
      expect(decodeDrag(text)).toBeNull();
    }
  });

  it("keeps only the ids it can use", () => {
    expect(decodeDrag(JSON.stringify({ trackIds: ["a", 1, null, "b"] }))).toEqual({
      trackIds: ["a", "b"],
    });
  });
});

describe("whose drag it is", () => {
  function fakeDrag(payload: DragPayload): DragEvent {
    const store = new Map<string, string>([[DRAG_MIME, encodeDrag(payload)]]);
    const dataTransfer = {
      effectAllowed: "none",
      get types() {
        return [...store.keys()];
      },
      getData: (type: string) => store.get(type) ?? "",
      setData: (type: string, value: string) => void store.set(type, value),
    };
    return { dataTransfer } as unknown as DragEvent;
  }

  it("ends when its source ends it, even after the source left the page", () => {
    const row = document.body.appendChild(document.createElement("div"));
    const drag = { ...fakeDrag({ trackIds: ["t1"] }), target: row } as unknown as DragEvent;
    startDrag(drag, { trackIds: ["t1"] });
    row.remove();
    row.dispatchEvent(new Event("dragend"));
    expect(isOurDrag(drag)).toBe(false);
  });

  it("is not ours when it only carries our type: another app can set it", () => {
    const forged = fakeDrag({ trackIds: ["t1"] });
    expect(isOurDrag(forged)).toBe(false);
    expect(readDrag(forged)).toBeNull();
  });

  it("is ours from the start of a drag here until it ends", () => {
    const drag = fakeDrag({ trackIds: ["t1"] });
    startDrag(drag, { trackIds: ["t1"] });
    expect(isOurDrag(drag)).toBe(true);
    expect(readDrag(drag)).toEqual({ trackIds: ["t1"] });
    window.dispatchEvent(new Event("dragend"));
    expect(isOurDrag(drag)).toBe(false);

    startDrag(drag, { trackIds: ["t1"] });
    window.dispatchEvent(new Event("drop"));
    expect(readDrag(drag)).toBeNull();
  });
});
