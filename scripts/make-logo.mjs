// The Jellysic mark, described once as geometry and emitted in every form the
// project needs: the Windows icon set (.ico), the macOS one (.icns), the PNG
// sizes Tauri bundles, the favicon, and an SVG for the interface.
//
// Run after changing the mark or the accent:  node scripts/make-logo.mjs
// Needs the `pngjs` devDependency.
//
// Every size is rasterized natively from a distance field rather than
// downscaled from one big image -- that is what keeps the 16px icon crisp.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";

const ROOT = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");

// The mark: a bold J whose bowl ends in a tilted note head. Coordinates are a
// 100x100 design grid. Stroke widths are full widths, as in SVG.
const ACCENT = "#1db954"; // --color-accent
const ON_ACCENT = "#000000"; // --color-on-accent

const FIELD = { kind: "roundRect", x: 0, y: 0, w: 100, h: 100, r: 22.5 };
const GLYPH = [
  { kind: "line", a: [74, 20], b: [74, 60], w: 13 },
  {
    kind: "curve",
    pts: [
      [74, 60],
      [74, 82],
      [52, 86],
      [43, 74],
    ],
    w: 13,
  },
  { kind: "ellipse", c: [36, 71], rx: 17, ry: 11, deg: -24 },
];

// --- distance fields ---------------------------------------------------------

const len = (x, y) => Math.hypot(x, y);
const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);

function bezier(pts, t) {
  let cur = pts;
  while (cur.length > 1) {
    const next = [];
    for (let i = 0; i + 1 < cur.length; i++) {
      next.push([
        cur[i][0] + (cur[i + 1][0] - cur[i][0]) * t,
        cur[i][1] + (cur[i + 1][1] - cur[i][1]) * t,
      ]);
    }
    cur = next;
  }
  return cur[0];
}

function sdSegment(p, a, b, r) {
  const pax = p[0] - a[0];
  const pay = p[1] - a[1];
  const bax = b[0] - a[0];
  const bay = b[1] - a[1];
  const h = clamp((pax * bax + pay * bay) / (bax * bax + bay * bay), 0, 1);
  return len(pax - bax * h, pay - bay * h) - r;
}

/** One shape -> a signed distance function. */
function sdf(shape) {
  switch (shape.kind) {
    case "roundRect": {
      const cx = shape.x + shape.w / 2;
      const cy = shape.y + shape.h / 2;
      const hw = shape.w / 2 - shape.r;
      const hh = shape.h / 2 - shape.r;
      return (p) => {
        const qx = Math.abs(p[0] - cx) - hw;
        const qy = Math.abs(p[1] - cy) - hh;
        return len(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - shape.r;
      };
    }
    case "line":
      return (p) => sdSegment(p, shape.a, shape.b, shape.w / 2);
    case "curve": {
      // Sampled into segments; at 96 steps the seams are far below a pixel.
      const steps = 96;
      const pts = [];
      for (let i = 0; i <= steps; i++) pts.push(bezier(shape.pts, i / steps));
      return (p) => {
        let d = Infinity;
        for (let i = 0; i < steps; i++) {
          d = Math.min(d, sdSegment(p, pts[i], pts[i + 1], shape.w / 2));
        }
        return d;
      };
    }
    case "ellipse": {
      const t = (shape.deg * Math.PI) / 180;
      const cs = Math.cos(t);
      const sn = Math.sin(t);
      const k = Math.min(shape.rx, shape.ry);
      return (p) => {
        const dx = p[0] - shape.c[0];
        const dy = p[1] - shape.c[1];
        const x = (dx * cs + dy * sn) / shape.rx;
        const y = (-dx * sn + dy * cs) / shape.ry;
        return (len(x, y) - 1) * k;
      };
    }
    default:
      throw new Error("unknown shape " + shape.kind);
  }
}

function union(shapes) {
  const parts = shapes.map(sdf);
  return (p) => {
    let d = Infinity;
    for (const f of parts) d = Math.min(d, f(p));
    return d;
  };
}

const fieldSdf = sdf(FIELD);
const glyphSdf = union(GLYPH);

// --- raster ------------------------------------------------------------------

function hex(c) {
  return [1, 3, 5].map((i) => parseInt(c.slice(i, i + 2), 16));
}

/**
 * @param {number} size edge length in pixels
 * @param {boolean} withField false renders the bare glyph, cropped to itself
 */
function raster(size, withField = true) {
  const box = withField ? { x: 0, y: 0, w: 100 } : glyphBox();
  const png = new PNG({ width: size, height: size });
  const scale = box.w / size;
  const feather = scale * 0.8; // roughly a pixel of antialiasing
  const layers = withField
    ? [
        { f: fieldSdf, c: hex(ACCENT) },
        { f: glyphSdf, c: hex(ON_ACCENT) },
      ]
    : [{ f: glyphSdf, c: hex(ON_ACCENT) }];
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const p = [box.x + (x + 0.5) * scale, box.y + (y + 0.5) * scale];
      let r = 0;
      let g = 0;
      let b = 0;
      let a = 0;
      for (const layer of layers) {
        const cov = clamp(0.5 - layer.f(p) / feather, 0, 1);
        if (cov <= 0) continue;
        r = layer.c[0] * cov + r * (1 - cov);
        g = layer.c[1] * cov + g * (1 - cov);
        b = layer.c[2] * cov + b * (1 - cov);
        a = cov + a * (1 - cov);
      }
      const i = (y * size + x) << 2;
      png.data[i] = Math.round(r);
      png.data[i + 1] = Math.round(g);
      png.data[i + 2] = Math.round(b);
      png.data[i + 3] = Math.round(a * 255);
    }
  }
  return png;
}

