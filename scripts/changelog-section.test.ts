// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { sectionText } from "./changelog-section.mjs";

/** A changelog with one section for 9.9.9, followed by an older one. */
function changelog(body: string): string {
  return `# Changelog\n\nIntro.\n\n## [9.9.9] — 2026-01-01\n\n${body}\n\n## [9.9.8]\n\n- older\n`;
}

const notes = (body: string) => sectionText(changelog(body), "9.9.9");

/**
 * The changelog lint. It reads the output with every code span blanked --
 * what a span holds is literal, `**` included -- and its patterns are broader
 * than plain()'s own rules on purpose, so it also catches markdown plain()
 * does not handle.
 */
const MARKERS = [
  /\*\*/, // bold
  /__/, // bold, the underscore way
  /~~/, // strikethrough
  /`/, // a backtick no code span accounts for
  /\]\(/, // a link
  /\]\[/, // a reference link
  // Emphasis: a marker that opens a word and the same one closing a word
  // later on the line, whatever lies between.
  /(^|[^\w*_])([*_])(?=\S).*?\S\2(?!\w)/,
];
const prose = (file: string, version: string) =>
  sectionText(file, version, Number.MAX_SAFE_INTEGER, { code: () => "" });
const leftovers = (text: string) =>
  text.split("\n").filter((line) => MARKERS.some((marker) => marker.test(line)));

describe("sectionText", () => {
  it("drops single emphasis, as asterisks and as underscores", () => {
    expect(notes("- The new *History* page and the _Year in review_.")).toBe(
      "- The new History page and the Year in review.",
    );
    expect(notes("- *Sonic radio*: from any track.")).toBe("- Sonic radio: from any track.");
  });

  it("drops bold, code and links as before", () => {
    expect(notes("- **Bold.** Run `npm run verify`, see [the docs](docs/testing.md).")).toBe(
      "- Bold. Run npm run verify, see the docs.",
    );
  });

  it("leaves asterisks and underscores that are not emphasis", () => {
    expect(notes("- 2 * 3 * 4, every *.md file, and snake_case_names.")).toBe(
      "- 2 * 3 * 4, every *.md file, and snake_case_names.",
    );
  });

  it("keeps a code span literal", () => {
    expect(notes("- The keys `sonarune.*` and `sonarune:*` stay.")).toBe(
      "- The keys sonarune.* and sonarune:* stay.",
    );
  });

  it("finds a marker pair the hard wrap split across two lines", () => {
    expect(notes("- A **sonic\n  radio** and the *sonic\n  path*.")).toBe(
      "- A sonic radio and the sonic path.",
    );
  });

  it("joins wrapped lines and keeps headings and paragraphs apart", () => {
    expect(notes("### Added\n\n- One\n  line.\n- Two.\n\nA paragraph\nthat wraps.")).toBe(
      "Added\n\n- One line.\n- Two.\n\nA paragraph that wraps.",
    );
  });

  it("keeps a glob in a code span, and the lint leaves it alone", () => {
    const body = "- Vitest now also runs `scripts/**/*.test.ts` and keeps `*emphasis*`.";
    expect(notes(body)).toBe("- Vitest now also runs scripts/**/*.test.ts and keeps *emphasis*.");
    expect(leftovers(prose(changelog(body), "9.9.9"))).toEqual([]);
  });

  it("lint: flags what plain() leaves behind, not ordinary asterisks", () => {
    const flagged = [
      "- __Bold__ the other way.",
      "- ~~Gone~~ again.",
      "- A stray ` backtick.",
      "- *Inner*emphasis* and *x*y.",
      "- See [the [nested] docs](docs/testing.md).",
      "- See [the docs][testing].",
    ];
    for (const body of flagged) {
      expect(leftovers(prose(changelog(body), "9.9.9")), body).not.toEqual([]);
    }
    const clean = [
      "- 2 * 3 * 4, every *.md file, and snake_case_names.",
      "- The *History* page and the _Year in review_, `sonarune.*` in a span.",
      "- **Bold**, [a link](docs/testing.md) and `code`.",
      "- A [note] (in brackets) is no link.",
    ];
    for (const body of clean) {
      expect(leftovers(prose(changelog(body), "9.9.9")), body).toEqual([]);
    }
  });

  // What the release puts into latest.json and the winget manifest: every
  // section of the real changelog has to come out without markdown markers.
  it("leaves no markdown markers in any section of CHANGELOG.md", () => {
    const file = readFileSync(new URL("../CHANGELOG.md", import.meta.url), "utf8");
    const versions = [...file.matchAll(/^## \[([^\]]+)\]/gm)].map((match) => match[1]);
    expect(versions.length).toBeGreaterThan(0);
    const found = versions.flatMap((version) =>
      leftovers(prose(file, version)).map((line) => `[${version}] ${line}`),
    );
    expect(found).toEqual([]);
  });
});
