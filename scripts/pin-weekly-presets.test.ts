// @vitest-environment node
import { describe, expect, it } from "vitest";
import { manifestTexts } from "./pin-weekly-presets.mjs";

const file = (c: string) => `${c.repeat(32)}.json`;
const hash = (c: string) => c.repeat(64);

describe("pin-weekly-presets manifests", () => {
  const pins = new Map([
    [file("b"), hash("1")],
    [file("a"), hash("2")],
  ]);

  it("writes the pins sorted by file name", () => {
    const { manifest } = manifestTexts(pins, []);
    expect(manifest.endsWith("\n")).toBe(true);
    expect(Object.entries(JSON.parse(manifest))).toEqual([
      [file("a"), hash("2")],
      [file("b"), hash("1")],
    ]);
  });

  // --allow-missing: the package still lists these, so the drift check in
  // presets.test.ts has to learn that they are gone rather than unpinned.
  it("lists the files left out as gone, sorted", () => {
    const { manifest, missing } = manifestTexts(pins, [file("d"), file("c")]);
    expect(Object.keys(JSON.parse(manifest))).toEqual([file("a"), file("b")]);
    expect(missing).not.toBeNull();
    expect(JSON.parse(missing!)).toEqual([file("c"), file("d")]);
  });

  it("writes no list when nothing was left out", () => {
    expect(manifestTexts(pins, []).missing).toBeNull();
  });
});
