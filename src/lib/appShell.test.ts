import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

// Read from disk (the tests run from the project root): Vitest hands a CSS
// import, `?raw` included, over empty.
const css = readFileSync(resolve("src/app.css"), "utf8");

describe("app shell", () => {
  it("is no scroll container", () => {
    // A box with `overflow: hidden` can still be scrolled by script — a
    // scrollIntoView, a focus — and `.app-root::before` overflows it by 20 %.
    // Revealing the playing queue row once moved the whole shell, title bar
    // and window buttons, out of the window. A `clip` box cannot scroll.
    const rule = css.match(/\.app-root\s*\{[^}]*\}/)?.[0] ?? "";
    expect(rule).toMatch(/overflow:\s*clip;/);
  });
});
