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

/** Plays like the audio thread, in small render quanta; the left channel. */
function play(feed: Feed, frames: number, quantum = 8): number[] {
  const heard: number[] = [];
  for (let done = 0; done < frames; done += quantum) {
    heard.push(...render(feed, Math.min(quantum, frames - done))[0]);
  }
  return heard;
}

/** Chunks of frames numbered from 1 (left n, right -n): 0 only means silence. */
function numberedChunks() {
  let next = 1;
  return (frames: number) => {
    const data = new Float32Array(frames * 2);
    for (let f = 0; f < frames; f++) {
      data[2 * f] = next;
      data[2 * f + 1] = -next;
      next += 1;
    }
    return data;
  };
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
    const chunk = numberedChunks();
    // 1 s of 50 ms chunks at once at a 1 kHz "sample rate": far beyond 3x the
    // 100 ms target. (Every message is one chunk, and a chunk is never cut.)
    for (let i = 0; i < SAMPLE_RATE / 50; i++) feed.push(chunk(50));
    const [l, r] = render(feed, 300);
    const played = [...l].filter((frame) => frame !== 0);
    // No more than the 300 ms clamp is left, the newest frames, and right
    // still mirrors left.
    expect(played[0]).toBeGreaterThan(SAMPLE_RATE - 300);
    expect(played.at(-1)).toBe(SAMPLE_RATE);
    for (let i = 0; i < played.length; i++) expect(r[i]).toBe(-l[i]);
  });

  // Slow playback of a low-rate track: a tap chunk (2048 frames) of an 8 kHz
  // track lasts 341 ms at 0.75x and 512 ms at 0.5x. 400 ms is past the burst
  // clamp, 800 ms past the backlog window.
  it.each([400, 800])("plays %i ms chunks whole and back to back", (CHUNK) => {
    const feed = new Processor();
    const chunk = numberedChunks();
    const heard: number[] = [];
    // Delivered at exactly the pace they are played: each chunk arrives as
    // the one before has played out.
    for (let i = 0; i < 12; i++) {
      feed.push(chunk(CHUNK));
      heard.push(...play(feed, CHUNK));
    }
    // Every frame, in order: no silence, nothing dropped.
    expect(heard.filter((frame) => frame === 0)).toHaveLength(0);
    expect(heard.at(-1)).toBe(12 * CHUNK);
  });

  it("cuts a burst of long chunks back to the newest one, whole", () => {
    const feed = new Processor();
    const chunk = numberedChunks();
    const CHUNK = 400;
    // A stall held three chunks back; they arrive at once.
    for (let i = 0; i < 3; i++) feed.push(chunk(CHUNK));
    const heard = play(feed, CHUNK);
    expect(heard[0]).toBe(2 * CHUNK + 1);
    expect(heard.at(-1)).toBe(3 * CHUNK);
    // And on at the chunks' own pace, without a gap.
    for (let i = 0; i < 3; i++) {
      feed.push(chunk(CHUNK));
      heard.push(...play(feed, CHUNK));
    }
    expect(heard.filter((frame) => frame === 0)).toHaveLength(0);
    expect(heard.at(-1)).toBe(6 * CHUNK);
  });

  it("drops the backlog a stall left behind instead of lagging for good", () => {
    const feed = new Processor();
    const CHUNK = 43; // ~one tap frame
    let next = 1; // frame numbers: 0 only ever means silence
    const chunk = () => {
      const data = new Float32Array(CHUNK * 2);
      for (let f = 0; f < CHUNK; f++) {
        data[2 * f] = next;
        data[2 * f + 1] = -next;
        next += 1;
      }
      return data;
    };
    const play = (frames: number) => render(feed, frames)[0];

    for (let i = 0; i < 10; i++) {
      feed.push(chunk());
      play(CHUNK);
    }
    // The main thread stalls for 215 ms (a preset switch): the worklet runs
    // dry, then the queued chunks arrive at once — less than the hard trim.
    play(215);
    for (let i = 0; i < 5; i++) feed.push(chunk());

    // Steady delivery again, at exactly the rate it is played.
    let silent = 0;
    let last = 0;
    for (let i = 0; i < 40; i++) {
      feed.push(chunk());
      const out = play(CHUNK);
      silent += out.filter((sample) => sample === 0).length;
      last = out[out.length - 1];
    }
    // The output trails the newest frame by the jitter margin, not by the
    // stall, and the trim never ran it dry.
    expect(next - 1 - last).toBeLessThan(100);
    expect(silent).toBe(0);
  });
});
