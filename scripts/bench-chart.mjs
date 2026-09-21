// Renders the comparison chart for the README from the measured figures.
//
//   node scripts/bench-chart.mjs
//
// Writes docs/bench-dark.svg and docs/bench-light.svg. SVG because the text
// stays crisp and the file stays a few KB; two files because GitHub strips
// <style> out of embedded SVG, so the README switches them with <picture>.
//
// Form: seven measures on different scales, so each row is normalised within
// itself. Never one axis for two units.
//
// Colour: slots 1-4 of the reference categorical palette. They clear the
// lightness band, chroma floor, CVD separation and normal-vision floor in both
// modes. Not the brand green: #1db954 sits above the dark lightness band, and
// green against orange separates by only 3.9 for deuteranopia, the classic
// red-green confusion. In light mode the aqua and yellow fall under 3:1
// against the surface, a WARN that the per-bar value labels and the table in
// the README discharge.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const APPS = [
  { key: "jellysic", name: "Jellysic 0.1.0", runtime: "Rust + WebView2" },
  { key: "supersonic", name: "Supersonic 0.22.1", runtime: "Go + Fyne" },
  { key: "feishin", name: "Feishin 1.17.0", runtime: "Electron" },
  { key: "sonixd", name: "Sonixd 0.15.5", runtime: "Electron, archived" },
];

/** Median of two runs each. See scripts/bench.mjs for the method. */
const SECTIONS = [
  {
    title: "Footprint",
    rows: [
      { label: "Installer", unit: "MB", jellysic: 7.2, supersonic: 49.8, feishin: 178, sonixd: 81.7 },
      { label: "On disk", unit: "MB", jellysic: 28, supersonic: 169, feishin: 644, sonixd: 332 },
    ],
  },
  {
    title: "Startup",
    rows: [
      { label: "Launch → window", unit: "ms", jellysic: 177, supersonic: 594, feishin: 722, sonixd: 719 },
    ],
  },
  {
    title: "At rest, signed in and idle",
    rows: [
      { label: "Memory, working set", unit: "MB", jellysic: 427, supersonic: 198, feishin: 760, sonixd: 406 },
      { label: "Memory, private bytes", unit: "MB", jellysic: 273, supersonic: 404, feishin: 464, sonixd: 213 },
      { label: "CPU", unit: "%", jellysic: 0.65, supersonic: 0.2, feishin: 2.1, sonixd: 4.95 },
      { label: "Processes", unit: "", jellysic: 7, supersonic: 1, feishin: 6, sonixd: 5 },
    ],
  },
];

const THEME = {
  dark: {
    surface: "#0a0a0c",
    ink: "#f2f2f4",
    muted: "#8a8a93",
    faint: "#ffffff12",
    series: { jellysic: "#3987e5", supersonic: "#199e70", feishin: "#d95926", sonixd: "#c98500" },
  },
  light: {
    surface: "#fcfcfb",
    ink: "#14141a",
    muted: "#5c5b63",
    faint: "#14141a12",
    series: { jellysic: "#2a78d6", supersonic: "#1baf7a", feishin: "#eb6834", sonixd: "#eda100" },
  },
};

const W = 1000;
const PAD = 32;
const LABEL_W = 186;
const TAIL_W = 84;
const BAR_H = 10;
const BAR_GAP = 5;
const ROW_H = 80;
const SECTION_H = 34;
const HEAD_H = 104;
const FOOT_H = 52;
const TRACK_X = PAD + LABEL_W;
const TRACK_W = W - PAD * 2 - LABEL_W - TAIL_W;
const ROW_COUNT = SECTIONS.reduce((n, s) => n + s.rows.length, 0);
const H = HEAD_H + SECTIONS.length * SECTION_H + ROW_COUNT * ROW_H + FOOT_H;

const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const n2 = (n) => Number(n.toFixed(2));

/** Rounded only on the data end, anchored to the baseline. */
function bar(x, y, w, h, fill) {
  const r = Math.min(4, w / 2, h / 2);
  if (w <= r * 2) {
    return `<rect x="${n2(x)}" y="${y}" width="${n2(Math.max(w, 2))}" height="${h}" rx="${n2(Math.min(2, w / 2))}" fill="${fill}"/>`;
  }
  return (
    `<path d="M${n2(x)} ${y} H${n2(x + w - r)} a${r} ${r} 0 0 1 ${r} ${r} ` +
    `V${n2(y + h - r)} a${r} ${r} 0 0 1 ${-r} ${r} H${n2(x)} Z" fill="${fill}"/>`
  );
}

