import { api } from "#lib/api.js";

/**
 * Play a track the way clicking it in a track list does: inside its album,
 * starting at the track, so the album plays on from there.
 *
 * Without a usable album — none at all (a loose file without an album tag),
 * or one that no longer lists the track (retagged or moved since the list
 * was loaded) — the track plays on its own. Appending it to the queue
 * instead would leave a click that seems to do nothing. The album is looked
 * up in Rust, after the play took its place in line: the newest click wins.
 */
export function playInAlbum(trackId: string, albumId: string | null): Promise<void> {
  return api.playTrackInAlbum(trackId, albumId);
}
