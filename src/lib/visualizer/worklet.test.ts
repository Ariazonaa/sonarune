import { beforeAll, describe, expect, it } from "vitest";

// The worklet runs in an AudioWorkletGlobalScope. Recreate the three globals
// it needs and run the real file (static/visualizer-worklet.js).
const SAMPLE_RATE = 1000;

interface Feed {
  push(data: Float32Array): void;
  process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean;
  port: { onmessage: ((e: { data: Float32Array }) => void) | null };
}

let Processor: new () => Feed;

beforeAll(async () => {
  Object.assign(globalThis, {
    sampleRate: SAMPLE_RATE,
    AudioWorkletProcessor: class {
      port = { onmessage: null };
    },
    registerProcessor: (_name: string, ctor: new () => Feed) => {
      Processor = ctor;
    },
  });
  const path = "../../../static/visualizer-worklet.js";
  await import(/* @vite-ignore */ path);
});

function render(feed: Feed, frames: number, channels = 2): Float32Array[] {
  const out = Array.from({ length: channels }, () => new Float32Array(frames));
  feed.process([], [out]);
  return out;
}

describe("visualizer worklet", () => {
  it("puts left on channel 0 and right on channel 1", () => {
    const feed = new Processor();
    feed.port.onmessage!({ data: new Float32Array([0.5, -0.5, 0.25, -0.25]) });
    const [l, r] = render(feed, 2);
    expect([...l]).toEqual([0.5, 0.25]);
    expect([...r]).toEqual([-0.5, -0.25]);
  });

  it("plays silence when it runs dry instead of repeating", () => {
    const feed = new Processor();
    feed.push(new Float32Array([1, -1]));
    const [l, r] = render(feed, 3);
    expect([...l]).toEqual([1, 0, 0]);
    expect([...r]).toEqual([-1, 0, 0]);
  });

  it("gives a mono output the mid signal", () => {
    const feed = new Processor();
    feed.push(new Float32Array([1, 0]));
    const [mono] = render(feed, 1, 1);
    expect(mono[0]).toBe(0.5);
  });

  it("skips ahead when the producer runs far ahead, keeping the pairs intact", () => {
    const feed = new Processor();
    // 1 s of frames at a 1 kHz "sample rate": far beyond 3x the 100 ms target.
    const burst = new Float32Array(2 * SAMPLE_RATE);
    for (let f = 0; f < SAMPLE_RATE; f++) {
      burst[2 * f] = f;
      burst[2 * f + 1] = -f;
    }
    feed.push(burst);
    const [l, r] = render(feed, 100);
    // Only the newest ~100 ms is left, and right still mirrors left.
    expect(l[0]).toBeGreaterThanOrEqual(SAMPLE_RATE - 100);
    for (let i = 0; i < l.length; i++) expect(r[i]).toBe(-l[i]);
  });
});
