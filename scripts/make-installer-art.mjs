// Generates the NSIS installer artwork (header + welcome/finish sidebar) from
// the app logo, so the Windows setup looks branded instead of the default
// blank wizard. Outputs 24-bit BMP3 (what NSIS requires) into src-tauri/installer/.
//
// Run once (or whenever the logo/brand changes):  node scripts/make-installer-art.mjs
import { PNG } from "pngjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");
const LOGO = path.join(ROOT, "src-tauri", "icons", "icon.png");
const OUT_DIR = path.join(ROOT, "src-tauri", "installer");

// Brand palette (installer art is a fixed artifact — always the default green).
const ACCENT = [29, 185, 84]; // #1db954
const TOP = [10, 12, 15]; // near-black, faint blue
const BOTTOM = [12, 34, 22]; // very dark green

const clamp = (v) => Math.max(0, Math.min(255, Math.round(v)));
const lerp = (a, b, t) => a + (b - a) * t;

/** Box-average downscale of an RGBA image to w×h, returns RGBA Uint8Array. */
function downscale(src, sw, sh, w, h) {
  const out = new Uint8Array(w * h * 4);
  const fx = sw / w;
  const fy = sh / h;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      let r = 0, g = 0, b = 0, a = 0, n = 0;
      const x0 = Math.floor(x * fx), x1 = Math.max(x0 + 1, Math.floor((x + 1) * fx));
      const y0 = Math.floor(y * fy), y1 = Math.max(y0 + 1, Math.floor((y + 1) * fy));
      for (let sy = y0; sy < y1; sy++) {
        for (let sx = x0; sx < x1; sx++) {
          const i = (sy * sw + sx) * 4;
          const al = src[i + 3] / 255;
          r += src[i] * al; g += src[i + 1] * al; b += src[i + 2] * al; a += src[i + 3];
          n++;
        }
      }
      const o = (y * w + x) * 4;
      const alpha = a / n;
      const wa = a > 0 ? a / 255 : 1;
      out[o] = clamp(r / (wa || 1)); out[o + 1] = clamp(g / (wa || 1));
      out[o + 2] = clamp(b / (wa || 1)); out[o + 3] = clamp(alpha);
    }
  }
  return out;
}

/** An RGB canvas with gradient + additive glow + composite + bar helpers. */
function makeCanvas(w, h) {
  const px = new Float32Array(w * h * 3);
  const set = (x, y, r, g, b) => {
    if (x < 0 || y < 0 || x >= w || y >= h) return;
    const o = (y * w + x) * 3;
    px[o] = r; px[o + 1] = g; px[o + 2] = b;
  };
  const add = (x, y, r, g, b) => {
    if (x < 0 || y < 0 || x >= w || y >= h) return;
    const o = (y * w + x) * 3;
    px[o] = Math.min(255, px[o] + r);
    px[o + 1] = Math.min(255, px[o + 1] + g);
    px[o + 2] = Math.min(255, px[o + 2] + b);
  };
  return {
    w, h, px, set, add,
    gradient(diag = true) {
      for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) {
          const t = diag ? (x / w + y / h) / 2 : y / h;
          set(x, y, lerp(TOP[0], BOTTOM[0], t), lerp(TOP[1], BOTTOM[1], t), lerp(TOP[2], BOTTOM[2], t));
        }
      }
    },
    glow(cx, cy, radius, strength) {
      for (let y = 0; y < h; y++) {
        for (let x = 0; x < w; x++) {
          const d = Math.hypot(x - cx, y - cy) / radius;
          if (d >= 1) continue;
          const f = (1 - d) * (1 - d) * strength;
          add(x, y, ACCENT[0] * f, ACCENT[1] * f, ACCENT[2] * f);
        }
      }
    },
    composite(img, iw, ih, dx, dy) {
      for (let y = 0; y < ih; y++) {
        for (let x = 0; x < iw; x++) {
          const i = (y * iw + x) * 4;
          const a = img[i + 3] / 255;
          if (a <= 0) continue;
          const ox = dx + x, oy = dy + y;
          if (ox < 0 || oy < 0 || ox >= w || oy >= h) continue;
          const o = (oy * w + ox) * 3;
          px[o] = lerp(px[o], img[i], a);
          px[o + 1] = lerp(px[o + 1], img[i + 1], a);
          px[o + 2] = lerp(px[o + 2], img[i + 2], a);
        }
      }
    },
    // A row of equalizer bars anchored to `baseY`, the music-app signature.
    eqBars(x0, baseY, count, barW, gap, maxH, alpha) {
      for (let n = 0; n < count; n++) {
        const bx = x0 + n * (barW + gap);
        const hgt = Math.round((0.35 + 0.65 * Math.abs(Math.sin(n * 0.9 + 0.5))) * maxH);
        for (let y = baseY - hgt; y < baseY; y++) {
          for (let x = bx; x < bx + barW; x++) {
            add(x, y, ACCENT[0] * alpha, ACCENT[1] * alpha, ACCENT[2] * alpha);
          }
        }
      }
    },
  };
}

