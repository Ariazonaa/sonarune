import { describe, expect, it } from "vitest";
import { decodeDrag, encodeDrag } from "./dragToPlaylist";

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
