import { describe, expect, it } from "vitest";
import { sameNames, splitNames } from "./metadataNames";

describe("splitNames", () => {
  it("splits on commas, trims, drops empties and repeats", () => {
    expect(splitNames(" Rock,Jazz , ,rock, Blues ")).toEqual(["Rock", "Jazz", "Blues"]);
    expect(splitNames("")).toEqual([]);
  });

  it("keeps a name the item has whole, even with a comma in it", () => {
    const known = ["Earth, Wind & Fire"];
    // The text box as the editor fills it: nothing changes.
    expect(splitNames("Earth, Wind & Fire", known)).toEqual(["Earth, Wind & Fire"]);
    // Another artist added next to it.
    expect(splitNames("Earth, Wind & Fire, Ramsey Lewis", known)).toEqual([
      "Earth, Wind & Fire",
      "Ramsey Lewis",
    ]);
    expect(splitNames("Ramsey Lewis,Earth,Wind & Fire", known)).toEqual([
      "Ramsey Lewis",
      "Earth, Wind & Fire",
    ]);
  });

  it("splits what the user typed into something new", () => {
    // Without the name it came from, a comma separates.
    expect(splitNames("Earth, Wind & Fire")).toEqual(["Earth", "Wind & Fire"]);
    // An edited name is no longer the one the item has.
    expect(splitNames("Earth, Wind and Fire", ["Earth, Wind & Fire"])).toEqual([
      "Earth",
      "Wind and Fire",
    ]);
  });

  it("prefers the known name that covers the most", () => {
    const known = ["Folk, World", "Folk, World, & Country"];
    expect(splitNames("Folk, World, & Country, Jazz", known)).toEqual([
      "Folk, World, & Country",
      "Jazz",
    ]);
    expect(splitNames("Folk, World, Jazz", known)).toEqual(["Folk, World", "Jazz"]);
  });
});

describe("sameNames", () => {
  it("compares lists in order", () => {
    expect(sameNames(["a", "b"], ["a", "b"])).toBe(true);
    expect(sameNames(["a", "b"], ["b", "a"])).toBe(false);
    expect(sameNames(["Earth, Wind & Fire"], ["Earth", "Wind & Fire"])).toBe(false);
    expect(sameNames([], [])).toBe(true);
  });
});