function encodeBMP(canvas) {
  const { w, h, px } = canvas;
  const rowSize = Math.ceil((w * 3) / 4) * 4;
  const imgSize = rowSize * h;
  const buf = Buffer.alloc(54 + imgSize);
  buf.write("BM", 0);
  buf.writeUInt32LE(54 + imgSize, 2);
  buf.writeUInt32LE(54, 10);
  buf.writeUInt32LE(40, 14);
  buf.writeInt32LE(w, 18);
  buf.writeInt32LE(h, 22);
  buf.writeUInt16LE(1, 26);
  buf.writeUInt16LE(24, 28);
  buf.writeUInt32LE(0, 30);
  buf.writeUInt32LE(imgSize, 34);
  buf.writeInt32LE(2835, 38);
  buf.writeInt32LE(2835, 42);
  for (let y = 0; y < h; y++) {
    const srcY = h - 1 - y; // BMP rows are stored bottom-up
    let off = 54 + y * rowSize;
    for (let x = 0; x < w; x++) {
      const i = (srcY * w + x) * 3;
      buf[off++] = clamp(px[i + 2]); // B
      buf[off++] = clamp(px[i + 1]); // G
      buf[off++] = clamp(px[i]); // R
    }
  }
  return buf;
}

/** Preview PNG (not shipped) so the design can be eyeballed. */
function writePreview(canvas, name) {
  const { w, h, px } = canvas;
  const png = new PNG({ width: w, height: h });
  for (let i = 0; i < w * h; i++) {
    png.data[i * 4] = clamp(px[i * 3]);
    png.data[i * 4 + 1] = clamp(px[i * 3 + 1]);
    png.data[i * 4 + 2] = clamp(px[i * 3 + 2]);
    png.data[i * 4 + 3] = 255;
  }
  const p = path.join(os.tmpdir(), name);
  fs.writeFileSync(p, PNG.sync.write(png));
  console.log("preview:", p);
}

const logoPng = PNG.sync.read(fs.readFileSync(LOGO));
fs.mkdirSync(OUT_DIR, { recursive: true });

// --- Sidebar (164×314): welcome / finish page ---
{
  const c = makeCanvas(164, 314);
  c.gradient(true);
  c.glow(82, 118, 130, 0.5);
  const size = 108;
  const logo = downscale(logoPng.data, logoPng.width, logoPng.height, size, size);
  c.composite(logo, size, size, Math.round((164 - size) / 2), 64);
  c.eqBars(14, 296, 16, 6, 3, 40, 0.22);
  fs.writeFileSync(path.join(OUT_DIR, "sidebar.bmp"), encodeBMP(c));
  writePreview(c, "jellysic-sidebar.png");
}

// --- Header (150×57): inner pages ---
{
  const c = makeCanvas(150, 57);
  c.gradient(false);
  c.glow(28, 28, 40, 0.5);
  const size = 40;
  const logo = downscale(logoPng.data, logoPng.width, logoPng.height, size, size);
  c.composite(logo, size, size, 9, 8);
  c.eqBars(96, 44, 9, 4, 2, 26, 0.25);
  fs.writeFileSync(path.join(OUT_DIR, "header.bmp"), encodeBMP(c));
  writePreview(c, "jellysic-header.png");
}

console.log("Wrote src-tauri/installer/{sidebar,header}.bmp");
