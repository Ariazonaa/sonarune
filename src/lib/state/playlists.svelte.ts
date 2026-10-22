import { apiLibrary } from "#lib/api/library.js";
import type { PlaylistDto } from "#lib/types.js";

/**
 * The user's playlists, for the sidebar.
 *
 * Playlists change because this app changed them, not because the server did,
 * so there is nothing to poll: whoever creates, renames or deletes one calls
 * `refresh()`. A failed load leaves the last list standing — a sidebar that
 * empties itself on a hiccup is worse than one that is briefly out of date.
 */
class PlaylistsStore {
  items = $state<PlaylistDto[]>([]);
  #pending = 0;

  refresh() {
    const seq = ++this.#pending;
    apiLibrary
      .getPlaylists()
      .then((items) => {
        if (seq === this.#pending) this.items = items;
      })
      .catch(() => {});
  }

  /** On sign-out. The list belongs to the account, so here it does not stand
   *  until the next account's first load succeeds; a load still on its way
   *  is dropped. */
  reset() {
    this.#pending++;
    this.items = [];
  }
}

export const playlists = new PlaylistsStore();
