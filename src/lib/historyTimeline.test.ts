import { describe, expect, it } from "vitest";
import type { HistoryEntry } from "#lib/types.js";
import { dayLabel, formatListened, groupTimeline } from "./historyTimeline";

let nextId = 1000;
function play(itemId: string, at: Date): HistoryEntry {
  return {
    id: nextId--,
    playedAt: at.getTime(),
    listenedMs: 180_000,
    track: {
      itemId,
      name: `Track ${itemId}`,
      artist: "Artist",
      album: "Album",
      albumId: null,
      artists: [],
      durationMs: 200_000,
      imageItemId: null,
      imageTag: null,
      imageBlurHash: null,
    },
  };
}

describe("groupTimeline", () => {
  it("splits by local day and folds only direct repeats", () => {
    const days = groupTimeline([
      play("a", new Date(2026, 9, 19, 22, 0)),
      play("a", new Date(2026, 9, 19, 21, 55)),
      play("b", new Date(2026, 9, 19, 21, 50)),
      play("a", new Date(2026, 9, 19, 21, 45)),
      play("a", new Date(2026, 9, 18, 23, 59)),
    ]);
    expect(days.map((day) => day.key)).toEqual(["2026-10-19", "2026-10-18"]);
    expect(days[0].rows.map((row) => [row.entry.track.itemId, row.entries.length])).toEqual([
      ["a", 2],
      ["b", 1],
      ["a", 1],
    ]);
    // Across midnight: two days, two rows.
    expect(days[1].rows).toHaveLength(1);
    expect(days[0].rows[0].entry.playedAt).toBe(new Date(2026, 9, 19, 22, 0).getTime());
  });

  it("is empty for no plays", () => {
    expect(groupTimeline([])).toEqual([]);
  });
});

describe("dayLabel", () => {
  const labels = { today: "Today", yesterday: "Yesterday" };
  const now = new Date(2026, 9, 19, 9, 30);

  it("names today and yesterday", () => {
    expect(dayLabel(new Date(2026, 9, 19), now, labels, "en")).toBe("Today");
    expect(dayLabel(new Date(2026, 9, 18), now, labels, "en")).toBe("Yesterday");
  });

  it("writes the year only for another year", () => {
    expect(dayLabel(new Date(2026, 9, 1), now, labels, "en")).not.toContain("2026");
    expect(dayLabel(new Date(2025, 9, 1), now, labels, "en")).toContain("2025");
  });
});

describe("formatListened", () => {
  it("splits into hours and minutes", () => {
    expect(formatListened(3 * 3_600_000 + 5 * 60_000 + 59_000)).toEqual({ hours: 3, minutes: 5 });
    expect(formatListened(-1)).toEqual({ hours: 0, minutes: 0 });
  });
});
