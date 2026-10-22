import { describe, expect, it } from "vitest";
import { dropSlot, moveItem, moveTarget } from "./dragReorder";
import { isLongList, LONG_LIST_ROWS } from "./longList";

describe("dropSlot", () => {
  const rect = { top: 480, height: 48 };

  it("lands in front of a row over its upper half, behind it over the lower", () => {
    expect(dropSlot(481, rect, 10)).toBe(10);
    expect(dropSlot(503, rect, 10)).toBe(10);
    expect(dropSlot(504, rect, 10)).toBe(11);
    expect(dropSlot(527, rect, 10)).toBe(11);
  });
});

describe("moveTarget", () => {
  it("shifts a drop below the origin up by one: the row leaves its place first", () => {
    // Row 2 dropped behind row 1005 (slot 1006) ends up at 1005.
    expect(moveTarget(2, 1006)).toBe(1005);
    expect(moveTarget(1005, 2)).toBe(2);
  });

  it("is no move when the row is dropped next to itself", () => {
    expect(moveTarget(7, 7)).toBeNull();
    expect(moveTarget(7, 8)).toBeNull();
    expect(moveTarget(7, 9)).toBe(8);
  });

  it("appends at the end", () => {
    expect(moveTarget(0, 5)).toBe(4);
  });
});

describe("moveItem", () => {
  it("moves one item and leaves the input alone", () => {
    const items = ["a", "b", "c", "d", "e"];
    expect(moveItem(items, 1, 3)).toEqual(["a", "c", "d", "b", "e"]);
    expect(moveItem(items, 4, 0)).toEqual(["e", "a", "b", "c", "d"]);
    expect(items).toEqual(["a", "b", "c", "d", "e"]);
  });

  it("agrees with moveTarget for every slot", () => {
    const items = Array.from({ length: 6 }, (_, n) => n);
    for (let from = 0; from < items.length; from++) {
      for (let slot = 0; slot <= items.length; slot++) {
        const to = moveTarget(from, slot);
        if (to === null) continue;
        const moved = moveItem(items, from, to);
        // The row sits where the marker was: in front of the row that was at
        // `slot`, or last.
        expect(moved[to]).toBe(from);
        if (slot < items.length) expect(moved[to + 1]).toBe(slot);
        else expect(to).toBe(items.length - 1);
      }
    }
  });
});

describe("isLongList", () => {
  it("virtualizes only above the limit", () => {
    expect(LONG_LIST_ROWS).toBe(300);
    expect(isLongList(0)).toBe(false);
    expect(isLongList(LONG_LIST_ROWS)).toBe(false);
    expect(isLongList(LONG_LIST_ROWS + 1)).toBe(true);
    expect(isLongList(8000)).toBe(true);
  });
});
