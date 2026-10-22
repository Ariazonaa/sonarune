// @vitest-environment node
import { readFileSync } from "node:fs";
import { PNG } from "pngjs";
import { describe, expect, it } from "vitest";

// The outputs of scripts/make-logo.mjs. The accent field's rounded corners are
// the only antialiased edge with nothing drawn over it, so a semi-transparent
// pixel there has to carry the accent itself (ACCENT in make-logo.mjs):
// PNG and ICO store straight alpha, and a colour still scaled by its coverage
// shows as a dark fringe on a light taskbar.
const ACCENT = [0x1d, 0xb9, 0x54];

type Pixels = { size: number; rgba: Uint8Array };

function read(file: string): Buffer {
  return readFileSync(new URL(`../${file}`, import.meta.url));
}

function decodePng(bytes: Buffer): Pixels {
  const png = PNG.sync.read(bytes);
  return { size: png.width, rgba: png.data };
}

/** Every image in an .ico: a PNG, or a 32-bit bottom-up BGRA DIB. */
function decodeIco(bytes: Buffer): Pixels[] {
  const images: Pixels[] = [];
  for (let n = 0; n < bytes.readUInt16LE(4); n++) {
    const entry = 6 + n * 16;
    const data = bytes.subarray(
      bytes.readUInt32LE(entry + 12),
      bytes.readUInt32LE(entry + 12) + bytes.readUInt32LE(entry + 8),
    );
    if (data.readUInt32BE(0) === 0x89504e47) {
      images.push(decodePng(data));
      continue;
    }
    const size = data.readInt32LE(4);
    const rgba = new Uint8Array(size * size * 4);
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) {
        const s = 40 + (((size - 1 - y) * size + x) << 2);
        const d = (y * size + x) << 2;
        rgba.set([data[s + 2], data[s + 1], data[s], data[s + 3]], d);
      }
    }
    images.push({ size, rgba });
  }
  return images;
}

/** Semi-transparent pixels whose colour is not the accent. */
function darkened({ rgba }: Pixels): { edge: number; off: string[] } {
  const off: string[] = [];
  let edge = 0;
  for (let i = 0; i < rgba.length; i += 4) {
    const alpha = rgba[i + 3];
    if (alpha === 0 || alpha === 255) continue;
    edge += 1;
    const rgb = [rgba[i], rgba[i + 1], rgba[i + 2]];
    if (rgb.some((c, k) => Math.abs(c - ACCENT[k]) > 1)) off.push(`${rgb.join(",")},a${alpha}`);
  }
  return { edge, off: [...new Set(off)] };
}

describe("generated logo", () => {
  it.each([
    "src-tauri/icons/32x32.png",
    "src-tauri/icons/128x128.png",
    "src-tauri/icons/icon.png",
    "static/favicon.png",
    "docs/logo.png",
  ])("%s keeps the accent at its antialiased corners", (file) => {
    const { edge, off } = darkened(decodePng(read(file)));
    expect(edge).toBeGreaterThan(0);
    expect(off).toEqual([]);
  });

  it("icon.ico keeps the accent at the corners of every size", () => {
    const images = decodeIco(read("src-tauri/icons/icon.ico"));
    expect(images.map((image) => image.size)).toEqual([16, 24, 32, 48, 64, 128, 256]);
    for (const image of images) {
      const { edge, off } = darkened(image);
      expect(edge, `${image.size}px`).toBeGreaterThan(0);
      expect(off, `${image.size}px`).toEqual([]);
    }
  });
});
