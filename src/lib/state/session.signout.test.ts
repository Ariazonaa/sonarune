import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import { session } from "#lib/state/session.svelte.js";
import { downloads } from "#lib/state/downloads.svelte.js";
import { library } from "#lib/state/library.svelte.js";
import { net } from "#lib/state/net.svelte.js";
import { player } from "#lib/state/player.svelte.js";
import { playlists } from "#lib/state/playlists.svelte.js";
import { sonic } from "#lib/state/sonic.svelte.js";
import type { DownloadTask, PlaylistDto } from "#lib/types.js";

const invokeMock = vi.mocked(invoke);
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

const playlist = { id: "p1", name: "Private mix" } as PlaylistDto;
const task: DownloadTask = {
  id: "d1",
  itemId: "t1",
  name: "Someone's track.flac",
  status: "queued",
  receivedBytes: 0,
  totalBytes: null,
  path: null,
  error: null,
};

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined as never);
  localStorage.clear();
});

describe("sign-out", () => {
  it("leaves nothing of the account to the next one", async () => {
    session.info = {
      serverUrl: "https://music.home",
      username: "a",
      userId: "a",
      canDelete: false,
      canEdit: false,
    };
    playlists.items = [playlist];
    downloads.tasks = [task];
    downloads.open = true;
    sonic.startPath("t1", "A track");
    expect(sonic.pathStart).not.toBeNull();
    net.online = false;
    player.error = "server returned 500";
    library.lastSyncedAt = 1;
    localStorage.setItem("sonarune:searchHistory", JSON.stringify(["before the update"]));

    await session.disconnect();

    expect(invokeMock).toHaveBeenCalledWith("disconnect");
    expect(session.info).toBeNull();
    expect(playlists.items).toEqual([]);
    expect(downloads.tasks).toEqual([]);
    expect(downloads.open).toBe(false);
    expect(sonic.pathStart).toBeNull();
    expect(net.online).toBe(true);
    expect(player.error).toBeNull();
    expect(library.lastSyncedAt).toBeNull();
    expect(localStorage.getItem("sonarune:searchHistory")).toBeNull();
  });

  it("drops a playlist load that was still on its way", async () => {
    let answer: (items: PlaylistDto[]) => void = () => {};
    invokeMock.mockImplementation(((command: string) =>
      command === "get_playlists"
        ? new Promise<PlaylistDto[]>((resolve) => (answer = resolve))
        : Promise.resolve(undefined)) as typeof invoke);
    playlists.refresh();
    await session.disconnect();
    answer([playlist]);
    await settle();
    expect(playlists.items).toEqual([]);
  });

  it("keeps everything when the sign-out itself fails", async () => {
    invokeMock.mockRejectedValueOnce("credential store error");
    playlists.items = [playlist];
    await expect(session.disconnect()).rejects.toBe("credential store error");
    expect(playlists.items).toEqual([playlist]);
  });
});