/** The mark's own bounds, as a square box centred on it. */
let cachedBox = null;
function glyphBox() {
  if (cachedBox) return cachedBox;
  let x0 = Infinity;
  let x1 = -Infinity;
  let y0 = Infinity;
  let y1 = -Infinity;
  for (let y = 0; y < 1000; y++) {
    for (let x = 0; x < 1000; x++) {
      if (glyphSdf([x / 10, y / 10]) >= 0) continue;
      if (x / 10 < x0) x0 = x / 10;
      if (x / 10 > x1) x1 = x / 10;
      if (y / 10 < y0) y0 = y / 10;
      if (y / 10 > y1) y1 = y / 10;
    }
  }
  const side = Math.max(x1 - x0, y1 - y0);
  cachedBox = {
    x: (x0 + x1) / 2 - side / 2,
    y: (y0 + y1) / 2 - side / 2,
    w: side,
    bounds: { x0, x1, y0, y1 },
  };
  return cachedBox;
}

const pngBytes = (size, withField = true) => PNG.sync.write(raster(size, withField));

// --- .ico --------------------------------------------------------------------

/** A 32-bit bottom-up DIB with an empty AND mask, which is what an ICO wants. */
function dib(size) {
  const png = raster(size);
  const header = Buffer.alloc(40);
  header.writeUInt32LE(40, 0);
  header.writeInt32LE(size, 4);
  header.writeInt32LE(size * 2, 8); // XOR plus the AND mask
  header.writeUInt16LE(1, 12);
  header.writeUInt16LE(32, 14);
  const xor = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const s = ((size - 1 - y) * size + x) << 2;
      const d = (y * size + x) << 2;
      xor[d] = png.data[s + 2];
      xor[d + 1] = png.data[s + 1];
      xor[d + 2] = png.data[s];
      xor[d + 3] = png.data[s + 3];
    }
  }
  const maskStride = Math.ceil(size / 8 / 4) * 4;
  return Buffer.concat([header, xor, Buffer.alloc(maskStride * size)]);
}

function ico(sizes) {
  const images = sizes.map((s) => (s >= 256 ? pngBytes(s) : dib(s)));
  const dir = Buffer.alloc(6 + 16 * sizes.length);
  dir.writeUInt16LE(0, 0);
  dir.writeUInt16LE(1, 2);
  dir.writeUInt16LE(sizes.length, 4);
  let offset = dir.length;
  sizes.forEach((s, i) => {
    const o = 6 + i * 16;
    dir[o] = s >= 256 ? 0 : s;
    dir[o + 1] = s >= 256 ? 0 : s;
    dir.writeUInt16LE(1, o + 4);
    dir.writeUInt16LE(32, o + 6);
    dir.writeUInt32LE(images[i].length, o + 8);
    dir.writeUInt32LE(offset, o + 12);
    offset += images[i].length;
  });
  return Buffer.concat([dir, ...images]);
}

// --- .icns -------------------------------------------------------------------

function icns(entries) {
  const chunks = entries.map(([type, size]) => {
    const data = pngBytes(size);
    const head = Buffer.alloc(8);
    head.write(type, 0, "ascii");
    head.writeUInt32BE(data.length + 8, 4);
    return Buffer.concat([head, data]);
  });
  const body = Buffer.concat(chunks);
  const head = Buffer.alloc(8);
  head.write("icns", 0, "ascii");
  head.writeUInt32BE(body.length + 8, 4);
  return Buffer.concat([head, body]);
}

// --- svg ---------------------------------------------------------------------

const n = (v) => String(Math.round(v * 1000) / 1000);

