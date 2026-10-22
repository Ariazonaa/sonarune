import { m } from "#lib/paraglide/messages.js";
import type { ToastKind } from "#lib/state/toast.svelte.js";

/**
 * The toast after adding `trackIds` to the playlist `name`. Jellyfin skips the
 * tracks a playlist already holds and still reports success, so this goes by
 * how many the playlist actually gained (`added`, null when the backend could
 * not tell) rather than by how many were sent.
 */
export function playlistAddedToast(
  name: string,
  trackIds: readonly string[],
  added: number | null,
): { message: string; kind: ToastKind } {
  const sent = new Set(trackIds).size;
  if (added === null || added >= sent) {
    return { message: m.add_to_playlist_done({ name }), kind: "success" };
  }
  if (added === 0) {
    const already = sent === 1 ? m.playlist_already_has_one : m.playlist_already_has_many;
    return { message: already({ name }), kind: "info" };
  }
  return { message: m.add_to_playlist_partial({ count: added, total: sent, name }), kind: "success" };
}
