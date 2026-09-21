import { goto } from "$app/navigation";
import { api } from "$lib/api";
import { apiLibrary } from "$lib/api/library";
import { m } from "$lib/paraglide/messages";
import { confirm } from "$lib/state/confirm.svelte";
import { downloads } from "$lib/state/downloads.svelte";
import { player } from "$lib/state/player.svelte";
import { session } from "$lib/state/session.svelte";
import { toast } from "$lib/state/toast.svelte";
import type { ContextMenuItem } from "$lib/components/ContextMenu.svelte";
import type { TrackDto } from "$lib/types";

export const TRACK_ICON = {
  play: "M8 5v14l11-7L8 5z",
  playNext: "M3 10h11v2H3v-2zm0-4h11v2H3V6zm0 8h7v2H3v-2zm13-1v8l6-4-6-4z",
  addQueue: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
  addPlaylist: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
  mix: "M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.03 3.03L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z",
  album:
    "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 14.5c-2.49 0-4.5-2.01-4.5-4.5S9.51 7.5 12 7.5s4.5 2.01 4.5 4.5-2.01 4.5-4.5 4.5zm0-5.5c-.55 0-1 .45-1 1s.45 1 1 1 1-.45 1-1-.45-1-1-1z",
  person: "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
  info: "M11 7h2v2h-2V7zm0 4h2v6h-2v-6zm1-9C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8z",
  download: "M5 20h14v-2H5v2zM19 9h-4V3H9v6H5l7 7 7-7z",
  edit: "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z",
  trash: "M6 7h12l-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 7zm3-3h6l1 2H8l1-2z",
  heart:
    "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
};

/** Queue the given tracks for download and say so. Identical everywhere a
 *  track can be right-clicked, which is why it lives here. */
export async function downloadTracks(tracks: TrackDto[]) {
  if (tracks.length === 0) return;
  try {
    await downloads.enqueue(tracks.map((track) => ({ itemId: track.id, name: track.name })));
    toast.show(tracks.length === 1 ? m.download_queued_one() : m.download_queued_many({ count: tracks.length }));
  } catch (e) {
    player.error = String(e);
  }
}

/**
 * Ask, then delete the tracks from the server for good. `onRemoved` gets the
 * ids so the list they came from can drop them — the server is not asked
 * again, and a row that is gone must not sit there looking playable.
 */
export async function deleteTracks(tracks: TrackDto[], onRemoved: (ids: string[]) => void) {
  const ids = tracks.map((track) => track.id);
  if (ids.length === 0) return;
  const ok = await confirm.ask({
    title: m.delete_confirm_title(),
    body:
      ids.length === 1
        ? m.delete_track_confirm({ name: tracks[0].name })
        : m.delete_tracks_confirm({ count: ids.length }),
    confirmLabel: m.delete_action(),
    danger: true,
  });
  if (!ok) return;
  try {
    await apiLibrary.deleteItems(ids);
    onRemoved(ids);
    toast.show(m.delete_done());
  } catch (e) {
    player.error = String(e);
  }
}

/** One entry per artist; the name is only spelled out when there are several. */
function artistEntries(track: TrackDto): ContextMenuItem[] {
  return track.artists.map((artist) => ({
    label: track.artists.length === 1 ? m.ctx_go_to_artist() : `${m.ctx_go_to_artist()}: ${artist.name}`,
    icon: TRACK_ICON.person,
    action: () => goto(`/artist/${artist.id}`),
  }));
}

export interface TrackMenuHooks {
  /**
   * Play the track in the context the list stands for — its album, the
   * playlist, the search result. Left out where there is none: a "play" that
   * quietly queues a single track and nothing else is a lie about what the
   * list does. `disabled` keeps the entry visible but inert where the context
   * is missing for this one row (a track without an album), which says more
   * than an entry that is simply not there.
   */
  play?: { run: () => void; disabled?: boolean };
  /** Open the playlist picker for these tracks (the caller knows where). */
  addToPlaylist: (tracks: TrackDto[]) => void;
  /** Open the info panel for one track. */
  info?: (track: TrackDto) => void;
  /** Open the metadata editor. Only offered when the account may edit. */
  edit?: (track: TrackDto) => void;
  /** Drop the ids from the list after a server-side delete. Without it the
   *  delete entry is not offered — a deletion the list does not notice looks
   *  like a failure. */
  onRemoved?: (ids: string[]) => void;
  /** Entries only this list has, such as "remove from this playlist". They sit
   *  after the shared ones and before the destructive ones. */
  extra?: (tracks: TrackDto[]) => ContextMenuItem[];
}

/**
 * The right-click menu of a track row, shared by every list that has one.
 *
 * `tracks` is the selection the menu was opened for, in list order: one row,
 * or the rows a multi-select covers. A selection of several gets the entries
 * that make sense in bulk and nothing that names a single track.
 */
export function trackMenuItems(tracks: TrackDto[], hooks: TrackMenuHooks): ContextMenuItem[] {
  if (tracks.length === 0) return [];
  const ids = tracks.map((track) => track.id);
  const many = tracks.length > 1;
  const track = tracks[0];

  const items: ContextMenuItem[] = [];
  if (!many && hooks.play) {
    const play = hooks.play;
    items.push({
      label: m.album_play(),
      icon: TRACK_ICON.play,
      disabled: play.disabled,
      action: () => play.run(),
    });
  }
  items.push(
    {
      label: m.album_play_next(),
      icon: TRACK_ICON.playNext,
      action: () => player.run(api.enqueueTracks(ids, true)),
    },
    {
      label: m.album_add_to_queue(),
      icon: TRACK_ICON.addQueue,
      action: () => player.run(api.enqueueTracks(ids, false)),
    },
    {
      label: m.add_to_playlist(),
      icon: TRACK_ICON.addPlaylist,
      action: () => hooks.addToPlaylist(tracks),
    },
  );

  if (!many) {
    items.push({
      label: m.ctx_instant_mix(),
      icon: TRACK_ICON.mix,
      action: () => player.run(api.playInstantMix(track.id)),
    });
    items.push({
      label: m.ctx_go_to_album(),
      icon: TRACK_ICON.album,
      disabled: !track.albumId,
      action: () => goto(`/album/${track.albumId}`),
    });
    items.push(...artistEntries(track));
    if (hooks.info) {
      items.push({
        label: m.ctx_info(),
        icon: TRACK_ICON.info,
        action: () => hooks.info?.(track),
      });
    }
    if (hooks.edit && session.info?.canEdit) {
      items.push({
        label: m.ctx_edit(),
        icon: TRACK_ICON.edit,
        action: () => hooks.edit?.(track),
      });
    }
  }

  items.push(...(hooks.extra?.(tracks) ?? []));
  items.push({
    label: m.ctx_download(),
    icon: TRACK_ICON.download,
    action: () => void downloadTracks(tracks),
  });
  if (hooks.onRemoved && session.info?.canDelete) {
    const onRemoved = hooks.onRemoved;
    items.push({
      label: m.delete_from_server(),
      icon: TRACK_ICON.trash,
      danger: true,
      action: () => void deleteTracks(tracks, onRemoved),
    });
  }
  return items;
}
