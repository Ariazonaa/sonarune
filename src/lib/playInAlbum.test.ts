import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { playInAlbum } from "./playInAlbum";

const invokeMock = vi.mocked(invoke);

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined as never);
});

describe("playInAlbum", () => {
  it("leaves finding the track in its album to one command", async () => {
    // A lookup in the WebView first would let a slower, older click play
    // after a newer one: the play takes its ticket in Rust, before fetching.
    await playInAlbum("t3", "album-1");
    expect(invokeMock.mock.calls).toEqual([
      ["play_track_in_album", { trackId: "t3", albumId: "album-1" }],
    ]);
  });

  it("plays a track without an album on its own instead of queueing it", async () => {
    await playInAlbum("loose", null);
    expect(invokeMock.mock.calls).toEqual([
      ["play_track_in_album", { trackId: "loose", albumId: null }],
    ]);
  });

  it("rejects when the play fails", async () => {
    invokeMock.mockRejectedValueOnce(new Error("offline"));
    await expect(playInAlbum("t1", "album-1")).rejects.toThrow("offline");
  });
});
