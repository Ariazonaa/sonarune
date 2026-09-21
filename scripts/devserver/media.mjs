// Generates the fake library's media: one cover per album (pure Node, via the
// pngjs devDependency the installer art already uses) and one short audio file
// per track (ffmpeg). Both are written to a cache directory outside the repo --
// the generator is checked in, its output is not.
//
// Covers are abstract on purpose: drawing text would need a font rasterizer,
// and a geometric cover derived from the album id looks like an album cover
// without one.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { PNG } from "pngjs";

const SIZE = 640;

/** Deterministic PRNG so a given album always gets the same cover. */
function rng(seedHex) {
  let s = parseInt(seedHex.slice(0, 8), 16) || 1;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 0x100000000;
  };
}

function hsl(h, s, l) {
  const k = (n) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [f(0) * 255, f(8) * 255, f(4) * 255];
}

/** A diagonal two-tone gradient with a few translucent bands over it. */
export function renderCover(seedHex) {
  const rand = rng(seedHex);
  const hue = Math.floor(rand() * 360);
  const hue2 = (hue + 40 + Math.floor(rand() * 120)) % 360;
  const top = hsl(hue, 0.55 + rand() * 0.25, 0.22 + rand() * 0.12);
  const bottom = hsl(hue2, 0.5 + rand() * 0.3, 0.05 + rand() * 0.08);

  const png = new PNG({ width: SIZE, height: SIZE });
  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      const t = (x / SIZE) * 0.35 + (y / SIZE) * 0.65;
      const i = (y * SIZE + x) << 2;
      png.data[i] = top[0] * (1 - t) + bottom[0] * t;
      png.data[i + 1] = top[1] * (1 - t) + bottom[1] * t;
      png.data[i + 2] = top[2] * (1 - t) + bottom[2] * t;
      png.data[i + 3] = 255;
    }
  }

  // Three soft bands, each a rotated stripe with its own tint.
  const bands = 2 + Math.floor(rand() * 3);
  for (let b = 0; b < bands; b++) {
    const angle = rand() * Math.PI;
    const offset = rand() * SIZE;
    const width = SIZE * (0.05 + rand() * 0.12);
    const tint = hsl((hue + 180 + rand() * 60) % 360, 0.7, 0.6);
    const alpha = 0.08 + rand() * 0.14;
    const cos = Math.cos(angle);
    const sin = Math.sin(angle);
    for (let y = 0; y < SIZE; y++) {
      for (let x = 0; x < SIZE; x++) {
        const d = Math.abs(x * cos + y * sin - offset);
        if (d > width) continue;
        const fade = (1 - d / width) * alpha;
        const i = (y * SIZE + x) << 2;
        png.data[i] += (tint[0] - png.data[i]) * fade;
        png.data[i + 1] += (tint[1] - png.data[i + 1]) * fade;
        png.data[i + 2] += (tint[2] - png.data[i + 2]) * fade;
      }
    }
  }

  // A circle off-centre, the way a lot of sleeve art sits.
  const cx = SIZE * (0.3 + rand() * 0.4);
  const cy = SIZE * (0.3 + rand() * 0.4);
  const r = SIZE * (0.12 + rand() * 0.14);
  const ring = hsl((hue + 200) % 360, 0.25, 0.85);
  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      const d = Math.abs(Math.hypot(x - cx, y - cy) - r);
      if (d > 2.5) continue;
      const fade = (1 - d / 2.5) * 0.55;
      const i = (y * SIZE + x) << 2;
      png.data[i] += (ring[0] - png.data[i]) * fade;
      png.data[i + 1] += (ring[1] - png.data[i + 1]) * fade;
      png.data[i + 2] += (ring[2] - png.data[i + 2]) * fade;
    }
  }

  return PNG.sync.write(png);
}

/**
 * A short, listenable stand-in. Three static sine tones would be simpler, but
 * every visualizer preset then renders the same pale ring: MilkDrop reacts to
 * bass/mid/treble ENERGY, and a constant tone has none of it. So this is a
 * small loop with transients -- kick, clap, hats, a bass line and a pad --
 * synthesized as raw PCM here and handed to ffmpeg to encode.
 */
