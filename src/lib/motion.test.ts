import { afterEach, describe, expect, it, vi } from "vitest";
import { FLIP_ROW_LIMIT, listFlip, uniqueRowKeys } from "$lib/motion";

const rects = {
  from: { left: 0, top: 0, width: 100, height: 20 } as DOMRect,
  to: { left: 0, top: 40, width: 100, height: 20 } as DOMRect,
};

function stubReducedMotion(reduce: boolean) {
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: reduce && query.includes("reduce") }));
}

afterEach(() => vi.unstubAllGlobals());

describe("uniqueRowKeys", () => {
  it("keeps unique ids and replaces empty or repeated ones", () => {
    expect(uniqueRowKeys(["a", "b", "c"])).toEqual(["a", "b", "c"]);
    expect(uniqueRowKeys(["a", "", "a", ""])).toEqual(["a", "row:1", "row:2", "row:3"]);
  });

  it("always yields distinct keys", () => {
    const keys = uniqueRowKeys(["x", "x", "", "y", "", "x"]);
    expect(new Set(keys).size).toBe(keys.length);
  });
});

describe("listFlip", () => {
  it("slides a short list", () => {
    stubReducedMotion(false);
    const node = document.createElement("li");
    const config = listFlip(node, rects, { rows: 12 });
    expect(config.duration).toBe(200);
    expect(config.css).toBeTypeOf("function");
  });

  it("stays still for long lists and for reduced motion", () => {
    stubReducedMotion(false);
    const node = document.createElement("li");
    expect(listFlip(node, rects, { rows: FLIP_ROW_LIMIT + 1 }).duration).toBe(0);
    stubReducedMotion(true);
    expect(listFlip(node, rects, { rows: 12 }).duration).toBe(0);
  });
});
