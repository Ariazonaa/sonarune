import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
// The menu only hands its play actions to the player; keep the Tauri-backed store out.
vi.mock("#lib/state/player.svelte.js", () => ({ player: { run: vi.fn() } }));

import { invoke } from "@tauri-apps/api/core";
import { m } from "#lib/paraglide/messages.js";
import { createAlbumMenu, type AlbumMenu } from "./albumMenu.svelte";
import type { AlbumDto } from "#lib/types.js";

const invokeMock = vi.mocked(invoke);

function album(isFavorite: boolean): AlbumDto {
  return {
    id: "album-1",
    name: "Album",
    artist: "Artist",
    year: null,
    imageTag: null,
    imageBlurHash: null,
    isFavorite,
    trackCount: null,
    artists: [],
  };
}

/** Opens the menu for `target` and returns its favorite entry. */
function favoriteEntry(menu: AlbumMenu, target: AlbumDto) {
  menu.open(new MouseEvent("contextmenu", { cancelable: true, clientX: 10, clientY: 20 }), target);
  const labels: string[] = [m.favorite_add(), m.favorite_remove()];
  const entry = menu.menu?.items.find((item) => labels.includes(item.label));
  if (!entry) throw new Error("no favorite entry in the album menu");
  return entry;
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined as never);
});

describe("createAlbumMenu favorite entry", () => {
  it("flips the album's heart and saves it by default", () => {
    const target = album(false);
    favoriteEntry(createAlbumMenu(), target).action();
    expect(target.isFavorite).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("set_favorite", { itemId: "album-1", favorite: true });
  });

  it("leaves the entry to a page that takes it over", () => {
    const toggleFavorite = vi.fn();
    const target = album(true);
    const entry = favoriteEntry(createAlbumMenu({ toggleFavorite }), target);
    expect(entry.label).toBe(m.favorite_remove());
    entry.action();
    expect(toggleFavorite).toHaveBeenCalledWith(target);
    // The page decides what removing means; the menu neither flips nor saves.
    expect(target.isFavorite).toBe(true);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
