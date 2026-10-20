import { describe, expect, it } from "vitest";
import {
  currentPosition,
  hasNext,
  nextEntries,
  playOrder,
  playOrderItemIds,
  stopsAfter,
} from "./queueOrder";
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

describe("stopsAfter", () => {
  const armed = { stopAfter: "track" as const, stopAfterEntry: "e2", stopAfterItem: "x" };

  it("marks only the copy the stop was armed on", () => {
    expect(stopsAfter(armed, { entryId: "e1", itemId: "x" })).toBe(false);
    expect(stopsAfter(armed, { entryId: "e2", itemId: "x" })).toBe(true);
  });

  it("goes by the item id for an entry from before entry ids", () => {
    expect(stopsAfter(armed, { entryId: "", itemId: "x" })).toBe(true);
    expect(stopsAfter({ ...armed, stopAfterEntry: null }, { entryId: "e9", itemId: "x" })).toBe(
      true,
    );
    expect(stopsAfter(armed, { entryId: "", itemId: "y" })).toBe(false);
  });

  it("marks nothing while no stop is armed", () => {
    expect(stopsAfter({ ...armed, stopAfter: "off" }, { entryId: "e2", itemId: "x" })).toBe(false);
  });
});

describe("hasNext", () => {
  it("stops at the last track of the shuffle order, not of the stored one", () => {
    // Stored index 1 is last in play order; stored index 3 is not.
    expect(hasNext({ tracks, order: [2, 0, 3, 1], index: 1 }, "off")).toBe(false);
    expect(hasNext({ tracks, order: [2, 0, 3, 1], index: 3 }, "off")).toBe(true);
  });

  it("does not wrap under Repeat One, only under Repeat All", () => {
    const last = { tracks, order: [], index: 3 };
    expect(hasNext(last, "off")).toBe(false);
    expect(hasNext(last, "one")).toBe(false);
    expect(hasNext(last, "all")).toBe(true);
    expect(hasNext({ tracks, order: [], index: 2 }, "one")).toBe(true);
  });

  it("has nowhere to go in an empty queue", () => {
    expect(hasNext({ tracks: [], order: [], index: 0 }, "all")).toBe(false);
  });

  it("goes nowhere from an entry the order does not hold, like the player", () => {
    expect(hasNext({ tracks, order: [2, 0, 3, 1], index: 7 }, "off")).toBe(false);
    expect(hasNext({ tracks, order: [], index: 7 }, "off")).toBe(false);
  });

  it("reads no track, so a long queue costs only its order", () => {
    const untouchable = new Proxy([...tracks], {
      get(target, key, receiver) {
        if (typeof key === "string" && /^\d+$/.test(key)) throw new Error(`read track ${key}`);
        return Reflect.get(target, key, receiver);
      },
    });
    expect(hasNext({ tracks: untouchable, order: [2, 0, 3, 1], index: 0 }, "off")).toBe(true);
    expect(hasNext({ tracks: untouchable, order: [], index: 3 }, "off")).toBe(false);
  });
});

describe("playOrderItemIds", () => {
  it("lists the queue as it plays", () => {
    expect(playOrderItemIds({ tracks, order: [2, 0, 3, 1] })).toEqual(["c", "a", "d", "b"]);
    expect(playOrderItemIds({ tracks, order: [] })).toEqual(["a", "b", "c", "d"]);
  });
});
