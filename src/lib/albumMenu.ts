import { api } from "$lib/api";
import { m } from "$lib/paraglide/messages";
import { player } from "$lib/state/player.svelte";
import type { ContextMenuItem } from "$lib/components/ContextMenu.svelte";
import type { AlbumDto } from "$lib/types";

const ICON = {
  play: "M8 5v14l11-7L8 5z",
  playNext: "M3 10h11v2H3v-2zm0-4h11v2H3V6zm0 8h7v2H3v-2zm13-1v8l6-4-6-4z",
  addQueue: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
  addPlaylist: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
  heart:
    "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
};

/**
 * Right-click menu items for an album card, shared by every album grid.
 * `onAddToPlaylist` opens the playlist picker (the caller fetches the album's
 * track ids); `onFavorite` toggles the favorite (the caller mutates local
 * state so the heart updates immediately).
 */
export function albumMenuItems(
  album: AlbumDto,
  onAddToPlaylist: () => void,
  onFavorite: () => void,
): ContextMenuItem[] {
  return [
    { label: m.album_play(), icon: ICON.play, action: () => player.run(api.playAlbum(album.id, 0)) },
    {
      label: m.album_play_next(),
      icon: ICON.playNext,
      action: () => player.run(api.enqueueAlbum(album.id, true)),
    },
    {
      label: m.album_add_to_queue(),
      icon: ICON.addQueue,
      action: () => player.run(api.enqueueAlbum(album.id, false)),
    },
    { label: m.add_to_playlist(), icon: ICON.addPlaylist, action: onAddToPlaylist },
    {
      label: album.isFavorite ? m.favorite_remove() : m.favorite_add(),
      icon: ICON.heart,
      action: onFavorite,
    },
  ];
}
