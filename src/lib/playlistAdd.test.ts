import { describe, expect, it } from "vitest";
import { m } from "#lib/paraglide/messages.js";
import { playlistAddedToast } from "./playlistAdd";

describe("playlistAddedToast", () => {
  it("says added when everything went in, or when nobody could count", () => {
    const done = { message: m.add_to_playlist_done({ name: "Mix" }), kind: "success" };
    expect(playlistAddedToast("Mix", ["a", "b"], 2)).toEqual(done);
    expect(playlistAddedToast("Mix", ["a", "b"], null)).toEqual(done);
    // The same track twice in one add is one track.
    expect(playlistAddedToast("Mix", ["a", "a"], 1)).toEqual(done);
  });

  it("does not claim an add the playlist skipped", () => {
    expect(playlistAddedToast("Mix", ["a"], 0)).toEqual({
      message: m.playlist_already_has_one({ name: "Mix" }),
      kind: "info",
    });
    expect(playlistAddedToast("Mix", ["a", "b"], 0)).toEqual({
      message: m.playlist_already_has_many({ name: "Mix" }),
      kind: "info",
    });
    expect(playlistAddedToast("Mix", ["a", "b", "c"], 1)).toEqual({
      message: m.add_to_playlist_partial({ count: 1, total: 3, name: "Mix" }),
      kind: "success",
    });
  });
});
