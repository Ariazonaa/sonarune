import { api } from "$lib/api";
import { apiLibrary } from "$lib/api/library";
import { player } from "$lib/state/player.svelte";
import { albumMenuItems } from "$lib/albumMenu";
import type { ContextMenuItem } from "$lib/components/ContextMenu.svelte";
import type { AlbumDto } from "$lib/types";

type MenuState = { x: number; y: number; items: ContextMenuItem[] } | null;
type AddToState = { x: number; y: number; trackIds: string[] } | null;

/**
 * Per-page holder for an album card's right-click menu + its "add to playlist"
 * picker. Create one in a component (`const am = createAlbumMenu()`), wire
 * `oncontextmenu={(e) => am.open(e, album)}` on cards, and render
 * `<AlbumContextMenu menu={am} />` once.
 */
export function createAlbumMenu() {
  let menu = $state<MenuState>(null);
  let addTo = $state<AddToState>(null);

  function open(e: MouseEvent, album: AlbumDto) {
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    menu = {
      x,
      y,
      items: albumMenuItems(
        album,
        async () => {
          try {
            const d = await api.getAlbum(album.id);
            addTo = { x, y, trackIds: d.tracks.map((t) => t.id) };
          } catch (err) {
            player.error = String(err);
          }
        },
        () => {
          const next = !album.isFavorite;
          album.isFavorite = next; // optimistic; the DTO in the grid is reactive
          apiLibrary.setFavorite(album.id, next).catch(() => (album.isFavorite = !next));
        },
      ),
    };
  }

  return {
    get menu() {
      return menu;
    },
    set menu(v: MenuState) {
      menu = v;
    },
    get addTo() {
      return addTo;
    },
    set addTo(v: AddToState) {
      addTo = v;
    },
    open,
  };
}

export type AlbumMenu = ReturnType<typeof createAlbumMenu>;