function renderPcm(seedHex, seconds, rate) {
  const rand = rng(seedHex);
  const bpm = 96 + Math.floor(rand() * 32);
  const beat = (60 / bpm) * rate;
  const n = Math.floor(seconds * rate);
  const left = new Float32Array(n);
  const right = new Float32Array(n);

  const root = 55 * Math.pow(2, Math.floor(rand() * 5) / 12);
  const progression = [0, 5, 3, 7].map((semi) => root * Math.pow(2, semi / 12));

  let noise = 1;
  const white = () => {
    // xorshift: cheap, deterministic, good enough for a hat.
    noise ^= noise << 13;
    noise ^= noise >>> 17;
    noise ^= noise << 5;
    return (noise / 0x80000000) % 1;
  };

  for (let i = 0; i < n; i++) {
    const t = i / rate;
    const beatPos = i / beat;
    const step = Math.floor(beatPos * 2) % 8; // eighth notes
    const bar = Math.floor(beatPos / 4) % progression.length;
    const intoStep = (beatPos * 2) % 1;

    // Kick on 1 and 3, pitched down fast.
    let s = 0;
    if (step === 0 || step === 4) {
      const env = Math.exp(-intoStep * 9);
      s += 0.9 * env * Math.sin(2 * Math.PI * (48 + 90 * Math.exp(-intoStep * 22)) * t);
    }
    // Clap on 2 and 4.
    if (step === 2 || step === 6) {
      const env = Math.exp(-intoStep * 16);
      s += 0.35 * env * white();
    }
    // Hats on every eighth, quieter off-beat.
    {
      const env = Math.exp(-intoStep * 40);
      s += (step % 2 === 1 ? 0.12 : 0.07) * env * white();
    }
    // Bass: the chord root, an octave down, gently gated.
    {
      const f = progression[bar] / 2;
      const gate = 0.6 + 0.4 * Math.exp(-intoStep * 4);
      s += 0.3 * gate * Math.sin(2 * Math.PI * f * t);
    }
    // Pad: root + fifth + octave, slow tremolo, wide.
    const f0 = progression[bar];
    const trem = 0.7 + 0.3 * Math.sin(2 * Math.PI * 0.35 * t);
    const padL = 0.16 * Math.sin(2 * Math.PI * f0 * 2 * t) + 0.10 * Math.sin(2 * Math.PI * f0 * 3 * t);
    const padR = 0.16 * Math.sin(2 * Math.PI * f0 * 2 * t + 0.4) + 0.10 * Math.sin(2 * Math.PI * f0 * 4.01 * t);

    left[i] = Math.max(-1, Math.min(1, s + padL * trem));
    right[i] = Math.max(-1, Math.min(1, s + padR * trem));
  }

  const buf = Buffer.alloc(n * 4); // 2ch * 16 bit
  for (let i = 0; i < n; i++) {
    buf.writeInt16LE(Math.round(left[i] * 32000), i * 4);
    buf.writeInt16LE(Math.round(right[i] * 32000), i * 4 + 2);
  }
  return buf;
}

function renderAudio(dest, seedHex) {
  const rate = 44100;
  const pcm = renderPcm(seedHex, 45, rate);
  const res = spawnSync(
    "ffmpeg",
    [
      "-hide_banner", "-loglevel", "error", "-y",
      "-f", "s16le", "-ar", String(rate), "-ac", "2", "-i", "pipe:0",
      "-c:a", "flac", "-compression_level", "5",
      dest,
    ],
    { input: pcm },
  );
  if (res.status !== 0) {
    throw new Error(`ffmpeg failed for ${dest}:
${res.stderr}`);
  }
}

/** Generates anything missing under `dir`. Returns the paths by item id. */
export function ensureMedia(dir, albums, tracks) {
  mkdirSync(join(dir, "covers"), { recursive: true });
  mkdirSync(join(dir, "audio"), { recursive: true });

  let madeCovers = 0;
  for (const album of albums) {
    const file = join(dir, "covers", `${album.id}.png`);
    if (existsSync(file)) continue;
    writeFileSync(file, renderCover(album.id));
    madeCovers++;
  }

  let madeAudio = 0;
  for (const track of tracks) {
    const file = join(dir, "audio", `${track.id}.flac`);
    if (existsSync(file)) continue;
    renderAudio(file, track.id);
    madeAudio++;
  }

  return { madeCovers, madeAudio };
}
