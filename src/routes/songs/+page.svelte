<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { VList } from "virtua/svelte";
  import { goto } from "$app/navigation";
  import { api, formatDuration } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import ContextMenu, { type ContextMenuItem } from "$lib/components/ContextMenu.svelte";
  import AddToPlaylistMenu from "$lib/components/AddToPlaylistMenu.svelte";
  import SongInfo from "$lib/components/SongInfo.svelte";
  import MetadataEditor from "$lib/components/MetadataEditor.svelte";
  import NewContentPill from "$lib/components/NewContentPill.svelte";
  import { apiLibrary } from "$lib/api/library";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { session } from "$lib/state/session.svelte";
  import { confirm } from "$lib/state/confirm.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { downloads } from "$lib/state/downloads.svelte";
  import { contextMenuKey } from "$lib/menu";
  import type { TrackDto } from "$lib/types";

  const PAGE_SIZE = 200;
  const LOAD_AHEAD_PX = 1200;

  let songs = $state<TrackDto[]>([]);
  let total = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let list = $state<VList<TrackDto> | null>(null);
  let staleHint = $state(false);
  /** Bumped on a library refresh so an in-flight page discards itself. */
  let reloadSeq = 0;

  const hasMore = $derived(total === null || songs.length < total);
  const currentTrackId = $derived(player.state.current?.itemId);

  // Multi-select: plain click = single, Ctrl toggles, Shift = range from the
  // last anchor. The Set is reassigned on every change (runes reactivity).
  let selected = $state<Set<string>>(new Set());
  let anchorIndex = -1;
  let menu = $state<{ x: number; y: number; items: ContextMenuItem[] } | null>(null);
  let addTo = $state<{ x: number; y: number; trackIds: string[] } | null>(null);
  let info = $state<{ itemId: string; name: string; playCount: number } | null>(null);
  let edit = $state<{ itemId: string; name: string } | null>(null);

  async function loadMore(): Promise<boolean> {
    if (loading || !hasMore) return false;
    const seq = reloadSeq;
    loading = true;
    error = null;
    try {
      const page = await api.getSongs(songs.length, PAGE_SIZE);
      if (seq !== reloadSeq) return false; // a library refresh superseded this
      // Items can shift between page fetches; duplicate keys would crash the
      // keyed list.
      const seen = new Set(songs.map((s) => s.id));
      songs = [...songs, ...page.items.filter((i) => !seen.has(i.id))];
      total = page.total;
      return true;
    } catch (e) {
      if (seq === reloadSeq) error = String(e);
      return false;
    } finally {
      if (seq === reloadSeq) loading = false;
    }
  }

  function maybeLoadMore() {
    if (!list || loading || !hasMore) return;
    const remaining = list.getScrollSize() - list.getScrollOffset() - list.getViewportSize();
    if (remaining < LOAD_AHEAD_PX) {
      // Only chain on success — a failing server must not retry in a loop.
      loadMore().then((ok) => {
        if (ok) maybeLoadMore();
      });
    }
  }

  /** Play a song in its album context (Spotify behavior). */
  async function playTrack(track: TrackDto) {
    if (!track.albumId) return;
    try {
      const detail = await api.getAlbum(track.albumId);
      const index = detail.tracks.findIndex((candidate) => candidate.id === track.id);
      await api.playAlbum(track.albumId, Math.max(0, index));
    } catch (e) {
      player.error = String(e);
    }
  }

  function onRowClick(event: MouseEvent, song: TrackDto, index: number) {
    if (event.shiftKey && anchorIndex >= 0) {
      const [from, to] = [Math.min(anchorIndex, index), Math.max(anchorIndex, index)];
      selected = new Set(songs.slice(from, to + 1).map((s) => s.id));
    } else if (event.ctrlKey || event.metaKey) {
      const next = new Set(selected);
      if (next.has(song.id)) next.delete(song.id);
      else next.add(song.id);
      selected = next;
      anchorIndex = index;
    } else {
      selected = new Set([song.id]);
      anchorIndex = index;
    }
  }

  /** Selected ids in list order (not click order) — keeps queue order sane. */
  function selectedIds(): string[] {
    return songs.filter((s) => selected.has(s.id)).map((s) => s.id);
  }

  const icons = {
    play: "M8 5v14l11-7L8 5z",
    playNext: "M3 10h11v2H3v-2zm0-4h11v2H3V6zm0 8h7v2H3v-2zm13-1v8l6-4-6-4z",
    addQueue: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
    addPlaylist: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
    mix: "M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.03 3.03L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z",
    album: "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 14.5c-2.49 0-4.5-2.01-4.5-4.5S9.51 7.5 12 7.5s4.5 2.01 4.5 4.5-2.01 4.5-4.5 4.5zm0-5.5c-.55 0-1 .45-1 1s.45 1 1 1 1-.45 1-1-.45-1-1-1z",
    person: "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
    info: "M11 7h2v2h-2V7zm0 4h2v6h-2v-6zm1-9C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8z",
    download: "M5 20h14v-2H5v2zM19 9h-4V3H9v6H5l7 7 7-7z",
    edit: "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z",
    trash: "M6 7h12l-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 7zm3-3h6l1 2H8l1-2z",
  };

  async function downloadTracks(tracks: TrackDto[]) {
    if (tracks.length === 0) return;
    try {
      await downloads.enqueue(tracks.map((track) => ({ itemId: track.id, name: track.name })));
      toast.show(
        tracks.length === 1
          ? m.download_queued_one()
          : m.download_queued_many({ count: tracks.length }),
        { kind: "info" },
      );
    } catch (e) {
      player.error = String(e);
    }
  }

  /** "Go to artist" context-menu entries — one per linkable artist. */
  function artistEntries(track: TrackDto): ContextMenuItem[] {
    return track.artists.map((a) => ({
      label: track.artists.length === 1 ? m.ctx_go_to_artist() : `${m.ctx_go_to_artist()}: ${a.name}`,
      icon: icons.person,
      action: () => goto(`/artist/${a.id}`),
    }));
  }

  /** Permanently delete the given tracks from the server, after confirming. */
  async function deleteTracks(ids: string[]) {
    if (ids.length === 0) return;
    const first = songs.find((s) => s.id === ids[0]);
    const ok = await confirm.ask({
      title: m.delete_confirm_title(),
      body:
        ids.length === 1
          ? m.delete_track_confirm({ name: first?.name ?? "" })
          : m.delete_tracks_confirm({ count: ids.length }),
      confirmLabel: m.delete_action(),
      danger: true,
    });
    if (!ok) return;
    try {
      await apiLibrary.deleteItems(ids);
      const removed = new Set(ids);
      songs = songs.filter((s) => !removed.has(s.id));
      if (total !== null) total = Math.max(0, total - ids.length);
      selected = new Set();
      anchorIndex = -1;
      toast.show(m.delete_done());
    } catch (e) {
      player.error = String(e);
    }
  }

  function onRowContextMenu(event: MouseEvent, song: TrackDto, index: number) {
    event.preventDefault();
    // Right-click outside the current selection re-anchors it to that row.
    if (!selected.has(song.id)) {
      selected = new Set([song.id]);
      anchorIndex = index;
    }
    const x = event.clientX;
    const y = event.clientY;
    // Snapshot the selection as the menu opens: a library refresh while it is
    // open clears `selected`, and an action must still act on the rows the
    // menu was opened for — never on an emptied or different set.
    const ids = selectedIds();
    const picked = songs.filter((track) => selected.has(track.id));
    const items: ContextMenuItem[] =
      ids.length > 1
        ? [
            {
              label: m.album_play_next(),
              icon: icons.playNext,
              action: () => player.run(api.enqueueTracks(ids, true)),
            },
            {
              label: m.album_add_to_queue(),
              icon: icons.addQueue,
              action: () => player.run(api.enqueueTracks(ids, false)),
            },
            {
              label: m.add_to_playlist(),
              icon: icons.addPlaylist,
              action: () => (addTo = { x, y, trackIds: ids }),
            },
            {
              label: m.ctx_download(),
              icon: icons.download,
              action: () => downloadTracks(picked),
            },
            ...(session.info?.canDelete
              ? [
                  {
                    label: m.delete_from_server(),
                    icon: icons.trash,
                    danger: true,
                    action: () => deleteTracks(ids),
                  },
                ]
              : []),
          ]
        : [
            {
              label: m.album_play(),
              icon: icons.play,
              disabled: !song.albumId,
              action: () => playTrack(song),
            },
            {
              label: m.album_play_next(),
              icon: icons.playNext,
              action: () => player.run(api.enqueueTracks([song.id], true)),
            },
            {
              label: m.album_add_to_queue(),
              icon: icons.addQueue,
              action: () => player.run(api.enqueueTracks([song.id], false)),
            },
            {
              label: m.add_to_playlist(),
              icon: icons.addPlaylist,
              action: () => (addTo = { x, y, trackIds: [song.id] }),
            },
            {
              label: m.ctx_instant_mix(),
              icon: icons.mix,
              action: () => player.run(api.playInstantMix(song.id)),
            },
            {
              label: m.ctx_go_to_album(),
              icon: icons.album,
              disabled: !song.albumId,
              action: () => goto(`/album/${song.albumId}`),
            },
            ...artistEntries(song),
            {
              label: m.ctx_info(),
              icon: icons.info,
              action: () => (info = { itemId: song.id, name: song.name, playCount: song.playCount }),
            },
            ...(session.info?.canEdit
              ? [
                  {
                    label: m.ctx_edit(),
                    icon: icons.edit,
                    action: () => (edit = { itemId: song.id, name: song.name }),
                  },
                ]
              : []),
            {
              label: m.ctx_download(),
              icon: icons.download,
              action: () => downloadTracks([song]),
            },
            ...(session.info?.canDelete
              ? [
                  {
                    label: m.delete_from_server(),
                    icon: icons.trash,
                    danger: true,
                    action: () => deleteTracks([song.id]),
                  },
                ]
              : []),
          ];
    menu = { x: event.clientX, y: event.clientY, items };
  }

  onMount(() => {
    loadMore().then(() => maybeLoadMore());
  });

  function refresh() {
    staleHint = false;
    reloadSeq++;
    songs = [];
    total = null;
    loading = false;
    selected = new Set();
    anchorIndex = -1; // the replaced list invalidates the shift-click anchor
    list?.scrollTo(0);
    loadMore().then((ok) => ok && maybeLoadMore());
  }

  // Reload live near the top, else offer a refresh pill (a deep scroll +
  // selection shouldn't be yanked away). Only `library.revision` is tracked.
  $effect(() => {
    const rev = library.revision;
    untrack(() => {
      if (rev === 0) return;
      if (!list || list.getScrollOffset() < 400) refresh();
      else staleHint = true;
    });
  });
