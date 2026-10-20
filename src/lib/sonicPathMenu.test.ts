import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionInfo } from "#lib/types.js";

const { invokeMock, session } = vi.hoisted(() => ({
  invokeMock: vi.fn(async () => undefined),
  session: { info: null as SessionInfo | null },
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
// Messages as their keys, with the name a path entry carries.
vi.mock("#lib/paraglide/messages.js", () => ({
  m: new Proxy(
    {},
    {
      get:
        (_target, key) =>
        (params?: { name?: string }) =>
          params?.name ? `${String(key)}:${params.name}` : String(key),
    },
  ),
}));
vi.mock("#lib/state/player.svelte.js", () => ({ player: { run: vi.fn(), error: null } }));
vi.mock("#lib/state/session.svelte.js", () => ({ session }));
vi.mock("#lib/state/toast.svelte.js", () => ({ toast: { show: vi.fn() } }));

const { sonic } = await import("#lib/state/sonic.svelte.js");
const { sonicPathEntries } = await import("./sonicPathMenu");

const home: SessionInfo = {
  serverUrl: "https://music.home",
  username: "me",
  userId: "u1",
  canDelete: false,
  canEdit: false,
};

function track(id: string) {
  return { id, name: `Track ${id}` };
}

const entries = (id: string) => sonicPathEntries(track(id), "icon");

const labels = (id: string) => entries(id).map((item) => item.label);

beforeEach(() => {
  session.info = home;
  sonic.cancelPath();
  invokeMock.mockClear();
});

describe("sonic path entries", () => {
  it("offer to start a path when none is picked", () => {
    expect(labels("a")).toEqual(["ctx_sonic_path_from"]);
  });

  it("let a picked start be ended, moved or dropped", () => {
    sonic.startPath("a", "Track a");
    expect(labels("b")).toEqual(["ctx_sonic_path_to:Track a", "ctx_sonic_path_from", "ctx_sonic_path_cancel"]);
    // On the start itself, picking it again would change nothing.
    expect(labels("a")).toEqual(["ctx_sonic_path_cancel"]);

    entries("b")[1].action();
    expect(sonic.pathStart?.id).toBe("b");
    entries("c")[2].action();
    expect(sonic.pathStart).toBeNull();
    expect(labels("c")).toEqual(["ctx_sonic_path_from"]);
  });

  it("forget a start picked before a sign-out or a switch", async () => {
    sonic.startPath("a", "Track a");
    session.info = { ...home, serverUrl: "https://other.home" };
    expect(sonic.pathStart).toBeNull();
    expect(labels("b")).toEqual(["ctx_sonic_path_from"]);
    // An old item id is never sent to the new server.
    await sonic.finishPath("b");
    expect(invokeMock).not.toHaveBeenCalled();

    sonic.startPath("a", "Track a");
    session.info = null;
    expect(sonic.pathStart).toBeNull();
  });

  it("send the path from the start to the track picked", async () => {
    sonic.startPath("a", "Track a");
    entries("b")[0].action();
    await Promise.resolve();
    expect(invokeMock).toHaveBeenCalledWith("sonic_path", { fromId: "a", toId: "b", steps: 10 });
    expect(sonic.pathStart).toBeNull();
  });
});
