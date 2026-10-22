import { invoke } from "@tauri-apps/api/core";
import type {
  AlbumDto,
  ArtistDto,
  BulkMetadataEdits,
  BulkMetadataResult,
  EditableMetadata,
  FavoritesData,
  HistoryEntry,
  HistoryYears,
  HomeData,
  MetadataEdits,
  Page,
  PlaylistCleanup,
  PlaylistDetail,
  PlaylistDto,
  PlaylistTransfer,
  StatsData,
  TrackDto,
  YearReview,
} from "#lib/types.js";

/** Library-slice commands (src-tauri/src/commands_library.rs). */
export const apiLibrary = {
  getPlaylists: () => invoke<PlaylistDto[]>("get_playlists"),
  getPlaylist: (playlistId: string) => invoke<PlaylistDetail>("get_playlist", { playlistId }),
  createPlaylist: (name: string, trackIds: string[]) =>
    invoke<string>("create_playlist", { name, trackIds }),
  deletePlaylist: (playlistId: string) => invoke<void>("delete_playlist", { playlistId }),
  renamePlaylist: (playlistId: string, name: string) =>
    invoke<void>("rename_playlist", { playlistId, name }),
  /** Resolves with how many tracks the playlist gained (null: unknown) —
   *  Jellyfin skips the ones it already holds. */
  playlistAdd: (playlistId: string, trackIds: string[]) =>
    invoke<number | null>("playlist_add", { playlistId, trackIds }),
  /** `entryIds` are playlist entry ids (PlaylistTrackDto.entryId), not item ids. */
  playlistRemove: (playlistId: string, entryIds: string[]) =>
    invoke<void>("playlist_remove", { playlistId, entryIds }),
  playlistMove: (playlistId: string, entryId: string, newIndex: number) =>
    invoke<void>("playlist_move", { playlistId, entryId, newIndex }),
  /** Keep the first entry of every track, drop the later ones — unless the
   *  playlist holds entries the user cannot see (`hidden`), which the rewrite
   *  would delete; then nothing is changed. */
  removePlaylistDuplicates: (playlistId: string) =>
    invoke<PlaylistCleanup>("remove_playlist_duplicates", { playlistId }),
  duplicatePlaylist: (playlistId: string, name: string) =>
    invoke<string>("duplicate_playlist", { playlistId, name }),
  transferPlaylistEntries: (
    sourcePlaylistId: string,
    targetPlaylistId: string,
    entryIds: string[],
    moveEntries: boolean,
  ) =>
    invoke<PlaylistTransfer>("transfer_playlist_entries", {
      sourcePlaylistId,
      targetPlaylistId,
      entryIds,
      moveEntries,
    }),
  playPlaylist: (playlistId: string, startIndex = 0) =>
    invoke<void>("play_playlist", { playlistId, startIndex }),
  setFavorite: (itemId: string, favorite: boolean) =>
    invoke<void>("set_favorite", { itemId, favorite }),
  /** Permanently delete items (tracks or whole albums) on the server. */
  deleteItems: (itemIds: string[]) => invoke<void>("delete_items", { itemIds }),
  /** Current metadata of an item, for the editor form (admin-only server-side). */
  getItemMetadata: (itemId: string) =>
    invoke<EditableMetadata>("get_item_metadata", { itemId }),
  /** Write the whitelisted metadata edits back to the server (admin-only). */
  updateItemMetadata: (itemId: string, edits: MetadataEdits) =>
    invoke<void>("update_item_metadata", { itemId, edits }),
  /** The same for many tracks, with only the named fields written. Resolves
   *  with how many went through; a single refusal does not stop the rest. */
  updateItemsMetadata: (itemIds: string[], edits: BulkMetadataEdits) =>
    invoke<BulkMetadataResult>("update_items_metadata", { itemIds, edits }),
  getHome: () => invoke<HomeData>("get_home"),
  getStats: () => invoke<StatsData>("get_stats"),
  /** The local play history, newest first: `limit` listens older than the
   *  row `before` (from the top when null). */
  historyPage: (before: number | null, limit: number) =>
    invoke<HistoryEntry[]>("history_page", { before, limit }),
  historyYears: () => invoke<HistoryYears>("history_years"),
  historyYear: (year: number) => invoke<YearReview>("history_year", { year }),
  historyDelete: (id: number) => invoke<boolean>("history_delete", { id }),
  /** Forget the history of the signed-in account; resolves with the rows gone. */
  historyClear: () => invoke<number>("history_clear"),
  /** First page of every favorites section, each with its total. */
  getFavorites: () => invoke<FavoritesData>("get_favorites"),
  /** Later pages of one favorites section (by name; limit 1–200). */
  getFavoriteTracks: (startIndex: number, limit: number) =>
    invoke<Page<TrackDto>>("get_favorite_tracks", { startIndex, limit }),
  getFavoriteAlbums: (startIndex: number, limit: number) =>
    invoke<Page<AlbumDto>>("get_favorite_albums", { startIndex, limit }),
  getFavoriteArtists: (startIndex: number, limit: number) =>
    invoke<Page<ArtistDto>>("get_favorite_artists", { startIndex, limit }),
};
