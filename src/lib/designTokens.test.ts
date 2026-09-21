import { describe, expect, it } from "vitest";

// Every source file, as text (Vite glob, so no fs access and no extra deps).
const sources = import.meta.glob("/src/**/*.{svelte,ts,css,html}", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

describe("design tokens", () => {
  it("never uses Tailwind's text-base (it is the background color here)", () => {
    // With the `--color-base` token, `text-base` compiles to
    // `color: var(--color-base)` and sets no font size — text in the
    // background color. Size: `text-md`; color: `text-(--color-base)`.
    const pattern = new RegExp(String.raw`(^|[\s"'{:])text-base(?![-\w])`, "m");
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.endsWith("designTokens.test.ts"))
      .filter(([, text]) => pattern.test(text))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });

  it("never highlights with white-alpha utilities (use the ink token)", () => {
    // The app is AMOLED dark and the ink token carries the highlight color.
    // `bg-white/10` and friends hardcode the same thing one layer lower, which
    // is how the four "always dark" views drifted out of the token system.
    // Opaque `text-white` on a colored surface (a red button, a generated hue
    // gradient) stays fine — that is a foreground, not a highlight.
    const pattern = new RegExp(
      String.raw`(^|[\s"'{:])(bg|text|border|ring|divide|outline|from|to|via|accent|fill|stroke)-white/`,
      "m",
    );
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.endsWith("designTokens.test.ts"))
      .filter(([, text]) => pattern.test(text))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });

  it("scans the real source tree", () => {
    expect(Object.keys(sources).length).toBeGreaterThan(50);
    expect(Object.keys(sources)).toContain("/src/app.css");
  });
});
