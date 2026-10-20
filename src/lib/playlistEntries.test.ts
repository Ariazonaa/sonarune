import { describe, expect, it } from "vitest";
import { entryRowKeys, firstOccurrences, sharedEntryIds } from "./playlistEntries";
import type { PlaylistTrackDto, TrackDto } from "#lib/types.js";

/** An entry the way Jellyfin 10.11 lists it: its entry id is the track's id. */
function entry(trackId: string, entryId = trackId): PlaylistTrackDto {
  return { track: { id: trackId } as TrackDto, entryId };
}

describe("entryRowKeys", () => {
  it("is unique when a track is in the playlist twice", () => {
    const keys = entryRowKeys([entry("t"), entry("a"), entry("t"), entry("t")]);
    expect(new Set(keys).size).toBe(4);
    expect(keys).toEqual(["e:t", "e:a", "e:t#2", "e:t#3"]);
  });

  it("keeps a row's key while other rows move", () => {
    const before = entryRowKeys([entry("t"), entry("a"), entry("t")]);
    const after = entryRowKeys([entry("a"), entry("t"), entry("t")]);
    // "a" moved to the top; the two copies of "t" keep their keys.
    expect(before).toEqual(["e:t", "e:a", "e:t#2"]);
    expect(after).toEqual(["e:a", "e:t", "e:t#2"]);
  });

  it("copes with entries the server sent without an id", () => {
    expect(new Set(entryRowKeys([entry("a", ""), entry("b", "")])).size).toBe(2);
  });
});

describe("sharedEntryIds", () => {
  it("names the ids several rows carry, and how many", () => {
    const shared = sharedEntryIds([entry("t"), entry("a"), entry("t"), entry("b"), entry("t")]);
    expect([...shared]).toEqual([["t", 3]]);
  });

  it("is empty when every entry has its own id", () => {
    // A server with real per-entry ids: the same track, two entries.
    expect(sharedEntryIds([entry("t", "e1"), entry("t", "e2")]).size).toBe(0);
  });
});

describe("firstOccurrences", () => {
  it("keeps the first entry of every track, in order", () => {
    const entries = [entry("t"), entry("a"), entry("t"), entry("b"), entry("a")];
    expect(firstOccurrences(entries)).toEqual([entries[0], entries[1], entries[3]]);
  });
});
