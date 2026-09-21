import { beforeEach, describe, expect, it } from "vitest";
import { loadLyricsOffset, lyricsOffsetKey, saveLyricsOffset } from "$lib/lyricsOffset";

beforeEach(() => localStorage.clear());

describe("lyrics offset persistence", () => {
  it("round-trips per track under the key every window reads", () => {
    expect(lyricsOffsetKey("abc")).toBe("jellysic.lyricsOffset.abc");
    saveLyricsOffset("abc", 750);
    expect(localStorage.getItem("jellysic.lyricsOffset.abc")).toBe("750");
    expect(loadLyricsOffset("abc")).toBe(750);
    expect(loadLyricsOffset("other")).toBe(0);
  });

  it("drops the key once the nudge is back at zero", () => {
    saveLyricsOffset("abc", -250);
    saveLyricsOffset("abc", 0);
    expect(localStorage.getItem("jellysic.lyricsOffset.abc")).toBeNull();
    expect(loadLyricsOffset("abc")).toBe(0);
  });

  it("reads a garbage value as no offset", () => {
    localStorage.setItem("jellysic.lyricsOffset.abc", "soon");
    expect(loadLyricsOffset("abc")).toBe(0);
  });

  it("never throws when storage is unavailable", () => {
    const denied = () => {
      throw new Error("denied");
    };
    const broken = { getItem: denied, setItem: denied, removeItem: denied } as unknown as Storage;
    expect(loadLyricsOffset("abc", broken)).toBe(0);
    expect(() => saveLyricsOffset("abc", 500, broken)).not.toThrow();
    expect(() => saveLyricsOffset("abc", 0, broken)).not.toThrow();
  });
});
