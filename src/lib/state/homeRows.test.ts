import { describe, expect, it } from "vitest";
import { cleanRows, moveRow, swapRows, HOME_ROWS, type HomeRowId } from "./homeRows.svelte";

describe("cleanRows", () => {
  it("keeps a stored order", () => {
    const order: HomeRowId[] = ["mostPlayed", "recentlyAdded", "forgottenFavorites", "recentlyPlayed"];
    expect(cleanRows({ order, hidden: [] }).order).toEqual(order);
  });

  it("appends a row the stored order has never seen", () => {
    // What an upgrade looks like: the setting was saved before the row existed.
    const { order } = cleanRows({ order: ["mostPlayed", "recentlyPlayed"], hidden: [] });
    expect(order).toEqual(["mostPlayed", "recentlyPlayed", "recentlyAdded", "forgottenFavorites"]);
    expect(new Set(order)).toEqual(new Set(HOME_ROWS));
  });

  it("drops what it cannot use", () => {
    const { order, hidden } = cleanRows({
      order: ["mostPlayed", "mostPlayed", "nonsense", 7],
      hidden: ["recentlyAdded", "nonsense", "recentlyAdded"],
    });
    expect(order.filter((id) => id === "mostPlayed")).toHaveLength(1);
    expect(order).toHaveLength(HOME_ROWS.length);
    expect(hidden).toEqual(["recentlyAdded"]);
  });

  it("falls back to the defaults for junk", () => {
    for (const value of [null, undefined, 42, "nope", []]) {
      expect(cleanRows(value).order).toEqual([...HOME_ROWS]);
      expect(cleanRows(value).hidden).toEqual([]);
    }
  });
});

describe("moveRow", () => {
  const order = [...HOME_ROWS];

  it("moves by one in either direction", () => {
    expect(moveRow(order, "recentlyAdded", -1)).toEqual([
      "recentlyAdded",
      "recentlyPlayed",
      "mostPlayed",
      "forgottenFavorites",
    ]);
    expect(moveRow(order, "recentlyPlayed", 1)).toEqual([
      "recentlyAdded",
      "recentlyPlayed",
      "mostPlayed",
      "forgottenFavorites",
    ]);
  });

  it("stops at the ends instead of wrapping", () => {
    expect(moveRow(order, "recentlyPlayed", -1)).toEqual(order);
    expect(moveRow(order, "forgottenFavorites", 1)).toEqual(order);
    expect(moveRow(order, "recentlyPlayed", 99)).toEqual([
      "recentlyAdded",
      "mostPlayed",
      "forgottenFavorites",
      "recentlyPlayed",
    ]);
  });
});

describe("swapRows", () => {
  it("exchanges two rows wherever they sit", () => {
    // What the home page does: swap with the next *visible* row, so a hidden
    // row in between does not swallow the move.
    expect(swapRows([...HOME_ROWS], "recentlyPlayed", "mostPlayed")).toEqual([
      "mostPlayed",
      "recentlyAdded",
      "recentlyPlayed",
      "forgottenFavorites",
    ]);
  });

  it("leaves the order alone when a row is not in it", () => {
    const order: HomeRowId[] = ["recentlyPlayed", "mostPlayed"];
    expect(swapRows(order, "recentlyPlayed", "forgottenFavorites")).toBe(order);
  });
});
