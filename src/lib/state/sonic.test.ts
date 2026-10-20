import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionInfo } from "#lib/types.js";

const { invokeMock, show, player, session } = vi.hoisted(() => ({
  invokeMock: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
  show: vi.fn(),
  player: { run: vi.fn(), error: null as string | null },
  session: { info: null as SessionInfo | null },
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
// Messages as their keys.
vi.mock("#lib/paraglide/messages.js", () => ({
  m: new Proxy({}, { get: (_target, key) => () => String(key) }),
}));
vi.mock("#lib/state/player.svelte.js", () => ({ player }));
vi.mock("#lib/state/session.svelte.js", () => ({ session }));
vi.mock("#lib/state/toast.svelte.js", () => ({ toast: { show } }));

const { sonic } = await import("#lib/state/sonic.svelte.js");

beforeEach(() => {
  session.info = {
    serverUrl: "https://music.home",
    username: "me",
    userId: "u1",
    canDelete: false,
    canEdit: false,
  };
  invokeMock.mockReset();
  show.mockClear();
  player.error = null;
});

/** What a failed sonic command left the user with: a toast's text, or the banner. */
function told() {
  return { toast: show.mock.calls.map((call) => call[0]), banner: player.error };
}

describe("a sonic radio or path that cannot be made", () => {
  it("says a track is too short or too quiet when it has no sound to compare", async () => {
    invokeMock.mockRejectedValueOnce("sonic:no-sound");
    await sonic.radio("jingle");
    expect(told()).toEqual({ toast: ["sonic_no_sound"], banner: null });

    sonic.startPath("a", "Track a");
    show.mockClear();
    invokeMock.mockRejectedValueOnce("sonic:no-sound");
    await sonic.finishPath("jingle");
    expect(told()).toEqual({ toast: ["sonic_path_no_sound"], banner: null });
  });

  it("points to the library analysis when a track has not been analysed", async () => {
    invokeMock.mockRejectedValueOnce("sonic:not-analysed");
    await sonic.radio("new");
    expect(told()).toEqual({ toast: ["sonic_not_analysed"], banner: null });
  });

  it("shows any other failure as an error", async () => {
    invokeMock.mockRejectedValueOnce("none of these tracks are on the server");
    await sonic.radio("gone");
    expect(told()).toEqual({ toast: [], banner: "none of these tracks are on the server" });
  });
});
