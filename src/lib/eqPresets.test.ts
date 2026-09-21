import { beforeEach, describe, expect, it } from "vitest";
import {
  deleteCustomPreset,
  loadCustomPresets,
  MAX_CUSTOM_PRESETS,
  persistCustomPresets,
  sameGains,
  sanitizeGains,
  saveCustomPreset,
  type EqPreset,
} from "$lib/eqPresets";

const flat = Array(10).fill(0);
const warm = [4, 3, 2, 1, 0, 0, -1, -1, 0, 1];

beforeEach(() => localStorage.clear());

describe("sanitizeGains", () => {
  it("clamps to the slider range and snaps to its 0.5 dB steps", () => {
    expect(sanitizeGains([20, -20, 1.26, 0, 0, 0, 0, 0, 0, 0])).toEqual([
      12, -12, 1.5, 0, 0, 0, 0, 0, 0, 0,
    ]);
  });

  it("rejects the wrong band count and non-numbers", () => {
    expect(sanitizeGains([1, 2, 3])).toBeNull();
    expect(sanitizeGains([...flat.slice(1), Number.NaN])).toBeNull();
    expect(sanitizeGains("loud")).toBeNull();
  });
});

describe("saving and deleting", () => {
  it("adds, replaces the same name in any case, and deletes", () => {
    let presets: EqPreset[] = [];
    const first = saveCustomPreset(presets, "  My   Car ", warm);
    expect(first).toMatchObject({ ok: true, replaced: false });
    presets = first.ok ? first.presets : presets;
    expect(presets).toEqual([{ name: "My Car", gains: warm }]);

    const again = saveCustomPreset(presets, "my car", flat);
    expect(again).toMatchObject({ ok: true, replaced: true });
    presets = again.ok ? again.presets : presets;
    expect(presets).toEqual([{ name: "my car", gains: flat }]);

    expect(deleteCustomPreset(presets, "MY CAR")).toEqual([]);
  });

  it("refuses an empty name and a full list", () => {
    expect(saveCustomPreset([], "   ", warm)).toEqual({ ok: false, reason: "name" });
    const full = Array.from({ length: MAX_CUSTOM_PRESETS }, (_, i) => ({ name: `p${i}`, gains: flat }));
    expect(saveCustomPreset(full, "one more", warm)).toEqual({ ok: false, reason: "full" });
    // Replacing still works when full.
    expect(saveCustomPreset(full, "p3", warm)).toMatchObject({ ok: true, replaced: true });
  });
});

describe("persistence", () => {
  it("round-trips through storage", () => {
    persistCustomPresets([{ name: "Night", gains: warm }]);
    expect(loadCustomPresets()).toEqual([{ name: "Night", gains: warm }]);
  });

  it("drops broken entries and duplicates instead of failing", () => {
    localStorage.setItem(
      "jellysic.eq.presets.v1",
      JSON.stringify([
        { name: "Good", gains: warm },
        { name: "good", gains: flat },
        { name: "", gains: warm },
        { name: "Short", gains: [1, 2] },
        null,
        "nonsense",
      ]),
    );
    expect(loadCustomPresets()).toEqual([{ name: "Good", gains: warm }]);
    localStorage.setItem("jellysic.eq.presets.v1", "{not json");
    expect(loadCustomPresets()).toEqual([]);
  });
});

describe("sameGains", () => {
  it("matches to the slider step", () => {
    expect(sameGains(warm, [...warm])).toBe(true);
    expect(sameGains(warm, warm.map((g, i) => (i === 0 ? g + 0.5 : g)))).toBe(false);
  });
});
