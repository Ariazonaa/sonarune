import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type { PlayerState, QueueTrack } from "#lib/types.js";

// The store mirrors Rust through events and commands: capture the event
// handlers it registers and answer its commands from a table.
const { handlers, clock } = vi.hoisted(() => {
  // Saved by an earlier session that quit muted (see lib/unmuteVolume.ts).
  localStorage.setItem("sonarune.unmuteVolume", JSON.stringify({ device: "", levels: { "": 0.2 } }));
  return {
    handlers: new Map<string, (event: { payload: unknown }) => void>(),
    clock: { now: 0 },
  };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    handlers.set(name, handler);
    return () => handlers.delete(name);
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { player } from "#lib/state/player.svelte.js";
import { session } from "#lib/state/session.svelte.js";
import type { SessionInfo } from "#lib/types.js";

const invokeMock = vi.mocked(invoke);

const track = (itemId: string, patch: Partial<QueueTrack> = {}) =>
  ({ itemId, entryId: `entry-${itemId}`, isFavorite: false, ...patch }) as QueueTrack;

const state = (patch: Partial<PlayerState> = {}): PlayerState => ({
  status: "playing",
  pendingPause: false,
  current: track("a"),
  sleepRemainingMs: null,
  stopAfter: "off",
  stopAfterItem: null,
  stopAfterEntry: null,
  index: 0,
  queueLen: 1,
  positionMs: 10_000,
  durationMs: 200_000,
  volume: 0.5,
  shuffle: false,
  shuffleMode: "off",
  repeat: "off",
  bufferedMs: null,
  speed: 1,
  preservePitch: true,
  ...patch,
});

/** What `get_player_state` answers (a resync). */
let rustState = state({ volume: 0 });
let favoriteFails = false;

function emitState(next: PlayerState) {
  handlers.get("player:state")?.({ payload: next });
}

/** The output switched to `device` and its sound profile took over. */
function emitProfileApplied(device: string) {
  handlers.get("audio:profile-applied")?.({ payload: { device } });
}

const calls = (command: string) => invokeMock.mock.calls.filter(([cmd]) => cmd === command);

beforeAll(async () => {
  vi.spyOn(performance, "now").mockImplementation(() => clock.now);
  invokeMock.mockImplementation(async (command: string) => {
    switch (command) {
      case "get_player_state":
        return rustState;
      case "get_queue":
        return { tracks: [], index: 0, order: [], canUndo: false, playedCount: 0, duplicateCount: 0 };
      case "get_waveform":
        return null; // not analysed yet
      case "set_favorite":
        if (favoriteFails) throw "server refused";
        return undefined;
      default:
        return undefined;
    }
  });
  // Starts muted: Rust restored the 0 the last session quit with.
  await player.init();
});

beforeEach(() => {
  invokeMock.mockClear();
  favoriteFails = false;
});

describe("mute", () => {
  it("unmutes to the level saved before the restart, not to full volume", async () => {
    expect(player.state.volume).toBe(0);
    await player.toggleMute();
    expect(invokeMock).toHaveBeenCalledWith("player_set_volume", { volume: 0.2 });
  });

  it("comes back from a slider dragged to zero to where the drag began", async () => {
    clock.now = 10_000;
    emitState(state({ volume: 0.6 }));
    clock.now = 20_000; // listened at 0.6 for ten seconds, then the drag
    emitState(state({ volume: 0.3 }));
    clock.now = 20_040;
    emitState(state({ volume: 0.05 }));
    clock.now = 20_080;
    emitState(state({ volume: 0 }));
    await player.toggleMute();
    expect(invokeMock).toHaveBeenCalledWith("player_set_volume", { volume: 0.6 });
  });

  it("remembers the level mute was pressed at", async () => {
    clock.now = 30_000;
    emitState(state({ volume: 0.35 }));
    await player.toggleMute();
    expect(invokeMock).toHaveBeenCalledWith("player_set_volume", { volume: 0 });
    expect(JSON.parse(localStorage.getItem("sonarune.unmuteVolume")!).levels[""]).toBe(0.35);
  });

  it("unmutes a device its sound profile brought back muted to that device's level", async () => {
    clock.now = 100_000;
    emitProfileApplied("Headphones");
    emitState(state({ volume: 0.2 }));
    clock.now = 160_000;
    await player.toggleMute(); // M on the headphones; Rust saves their 0
    emitState(state({ volume: 0 }));

    // The speakers' profile: 0.8, listened to for minutes, then turned down.
    clock.now = 170_000;
    emitProfileApplied("Speakers");
    emitState(state({ volume: 0.8 }));
    clock.now = 400_000;
    emitState(state({ volume: 0.6 }));

    // Back on the headphones, whose profile restores their 0.
    clock.now = 500_000;
    emitProfileApplied("Headphones");
    emitState(state({ volume: 0 }));
    invokeMock.mockClear();
    await player.toggleMute();
    expect(invokeMock).toHaveBeenCalledWith("player_set_volume", { volume: 0.2 });
  });
});

describe("panels", () => {
  it("jumping to the playing track swaps the lyrics panel for the queue", () => {
    player.showQueue = false;
    player.showLyrics = true;
    const reveal = player.revealCurrent;
    player.jumpToCurrent();
    expect(player.showQueue).toBe(true);
    expect(player.showLyrics).toBe(false);
    expect(player.revealCurrent).toBe(reveal + 1);
  });
});

describe("waveform read-back", () => {
  it("asks once per track, and again after a resync", async () => {
    emitState(state({ current: track("w1") }));
    emitState(state({ current: track("w1") }));
    expect(calls("get_waveform")).toEqual([["get_waveform", { itemId: "w1" }]]);

    // The analysis finished while the window was hidden; its event is gone.
    rustState = state({ current: track("w1") });
    await player.resync();
    expect(calls("get_waveform")).toEqual([
      ["get_waveform", { itemId: "w1" }],
      ["get_waveform", { itemId: "w1" }],
    ]);
  });
});

describe("estimated position", () => {
  it("runs at the playback speed", () => {
    clock.now = 50_000;
    emitState(state({ positionMs: 10_000, speed: 2 }));
    clock.now = 50_300;
    expect(player.estimatedPositionMs()).toBe(10_600);

    emitState(state({ positionMs: 10_000, speed: 0.5 }));
    clock.now = 50_700;
    expect(player.estimatedPositionMs()).toBe(10_200);
  });

  it("stops running on from a stale state", () => {
    clock.now = 60_000;
    emitState(state({ positionMs: 10_000, speed: 1 }));
    clock.now = 60_000 + 5 * 60_000; // the WebView was suspended
    expect(player.estimatedPositionMs()).toBe(11_000);
  });

  it("stands still unless playing, and never passes the end", () => {
    clock.now = 70_000;
    emitState(state({ status: "paused", positionMs: 10_000 }));
    clock.now = 70_300;
    expect(player.estimatedPositionMs()).toBe(10_000);

    emitState(state({ positionMs: 199_900, durationMs: 200_000 }));
    clock.now = 71_000;
    expect(player.estimatedPositionMs()).toBe(200_000);
  });
});

describe("play/pause", () => {
  it("offers Play while a load is set to arrive paused", () => {
    emitState(state({ status: "loading", pendingPause: true }));
    expect(player.playing).toBe(false);
    emitState(state({ status: "loading", pendingPause: false }));
    expect(player.playing).toBe(true);
    emitState(state({ status: "playing" }));
    expect(player.playing).toBe(true);
    emitState(state({ status: "paused" }));
    expect(player.playing).toBe(false);
  });
});

describe("the error banner", () => {
  const emitError = (payload: unknown) => handlers.get("player:error")?.({ payload });
  const grain = track("grain", { name: "Grain" });
  const cannotPlayGrain = { code: "cannotPlay", track: "Grain", device: null, cause: { kind: "network" } };

  it("goes when the track it is about plays again", () => {
    emitState(state({ status: "paused", current: grain }));
    emitError(cannotPlayGrain);
    expect(player.error).not.toBeNull();
    emitState(state({ status: "loading", current: grain }));
    expect(player.error).not.toBeNull();
    emitState(state({ status: "playing", current: grain }));
    expect(player.error).toBeNull();
  });

  it("stays when the queue moves on to another track", () => {
    emitState(state({ status: "loading", current: grain }));
    emitError(cannotPlayGrain);
    emitState(state({ status: "playing", current: track("next", { name: "Next" }) }));
    expect(player.error).not.toBeNull();
    player.error = null;
  });

  it("stays for an error that is not about a track", () => {
    emitState(state({ status: "paused", current: grain }));
    emitError({ code: "outputFailed", track: null, device: "Speakers", cause: null });
    emitState(state({ status: "playing", current: grain }));
    expect(player.error).not.toBeNull();
    player.error = null;
  });

  it("goes when a stream that broke off plays on", () => {
    emitState(state({ status: "playing", current: grain }));
    handlers.get("player:stream-broke-off")?.({ payload: { name: "Grain" } });
    emitState(state({ status: "loading", current: grain }));
    emitState(state({ status: "playing", current: grain }));
    expect(player.error).toBeNull();
  });
});

describe("favorites", () => {
  it("shows a heart set elsewhere on the playing track", async () => {
    emitState(state({ current: track("f1", { isFavorite: false }) }));
    expect(player.currentIsFavorite).toBe(false);

    // The album page hearts the playing track.
    await player.setFavorite("f1", true);
    expect(player.currentIsFavorite).toBe(true);
    expect(player.isFavorite("f1", false)).toBe(true);
    // A stale queue snapshot does not take it back.
    emitState(state({ current: track("f1", { isFavorite: false }) }));
    expect(player.currentIsFavorite).toBe(true);

    // The favorites page removes it again; the next toggle favorites it.
    await player.setFavorite("f1", false);
    expect(player.currentIsFavorite).toBe(false);
    player.toggleCurrentFavorite();
    expect(invokeMock).toHaveBeenLastCalledWith("set_favorite", { itemId: "f1", favorite: true });
    expect(player.currentIsFavorite).toBe(true);
  });

  it("reverts and rejects when the server refuses", async () => {
    emitState(state({ current: track("f2", { isFavorite: false }) }));
    favoriteFails = true;
    await expect(player.setFavorite("f2", true)).rejects.toBe("server refused");
    expect(player.currentIsFavorite).toBe(false);
  });

  it("leaves tracks nobody touched to their snapshot", () => {
    expect(player.isFavorite("untouched", true)).toBe(true);
    expect(player.isFavorite("untouched", false)).toBe(false);
  });

  it("keeps one account's hearts from the next one on the same server", async () => {
    const signedIn = (userId: string): SessionInfo => ({
      serverUrl: "https://jellyfin.home",
      username: userId,
      userId,
      canDelete: false,
      canEdit: false,
    });
    session.info = signedIn("A");
    emitState(state({ current: track("f3", { isFavorite: false }) }));
    await player.setFavorite("f3", true);
    expect(player.currentIsFavorite).toBe(true);

    // A signs out, B signs in. f3 is not B's favorite.
    session.info = null;
    expect(player.isFavorite("f3", false)).toBe(false);
    session.info = signedIn("B");
    expect(player.currentIsFavorite).toBe(false);
    player.toggleCurrentFavorite();
    expect(invokeMock).toHaveBeenLastCalledWith("set_favorite", { itemId: "f3", favorite: true });
    expect(player.currentIsFavorite).toBe(true);
  });
});