function glyphSvg(color, indent) {
  const pad = " ".repeat(indent);
  const strokes = GLYPH.filter((s) => s.kind !== "ellipse");
  const heads = GLYPH.filter((s) => s.kind === "ellipse");
  const paths = strokes.map((s) =>
    s.kind === "line"
      ? pad + "  <path d=\"M" + n(s.a[0]) + " " + n(s.a[1]) + "L" + n(s.b[0]) + " " + n(s.b[1]) + "\" />"
      : pad
        + "  <path d=\"M"
        + s.pts[0].map(n).join(" ")
        + "C"
        + s.pts.slice(1).map((p) => p.map(n).join(" ")).join(" ")
        + "\" />",
  );
  const ellipses = heads.map(
    (s) =>
      pad
      + "<ellipse cx=\"" + n(s.c[0]) + "\" cy=\"" + n(s.c[1]) + "\""
      + " rx=\"" + n(s.rx) + "\" ry=\"" + n(s.ry) + "\""
      + " transform=\"rotate(" + n(s.deg) + " " + n(s.c[0]) + " " + n(s.c[1]) + ")\""
      + " fill=\"" + color + "\" />",
  );
  return [
    pad + "<g fill=\"none\" stroke=\"" + color + "\" stroke-width=\"" + n(strokes[0].w)
      + "\" stroke-linecap=\"round\">",
    ...paths,
    pad + "</g>",
    ...ellipses,
  ].join("\n");
}

function iconSvg() {
  return [
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\" role=\"img\" aria-label=\"Jellysic\">",
    "  <rect width=\"100\" height=\"100\" rx=\"" + n(FIELD.r) + "\" fill=\"" + ACCENT + "\" />",
    glyphSvg(ON_ACCENT, 2),
    "</svg>",
    "",
  ].join("\n");
}

/** The bare mark, cropped to itself, in whatever colour it is placed on. */
function markSvelte() {
  const b = glyphBox();
  const view = [n(b.x), n(b.y), n(b.w), n(b.w)].join(" ");
  return [
    "<!-- Generated by scripts/make-logo.mjs -- change the mark there, not here. -->",
    "<script lang=\"ts\">",
    "  let { class: className = \"\" }: { class?: string } = $props();",
    "</" + "script>",
    "",
    "<svg viewBox=\"" + view + "\" class={className} aria-hidden=\"true\">",
    glyphSvg("currentColor", 2),
    "</svg>",
    "",
  ].join("\n");
}

// --- write -------------------------------------------------------------------

function write(rel, data) {
  const p = path.join(ROOT, rel);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, data);
  console.log("  " + rel);
}

const bounds = glyphBox().bounds;
console.log(
  "mark bounds  x " + bounds.x0.toFixed(1) + ".." + bounds.x1.toFixed(1)
  + "  y " + bounds.y0.toFixed(1) + ".." + bounds.y1.toFixed(1)
  + "   margins L " + bounds.x0.toFixed(1)
  + " R " + (100 - bounds.x1).toFixed(1)
  + " T " + bounds.y0.toFixed(1)
  + " B " + (100 - bounds.y1).toFixed(1),
);
console.log("writing:");

const PNGS = [
  ["src-tauri/icons/icon.png", 512],
  ["src-tauri/icons/32x32.png", 32],
  ["src-tauri/icons/128x128.png", 128],
  ["src-tauri/icons/128x128@2x.png", 256],
  ["src-tauri/icons/StoreLogo.png", 50],
  ["src-tauri/icons/Square30x30Logo.png", 30],
  ["src-tauri/icons/Square44x44Logo.png", 44],
  ["src-tauri/icons/Square71x71Logo.png", 71],
  ["src-tauri/icons/Square89x89Logo.png", 89],
  ["src-tauri/icons/Square107x107Logo.png", 107],
  ["src-tauri/icons/Square142x142Logo.png", 142],
  ["src-tauri/icons/Square150x150Logo.png", 150],
  ["src-tauri/icons/Square284x284Logo.png", 284],
  ["src-tauri/icons/Square310x310Logo.png", 310],
  ["static/favicon.png", 128],
  ["docs/logo.png", 256],
];
for (const [rel, size] of PNGS) write(rel, pngBytes(size));

write("src-tauri/icons/icon.ico", ico([16, 24, 32, 48, 64, 128, 256]));
write(
  "src-tauri/icons/icon.icns",
  icns([
    ["ic11", 32],
    ["ic12", 64],
    ["ic07", 128],
    ["ic13", 256],
    ["ic09", 512],
    ["ic10", 1024],
  ]),
);
write("docs/logo.svg", iconSvg());
write("src/lib/components/BrandMark.svelte", markSvelte());

console.log("");
console.log("The installer art is generated from icon.png -- run:");
console.log("  node scripts/make-installer-art.mjs");