const fmt = (v, unit) => (unit ? `${v} ${unit}` : `${v}`);

function render(mode) {
  const t = THEME[mode];
  const font = `-apple-system, "Segoe UI Variable Text", "Segoe UI", Inter, Roboto, Helvetica, Arial, sans-serif`;
  const o = [];

  const summary = SECTIONS.flatMap((s) => s.rows)
    .map((r) => `${r.label}: ${APPS.map((a) => `${a.name.split(" ")[0]} ${r[a.key]}`).join(", ")} ${r.unit}`)
    .join(". ");

  o.push(
    `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" ` +
      `role="img" aria-label="Four Jellyfin music clients for Windows, measured on the same library. ` +
      `Shorter is better. ${esc(summary)}.">`,
  );
  o.push(`<rect width="${W}" height="${H}" fill="${t.surface}"/>`);
  o.push(`<g font-family='${font}'>`);

  o.push(
    `<text x="${PAD}" y="38" fill="${t.ink}" font-size="21" font-weight="700" letter-spacing="-0.3">` +
      `Four clients, one library</text>`,
  );
  o.push(
    `<text x="${PAD}" y="62" fill="${t.muted}" font-size="12.5">` +
      `Shorter is better · each row scaled to its own worst value · ` +
      `median of two runs, 45 s each after a 15 s settle</text>`,
  );

  // Legend: two columns of two, each with the runtime underneath.
  const legendX = W - PAD - 330;
  APPS.forEach((app, i) => {
    const x = legendX + (i % 2) * 170;
    const y = 26 + Math.floor(i / 2) * 26;
    o.push(`<rect x="${x}" y="${y}" width="9" height="9" rx="2" fill="${t.series[app.key]}"/>`);
    o.push(`<text x="${x + 15}" y="${y + 8}" fill="${t.ink}" font-size="11.5">${esc(app.name)}</text>`);
    o.push(
      `<text x="${x + 15}" y="${y + 19}" fill="${t.muted}" font-size="10">${esc(app.runtime)}</text>`,
    );
  });

  let y = HEAD_H;
  for (const section of SECTIONS) {
    o.push(
      `<text x="${PAD}" y="${y + 14}" fill="${t.muted}" font-size="10.5" font-weight="700" ` +
        `letter-spacing="1.1">${esc(section.title.toUpperCase())}</text>`,
    );
    o.push(
      `<line x1="${PAD}" y1="${y + 22}" x2="${W - PAD}" y2="${y + 22}" stroke="${t.faint}" stroke-width="1"/>`,
    );
    y += SECTION_H;

    for (const row of section.rows) {
      const worst = Math.max(...APPS.map((a) => row[a.key]));
      const best = Math.min(...APPS.map((a) => row[a.key]));

      o.push(
        `<text x="${PAD}" y="${y + 15}" fill="${t.ink}" font-size="13.5" font-weight="600">` +
          `${esc(row.label)}</text>`,
      );
      o.push(
        `<text x="${PAD}" y="${y + 33}" fill="${t.muted}" font-size="11">` +
          `best ${esc(fmt(best, row.unit))}</text>`,
      );

      APPS.forEach((app, j) => {
        const v = row[app.key];
        const w = Math.max(3, (v / worst) * TRACK_W);
        const by = y + 2 + j * (BAR_H + BAR_GAP);
        o.push(bar(TRACK_X, by, w, BAR_H, t.series[app.key]));
        o.push(
          `<text x="${n2(TRACK_X + w + 10)}" y="${by + BAR_H - 1}" ` +
            `fill="${v === best ? t.ink : t.muted}" font-size="11.5" ` +
            `font-weight="${v === best ? 600 : 400}">${esc(fmt(v, row.unit))}</text>`,
        );
      });
      y += ROW_H;
    }
  }

  o.push(
    `<text x="${PAD}" y="${H - 20}" fill="${t.muted}" font-size="11">` +
      `Same machine, same demo library, each client signed in and idle on its library view. ` +
      `Memory is summed across the whole process tree. Method: scripts/bench.mjs</text>`,
  );
  o.push(`</g></svg>`);
  return o.join("\n");
}

mkdirSync(join(root, "docs"), { recursive: true });
for (const mode of ["dark", "light"]) {
  writeFileSync(join(root, "docs", `bench-${mode}.svg`), render(mode) + "\n");
  console.log(`wrote docs/bench-${mode}.svg`);
}
