import { afterEach, describe, expect, it, vi } from "vitest";
import { pickAccent } from "$lib/ambient";
import {
  ACCENT_CHANGED,
  applyTheme,
  contrastRatio,
  coverAccent,
  DEFAULT_THEME,
  hexLuminance,
  setCoverHue,
} from "$lib/theme";

// --color-base in app.css. Keep in sync: the contrast assertions below are
// only meaningful against the background the app actually paints.
const BASE = hexLuminance("#000000")!;

afterEach(() => {
  localStorage.clear();
  setCoverHue(null);
});

describe("coverAccent", () => {
  const hues = Array.from({ length: 24 }, (_, i) => i * 15);

  it("stays readable on the black background for every hue", () => {
    for (const h of hues) {
      for (const s of [0.3, 0.6, 1]) {
        const { accent, hover, onAccent } = coverAccent({ h, s }, BASE);
        const lum = hexLuminance(accent)!;
        // Accent-colored icons and text against the background.
        expect(contrastRatio(lum, BASE), `h=${h} s=${s} ${accent}`).toBeGreaterThanOrEqual(3);
        // Text on an accent surface: the better of black and white.
        expect(contrastRatio(lum, hexLuminance(onAccent)!)).toBeGreaterThanOrEqual(4.5);
        expect(hover).toMatch(/^#[0-9a-f]{6}$/);
      }
    }
  });
});

describe("hexLuminance", () => {
  it("reads the shorthand the CSS minifier produces", () => {
    // app.css writes #000000; the built stylesheet says #000, and that is what
    // getComputedStyle hands back to baseLuminance().
    expect(hexLuminance("#000")).toBe(hexLuminance("#000000"));
    expect(hexLuminance("#abc")).toBe(hexLuminance("#aabbcc"));
    expect(hexLuminance("not a color")).toBeNull();
  });
});

describe("pickAccent", () => {
  it("prefers a clearly colored area over a bigger grey one", () => {
    const grey = { n: 300, h: 0, s: 0.05, l: 0.5 };
    const blue = { n: 40, h: 220, s: 0.7, l: 0.45 };
    expect(pickAccent([grey, blue])).toEqual({ h: 220, s: 0.7 });
  });

  it("ignores a handful of stray pixels and near-black or near-white areas", () => {
    expect(pickAccent([{ n: 2, h: 10, s: 1, l: 0.5 }])).toBeNull();
    expect(pickAccent([{ n: 200, h: 10, s: 0.9, l: 0.05 }])).toBeNull();
    expect(pickAccent([{ n: 200, h: 10, s: 0.9, l: 0.95 }])).toBeNull();
  });

  it("weighs population by saturation", () => {
    const big = { n: 120, h: 30, s: 0.3, l: 0.5 }; // 36
    const vivid = { n: 50, h: 300, s: 0.9, l: 0.5 }; // 45
    expect(pickAccent([big, vivid])?.h).toBe(300);
  });
});

describe("setCoverHue", () => {
  const accentVar = () => document.documentElement.style.getPropertyValue("--color-accent");

  it("recolors the accent in cover mode and announces it", () => {
    applyTheme({ ...DEFAULT_THEME, accentMode: "cover" });
    const listener = vi.fn();
    window.addEventListener(ACCENT_CHANGED, listener);
    setCoverHue({ h: 220, s: 0.7 });
    window.removeEventListener(ACCENT_CHANGED, listener);
    expect(accentVar()).not.toBe(DEFAULT_THEME.accent);
    expect(listener).toHaveBeenCalled();
    // Remembered for the pre-paint script.
    expect(JSON.parse(localStorage.getItem("jellysic.coverAccent.v2")!).accent).toBe(accentVar());
  });

  it("falls back to the fixed accent for a cover without a clear color", () => {
    applyTheme({ ...DEFAULT_THEME, accentMode: "cover" });
    setCoverHue({ h: 220, s: 0.7 });
    setCoverHue(null);
    expect(accentVar()).toBe(DEFAULT_THEME.accent);
  });

  it("leaves a fixed accent alone", () => {
    applyTheme({ ...DEFAULT_THEME, accentMode: "fixed" });
    setCoverHue({ h: 220, s: 0.7 });
    expect(accentVar()).toBe(DEFAULT_THEME.accent);
  });

  it("keeps the stored cover accent on start until a cover is analysed", async () => {
    const stored = { accent: "#123456", hover: "#234567", onAccent: "#ffffff" };
    localStorage.setItem("jellysic.theme", JSON.stringify({ ...DEFAULT_THEME, accentMode: "cover" }));
    localStorage.setItem("jellysic.coverAccent.v2", JSON.stringify(stored));
    // A fresh module: its import-time initTheme() runs before any cover is known.
    vi.resetModules();
    const fresh = await import("$lib/theme");
    expect(accentVar()).toBe("#123456");
    expect(document.documentElement.style.getPropertyValue("--color-on-accent")).toBe("#ffffff");

    // A fixed accent ignores the stored cover accent.
    fresh.applyTheme({ ...DEFAULT_THEME, accentMode: "fixed" });
    expect(accentVar()).toBe(DEFAULT_THEME.accent);

    // Once the cover turns out to have no clear color, the fixed accent wins.
    fresh.applyTheme({ ...DEFAULT_THEME, accentMode: "cover" });
    fresh.setCoverHue(null);
    expect(accentVar()).toBe(DEFAULT_THEME.accent);
  });

  it("ignores a `mode` left over from the light/dark era", () => {
    localStorage.setItem(
      "jellysic.theme",
      JSON.stringify({ ...DEFAULT_THEME, accentMode: "fixed", mode: "light" }),
    );
    applyTheme({ ...DEFAULT_THEME, accentMode: "fixed" });
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
    expect(accentVar()).toBe(DEFAULT_THEME.accent);
  });

  it("picks readable text for a fixed accent, dark or light", () => {
    const onAccent = () =>
      document.documentElement.style.getPropertyValue("--color-on-accent");

    // A dark custom accent used to get black icons on it -- an invisible play
    // button. Only the cover path ever computed this.
    applyTheme({ accent: "#1a1a4d", accentHover: "#25256b", accentMode: "fixed" });
    expect(onAccent()).toBe("#ffffff");

    applyTheme({ ...DEFAULT_THEME, accentMode: "fixed" });
    expect(onAccent()).toBe("#000000");

    applyTheme({ accent: "#ffffff", accentHover: "#e6e6e6", accentMode: "fixed" });
    expect(onAccent()).toBe("#000000");
  });

  it("never paints the <html> background (the window is transparent)", () => {
    applyTheme({ ...DEFAULT_THEME });
    expect(document.documentElement.style.backgroundColor).toBe("");
  });
});
