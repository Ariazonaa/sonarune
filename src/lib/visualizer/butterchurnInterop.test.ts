import { describe, expect, it } from "vitest";
import { unwrapButterchurn } from "./butterchurnInterop";

describe("unwrapButterchurn", () => {
  const Butterchurn = class {
    static createVisualizer() {
      return {};
    }
  };

  it("takes the class however deep the bundler put it", () => {
    // esbuild (dev): the class is the module's default export.
    expect(unwrapButterchurn({ default: Butterchurn })).toBe(Butterchurn);
    // Rolldown (build): module.exports wrapped once more.
    expect(unwrapButterchurn({ default: { __esModule: true, default: Butterchurn } })).toBe(
      Butterchurn,
    );
    // Already unwrapped.
    expect(unwrapButterchurn(Butterchurn)).toBe(Butterchurn);
  });

  it("says so when there is nothing to find", () => {
    expect(() => unwrapButterchurn({ default: {} })).toThrow(/createVisualizer/);
  });
});
