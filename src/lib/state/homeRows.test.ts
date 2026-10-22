import { describe, expect, it } from "vitest";
import { cleanRows, moveAmong, moveRow, swapRows, HOME_ROWS, type HomeRowId } from "./homeRows.svelte";

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

describe("moveAmong", () => {
  const order: HomeRowId[] = ["recentlyPlayed", "forgottenFavorites", "recentlyAdded", "mostPlayed"];

  it("swaps with the next row the page draws, past one it does not", () => {
    // No favorites, so the home page drops the empty "forgotten favorites"
    // row: "move down" on the first row must land below "recently added".
    const shown: HomeRowId[] = ["recentlyPlayed", "recentlyAdded", "mostPlayed"];
    expect(moveAmong(order, shown, "recentlyPlayed", 1)).toEqual([
      "recentlyAdded",
      "forgottenFavorites",
      "recentlyPlayed",
      "mostPlayed",
    ]);
    expect(moveAmong(order, shown, "recentlyAdded", -1)).toEqual([
      "recentlyAdded",
      "forgottenFavorites",
      "recentlyPlayed",
      "mostPlayed",
    ]);
  });

  it("leaves the order alone at the ends and for rows not drawn", () => {
    const shown: HomeRowId[] = ["recentlyPlayed", "mostPlayed"];
    expect(moveAmong(order, shown, "recentlyPlayed", -1)).toBe(order);
    expect(moveAmong(order, shown, "mostPlayed", 1)).toBe(order);
    expect(moveAmong(order, shown, "recentlyAdded", 1)).toBe(order);
  });
});

describe("the way back to a hidden row", () => {
  // A hidden row has nothing left to click on the home page, so its tooltip
  // and the "rows hidden" state name the place in the settings where it comes
  // back. They have to name it the way the settings label it.
  const catalogs = Object.entries(
    import.meta.glob("/messages/*.json", { import: "default", eager: true }) as Record<
      string,
      Record<string, string>
    >,
  );

  it("names the settings section and list as they are labelled", () => {
    expect(catalogs.map(([path]) => path).sort()).toEqual(["/messages/de.json", "/messages/en.json"]);
    for (const [, messages] of catalogs) {
      const place = [messages.settings_title, messages.settings_layout, messages.settings_home_rows].join(" → ");
      expect(messages.home_rows_hidden_hint).toContain(place);
      expect(messages.home_row_hide_hint).toContain(place);
    }
  });
});
