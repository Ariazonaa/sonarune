import { describe, expect, it } from "vitest";
import { currentPosition, nextEntries, playOrder } from "./queueOrder";
import type { QueueTrack } from "./types";

const track = (itemId: string) => ({ itemId }) as QueueTrack;
const tracks = ["a", "b", "c", "d"].map(track);

describe("playOrder", () => {
  it("follows the stored order while shuffle is off", () => {
    expect(playOrder({ tracks, order: [] }).map((entry) => entry.index)).toEqual([0, 1, 2, 3]);
  });

  it("follows the shuffle order and keeps the stored indices", () => {
    const entries = playOrder({ tracks, order: [2, 0, 3, 1] });
    expect(entries.map((entry) => entry.track.itemId)).toEqual(["c", "a", "d", "b"]);
    expect(entries.map((entry) => entry.index)).toEqual([2, 0, 3, 1]);
  });

  it("falls back to the stored order when the order does not cover the queue", () => {
    expect(playOrder({ tracks, order: [1, 0] }).map((entry) => entry.index)).toEqual([0, 1, 2, 3]);
    expect(playOrder({ tracks, order: [0, 1, 2, 9] }).map((entry) => entry.index)).toEqual([
      0, 1, 2, 3,
    ]);
  });
});

describe("nextEntries", () => {
  it("lists what plays after the current track under shuffle", () => {
    const queue = { tracks, order: [2, 0, 3, 1], index: 0 };
    expect(currentPosition(queue, playOrder(queue))).toBe(1);
    expect(nextEntries(queue, 3).map((entry) => entry.track.itemId)).toEqual(["d", "b"]);
  });

  it("lists the stored successors without shuffle", () => {
    const queue = { tracks, order: [], index: 1 };
    expect(nextEntries(queue, 2).map((entry) => entry.index)).toEqual([2, 3]);
  });

  it("is empty for an empty queue", () => {
    expect(nextEntries({ tracks: [], order: [], index: 0 }, 3)).toEqual([]);
  });
});
