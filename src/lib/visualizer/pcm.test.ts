import { describe, expect, it } from "vitest";
import { decodeFrame, resampleInterleaved, toMono, toStereo } from "$lib/visualizer/pcm";

/** A frame exactly as src-tauri/src/player/tap.rs encodes it. */
function frame(sampleRate: number, channels: number, samples: number[]): ArrayBuffer {
  const buf = new ArrayBuffer(8 + samples.length * 4);
  const view = new DataView(buf);
  view.setUint32(0, sampleRate, true);
  view.setUint32(4, channels, true);
  samples.forEach((s, i) => view.setFloat32(8 + i * 4, s, true));
  return buf;
}

describe("decodeFrame", () => {
  it("reads the rate, the channel count and the interleaved samples", () => {
    const pcm = decodeFrame(frame(44_100, 2, [0.5, -0.5, 0.25, -0.25]));
    expect(pcm?.sampleRate).toBe(44_100);
    expect(pcm?.channels).toBe(2);
    expect([...pcm!.samples]).toEqual([0.5, -0.5, 0.25, -0.25]);
  });

  it("drops a trailing half frame and rejects garbage", () => {
    expect(decodeFrame(frame(48_000, 2, [0.5, -0.5, 0.25]))!.samples).toHaveLength(2);
    expect(decodeFrame(frame(48_000, 0, [0.5]))).toBeNull();
    expect(decodeFrame(frame(48_000, 99, [0.5]))).toBeNull();
    expect(decodeFrame(new ArrayBuffer(6))).toBeNull();
  });
});

describe("channel shaping", () => {
  it("averages the channels for the mid signal", () => {
    expect([...toMono(new Float32Array([1, 0, 0.5, -0.5]), 2)]).toEqual([0.5, 0]);
  });

  it("duplicates mono and keeps stereo as is", () => {
    expect([...toStereo(new Float32Array([0.5, 0.25]), 1)]).toEqual([0.5, 0.5, 0.25, 0.25]);
    const stereo = new Float32Array([0.5, -0.5]);
    expect(toStereo(stereo, 2)).toBe(stereo);
  });
});

describe("resampleInterleaved", () => {
  it("resamples each channel separately — left never leaks into right", () => {
    // Left is constant +1, right constant -1, at 24 kHz → 48 kHz.
    const input = new Float32Array([1, -1, 1, -1, 1, -1, 1, -1]);
    const out = resampleInterleaved(input, 2, 24_000, 48_000);
    expect(out.length).toBe(16);
    for (let i = 0; i < out.length; i += 2) {
      expect(out[i]).toBe(1);
      expect(out[i + 1]).toBe(-1);
    }
  });

  it("interpolates linearly within a channel", () => {
    const out = resampleInterleaved(new Float32Array([0, 10, 1, 20]), 2, 2, 3);
    // Two frames become three (rate 2 → 3): halfway between them in the middle.
    expect([...out]).toEqual([0, 10, 0.5, 15, 1, 20]);
  });

  it("passes the input through when there is nothing to do", () => {
    const input = new Float32Array([0.5, -0.5]);
    expect(resampleInterleaved(input, 2, 48_000, 48_000)).toBe(input);
  });
});