</script>

<div class="flex h-full min-h-0 flex-col p-6 pb-0">
  <div class="mb-5 flex shrink-0 items-baseline gap-3">
    <h1 class="text-2xl font-bold tracking-tight">{m.nav_songs()}</h1>
    {#if total !== null}
      <span class="text-sm font-medium text-ink-muted">{(total === 1 ? m.songs_total_one : m.songs_total_many)({ count: total })}</span>
    {/if}
  </div>

  {#if error}
    <p class="mb-4 shrink-0 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}

  <div class="min-h-0 flex-1">
    <VList
      bind:this={list}
      data={songs}
      getKey={(song) => song.id}
      style="height: 100%;"
      onscroll={maybeLoadMore}
    >
      {#snippet children(song, index)}
        <button
          data-list-row
          class="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left transition-colors
            {selected.has(song.id) ? 'bg-ink/10 ring-1 ring-ink/20 ring-inset' : 'hover:bg-ink/10'}
            {song.id === currentTrackId ? 'text-accent' : ''}"
          onclick={(e) => onRowClick(e, song, index)}
          ondblclick={() => playTrack(song)}
          oncontextmenu={(e) => onRowContextMenu(e, song, index)}
          {@attach contextMenuKey}
        >
          <span class="w-10 shrink-0 text-right text-xs text-ink-muted tabular-nums">
            {index + 1}
          </span>
          <div class="w-10 shrink-0">
            <Cover itemId={song.imageItemId} tag={song.imageTag} size={96} alt="" blurhash={song.imageBlurHash} />
          </div>
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">{song.name}</p>
            <p class="truncate text-xs text-ink-muted">{song.artist}</p>
          </div>
          <p class="hidden w-1/4 shrink-0 truncate text-xs text-ink-muted md:block">
            {song.album}
          </p>
          <span class="min-w-12 shrink-0 text-right text-xs text-ink-muted tabular-nums">
            {formatDuration(song.durationMs)}
          </span>
        </button>
      {/snippet}
    </VList>
  </div>
</div>

{#if staleHint}
  <NewContentPill onrefresh={refresh} />
{/if}
{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}
{#if addTo}
  <AddToPlaylistMenu x={addTo.x} y={addTo.y} trackIds={addTo.trackIds} onclose={() => (addTo = null)} />
{/if}
{#if info}
  <SongInfo itemId={info.itemId} name={info.name} playCount={info.playCount} onclose={() => (info = null)} />
{/if}
{#if edit}
  <MetadataEditor itemId={edit.itemId} displayName={edit.name} onclose={() => (edit = null)} onsaved={refresh} />
{/if}
