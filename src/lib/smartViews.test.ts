import { beforeEach, describe, expect, it } from "vitest";
import type { SmartFilter } from "$lib/api/discovery";
import type { SessionInfo } from "$lib/types";
import {
  cloneSmartFilter,
  emptySmartFilter,
  loadSmartViews,
  newSmartView,
  persistSmartViews,
  smartYearProblem,
} from "./smartViews";

const years = (yearFrom: number | null, yearTo: number | null): SmartFilter => ({
  ...emptySmartFilter(),
  yearFrom,
  yearTo,
});

const session: SessionInfo = {
  serverUrl: "https://music.example",
  username: "u",
  userId: "user-1",
  canDelete: false,
  canEdit: false,
};

beforeEach(() => localStorage.clear());

describe("cloneSmartFilter", () => {
  it("sanitizes junk values", () => {
    const dirty = {
      yearFrom: Number.NaN,
      yearTo: 1999,
      genreIds: ["a", "a", "", 5, "b"],
      played: "bogus",
      favoriteOnly: 1,
      minPlayCount: Infinity,
      addedSince: "not-a-date",
    } as unknown as SmartFilter;
    expect(cloneSmartFilter(dirty)).toEqual({
      yearFrom: null,
      yearTo: 1999,
      genreIds: ["a", "b"],
      played: "all",
      favoriteOnly: true,
      minPlayCount: null,
      addedSince: null,
    });
  });

  it("rounds and clamps the numeric fields to the backend's integer bounds", () => {
    const filter = {
      ...emptySmartFilter(),
      yearFrom: 1999.6,
      yearTo: 1e12,
      minPlayCount: 2.4,
    } as SmartFilter;
    expect(cloneSmartFilter(filter)).toMatchObject({ yearFrom: 2000, yearTo: 9999, minPlayCount: 2 });

    const low = { ...emptySmartFilter(), yearFrom: -5, yearTo: 0, minPlayCount: -3.7 } as SmartFilter;
    expect(cloneSmartFilter(low)).toMatchObject({ yearFrom: 1, yearTo: 1, minPlayCount: 0 });

    const huge = { ...emptySmartFilter(), minPlayCount: 3e10 } as SmartFilter;
    expect(cloneSmartFilter(huge).minPlayCount).toBe(2_147_483_647);
  });

  it("keeps a valid ISO date and played state", () => {
    const filter = { ...emptySmartFilter(), played: "unplayed", addedSince: "2026-01-31" } as SmartFilter;
    const cloned = cloneSmartFilter(filter);
    expect(cloned.played).toBe("unplayed");
    expect(cloned.addedSince).toBe("2026-01-31");
  });
});

describe("smartYearProblem", () => {
  it("accepts open, single and in-range bounds", () => {
    expect(smartYearProblem(emptySmartFilter())).toBeNull();
    expect(smartYearProblem(years(1990, null))).toBeNull();
    expect(smartYearProblem(years(1990, 1999))).toBeNull();
    expect(smartYearProblem(years(1700, 2000))).toBeNull(); // exactly 300 years
  });

  it("flags a start after the end", () => {
    expect(smartYearProblem(years(2000, 1990))).toBe("order");
  });

  it("mirrors the backend's 300-year span limit", () => {
    expect(smartYearProblem(years(1699, 2000))).toBe("span");
  });

  it("reports years the input shows but cloneSmartFilter would change", () => {
    expect(smartYearProblem(years(0, 1999))).toBe("range");
    expect(smartYearProblem(years(1990, 10000))).toBe("range");
    expect(smartYearProblem(years(-5, null))).toBe("range");
    expect(smartYearProblem(years(1990.5, 1999))).toBe("range");
  });
});

describe("newSmartView", () => {
  it("trims the name and assigns an id + timestamps", () => {
    const view = newSmartView("  My mix  ", emptySmartFilter());
    expect(view.name).toBe("My mix");
    expect(view.id).toMatch(/.+/);
    expect(view.createdAt).toBe(view.updatedAt);
  });
});

describe("persist + load round-trip", () => {
  it("survives a store round-trip and is scoped per server/user", () => {
    const view = newSmartView("Fresh", { ...emptySmartFilter(), favoriteOnly: true });
    persistSmartViews(session, [view]);

    const loaded = loadSmartViews(session);
    expect(loaded).toHaveLength(1);
    expect(loaded[0].name).toBe("Fresh");
    expect(loaded[0].filter.favoriteOnly).toBe(true);

    // A different user sees nothing.
    expect(loadSmartViews({ ...session, userId: "other" })).toEqual([]);
  });

  it("drops structurally invalid entries", () => {
    localStorage.setItem(
      `jellysic:smart-views:v1:${encodeURIComponent(session.serverUrl)}:${encodeURIComponent(session.userId)}`,
      JSON.stringify([{ id: "x" }, { nope: true }]),
    );
    expect(loadSmartViews(session)).toEqual([]);
  });
});
