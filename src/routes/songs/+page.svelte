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
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { contextMenuKey } from "$lib/menu";
  import { trackMenuItems } from "$lib/trackMenu";
  import { startDrag } from "$lib/dragToPlaylist";
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

  /** Drop rows the server no longer has, without asking it again. */
  function dropTracks(ids: string[]) {
    const removed = new Set(ids);
    songs = songs.filter((song) => !removed.has(song.id));
    if (total !== null) total = Math.max(0, total - ids.length);
    selected = new Set();
    anchorIndex = -1;
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
    // menu was opened for — never on an emptied or different set. In list
    // order, so what is queued follows the list rather than the clicks.
    const picked = songs.filter((track) => selected.has(track.id));
    menu = {
      x,
      y,
      items: trackMenuItems(picked.length > 1 ? picked : [song], {
        // A song plays in its album's context here, so without an album there
        // is nothing to play it in.
        play: { run: () => void playTrack(song), disabled: !song.albumId },
        addToPlaylist: (tracks) => (addTo = { x, y, trackIds: tracks.map((track) => track.id) }),
        info: (track) =>
          (info = { itemId: track.id, name: track.name, playCount: track.playCount }),
        edit: (track) => (edit = { itemId: track.id, name: track.name }),
        onRemoved: dropTracks,
      }),
    };
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
          draggable="true"
          ondragstart={(event) =>
            startDrag(event, {
              // Dragging a row of the selection takes the whole selection;
              // dragging anything else takes just that row.
              trackIds: selected.has(song.id)
                ? songs.filter((track) => selected.has(track.id)).map((track) => track.id)
                : [song.id],
              label: song.name,
            })}
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
