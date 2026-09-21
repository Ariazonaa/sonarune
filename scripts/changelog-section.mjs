// Print one version's section of CHANGELOG.md as plain text.
//
//   node scripts/changelog-section.mjs 0.2.0 [--max 4000]
//
// The release workflow puts this into `latest.json` as the update's `notes`,
// and that is what the app shows in Settings -> Updates when it finds an
// update. Before this, the field said "see the changelog", which is a poor
// thing to tell someone who is looking at a dialog asking whether to install.
//
// The output is plain text, not markdown: the panel renders it as-is. So the
// headings lose their `###`, `**bold**` and `code` lose their markers, and
// the hard line wrapping of the file is undone — the panel wraps by itself,
// and keeping the file's line breaks would leave ragged short lines.
//
// Prints nothing and exits 0 when the version has no section; the caller
// decides what to do with that (the workflow falls back to a fixed sentence).
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/** The lines of `## [version]` up to the next `## ` heading. */
export function sectionLines(changelog, version) {
  const lines = changelog.split(/\r?\n/);
  const start = lines.findIndex((line) => line.startsWith(`## [${version}]`));
  if (start < 0) return [];
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => line.startsWith("## "));
  return end < 0 ? rest : rest.slice(0, end);
}

/** Markdown markers the plain-text panel would otherwise show literally. */
function plain(text) {
  return text
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
}

/**
 * Join the file's hard-wrapped lines back into one line per bullet and per
 * paragraph. A line that starts a bullet (`- `) or a heading begins a new
 * block; anything else continues the one before it.
 */
export function sectionText(changelog, version, max = 4000) {
  const blocks = [];
  for (const raw of sectionLines(changelog, version)) {
    const line = raw.trimEnd();
    if (!line.trim()) {
      blocks.push(null); // blank line: keep the paragraph break
      continue;
    }
    if (line.startsWith("#")) {
      blocks.push(plain(line.replace(/^#+\s*/, "")));
      blocks.push(null);
      continue;
    }
    if (/^[-*]\s/.test(line)) {
      blocks.push(plain(line.replace(/^[-*]\s+/, "- ")));
      continue;
    }
    const last = blocks.length > 0 ? blocks[blocks.length - 1] : null;
    if (last === null || last === undefined) blocks.push(plain(line.trim()));
    else blocks[blocks.length - 1] = `${last} ${plain(line.trim())}`;
  }

  // Collapse runs of blank lines, and drop them at both ends.
  const out = [];
  for (const block of blocks) {
    if (block === null) {
      if (out.length > 0 && out[out.length - 1] !== "") out.push("");
    } else {
      out.push(block);
    }
  }
  while (out.length > 0 && out[out.length - 1] === "") out.pop();

  const text = out.join("\n");
  if (text.length <= max) return text;
  // Cut between blocks, never inside a sentence: a bullet that stops halfway
  // reads like something went wrong, not like a text that was too long.
  const kept = [];
  let used = 0;
  for (const block of out) {
    if (used + block.length + 1 > max - 2) break;
    kept.push(block);
    used += block.length + 1;
  }
  return `${kept.join("\n").trimEnd()}\n…`;
}

// Run as a script (not when imported by a test).
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const version = process.argv[2];
  if (!version) {
    console.error("usage: node scripts/changelog-section.mjs <version> [--max N]");
    process.exit(2);
  }
  const maxIdx = process.argv.indexOf("--max");
  const max = maxIdx >= 0 ? Number(process.argv[maxIdx + 1]) : 4000;
  const changelog = readFileSync(join(root, "CHANGELOG.md"), "utf8");
  const text = sectionText(changelog, version, max);
  if (text) process.stdout.write(`${text}\n`);
}
