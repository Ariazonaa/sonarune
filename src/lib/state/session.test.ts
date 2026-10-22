import { beforeEach, describe, expect, it, vi } from "vitest";

// The backend ends a session on its own when the server refuses the stored
// password (or the server moved to https) and says so with `session:ended`.
// Before, nothing listened: the shell stayed signed in while every call
// failed with "not connected to a server".
type Handler = (event: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
const calls: string[] = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: Handler) => {
    calls.push(`listen ${name}`);
    handlers.set(name, handler);
    return () => handlers.delete(name);
  }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { net } from "#lib/state/net.svelte.js";
import { player } from "#lib/state/player.svelte.js";
import { playlists } from "#lib/state/playlists.svelte.js";
import { session } from "#lib/state/session.svelte.js";
import { swrGet, swrSet } from "#lib/swr.js";
import type { PlaylistDto, SessionEnded, SessionInfo } from "#lib/types.js";

const invokeMock = vi.mocked(invoke);

const info: SessionInfo = {
  serverUrl: "https://music.home",
  username: "alice",
  userId: "user-1",
  canDelete: false,
  canEdit: false,
};

const rejected: SessionEnded = {
  reason: "rejected",
  serverUrl: "https://music.home",
  username: "alice",
};

function end(payload: SessionEnded) {
  const handler = handlers.get("session:ended");
  expect(handler).toBeDefined();
  handler!({ payload });
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string) => {
    calls.push(`invoke ${command}`);
    if (command === "restore_session") return info as never;
    if (command === "connect") return { status: "connected", session: info } as never;
    return undefined as never;
  });
  calls.length = 0;
});

describe("session", () => {
  it("listens for an ended session before restoring, and drops back to sign-in", async () => {
    await session.restore({ followEnds: true });
    // The restore itself can end a session: the listener must already be up.
    expect(calls.slice(0, 2)).toEqual(["listen session:ended", "invoke restore_session"]);
    expect(session.info).toEqual(info);

    swrSet("album:a", { name: "cached" });
    end(rejected);
    expect(session.info).toBeNull();
    expect(session.ended).toEqual(rejected);
    // Nothing of the account that was signed in stays on screen.
    expect(swrGet("album:a")).toBeUndefined();
  });

  it("registers the listener once, however often it restores", async () => {
    await session.restore({ followEnds: true });
    expect(calls.filter((call) => call.startsWith("listen"))).toEqual([]);
  });

  it("leaves following an ended session to the window that shows the sign-in", async () => {
    // The projector restores only for its lyrics. Dropped there, its session
    // would stay gone after the next sign-in in the main window.
    vi.resetModules();
    const { session: projector } = await import("#lib/state/session.svelte.js");
    await projector.restore();
    expect(calls).toEqual(["invoke restore_session"]);
    expect(projector.info).toEqual(info);
  });

  it("clears the banner of a session that ended or was signed out", async () => {
    await session.restore({ followEnds: true });
    net.online = false;
    net.untrustedCertificate = "AB:CD";
    end(rejected);
    // The next session starts from a server it has just reached, not with a
    // fingerprint it never saw.
    expect(net.online).toBe(true);
    expect(net.untrustedCertificate).toBeNull();

    session.info = info;
    net.online = false;
    net.untrustedCertificate = "AB:CD";
    await session.disconnect();
    expect(net.online).toBe(true);
    expect(net.untrustedCertificate).toBeNull();
  });

  it("takes a repeated notice on the sign-in screen as nothing new", async () => {
    await session.restore({ followEnds: true });
    end(rejected);
    const shown = session.ended;
    // `check_server` repeats it on every focus while signed out.
    end({ ...rejected });
    expect(session.ended).toBe(shown);

    // Signed in again by a path that did not clear it: the same notice is
    // news again — the window may have missed the session ending.
    session.info = info;
    end({ ...rejected });
    expect(session.info).toBeNull();
  });

  it("forgets why the last session ended once a sign-in goes through", async () => {
    end({ reason: "moved", serverUrl: "https://music.home", username: "alice" });
    expect(session.ended?.reason).toBe("moved");
    await session.connect({
      serverUrl: "https://music.home",
      username: "alice",
      password: "new",
      acceptInvalidCerts: false,
    });
    expect(session.ended).toBeNull();
    expect(session.info).toEqual(info);
  });

  it("leaves nothing of the ended account on screen for the next one", async () => {
    await session.restore({ followEnds: true });
    playlists.items = [{ id: "p1", name: "Private mix" } as PlaylistDto];
    player.error = "server returned 500";
    end(rejected);
    expect(playlists.items).toEqual([]);
    expect(player.error).toBeNull();
  });

  it("forgets it on a sign-out too", async () => {
    end(rejected);
    await session.disconnect();
    expect(session.ended).toBeNull();
    expect(session.info).toBeNull();
  });
});
