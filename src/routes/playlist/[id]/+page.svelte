<script lang="ts">
  import { untrack } from "svelte";
  import { page } from "$app/state";
  import { goto, snapshot } from "$app/navigation";
  import { apiLibrary } from "#lib/api/library.js";
  import { errorCause, errorMessage, isNotFound, type AppErrorReport } from "#lib/appErrors.js";
  import { m } from "#lib/paraglide/messages.js";
  import Cover from "#lib/components/Cover.svelte";
  import EmptyState from "#lib/components/EmptyState.svelte";
  import AddToPlaylistMenu from "#lib/components/AddToPlaylistMenu.svelte";
  import ContextMenu, { type ContextMenuItem } from "#lib/components/ContextMenu.svelte";
  import SongInfo from "#lib/components/SongInfo.svelte";
  import MetadataEditor from "#lib/components/MetadataEditor.svelte";
  import PlaylistTrackList from "#lib/components/PlaylistTrackList.svelte";
  import { menuPoint } from "#lib/menu.js";
  import { TRACK_ICON, trackMenuItems } from "#lib/trackMenu.js";
  import PlaylistDuplicatesDialog from "#lib/components/PlaylistDuplicatesDialog.svelte";
  import PlaylistTransferDialog from "#lib/components/PlaylistTransferDialog.svelte";
  import { moveItem } from "#lib/dragReorder.js";
  import type { ListPosition } from "#lib/longList.js";
  import {
    entryRowKeys,
    filterRows,
    firstOccurrences,
    searchTexts,
    sharedEntryIds,
  } from "#lib/playlistEntries.js";
  import { confirm } from "#lib/state/confirm.svelte.js";
  import { library } from "#lib/state/library.svelte.js";
  import { player } from "#lib/state/player.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import { playlists } from "#lib/state/playlists.svelte.js";
  import { portal } from "#lib/portal.js";
  import { swrGet, swrSet, swrDelete } from "#lib/swr.js";
  import type { PlaylistDetail, PlaylistTrackDto } from "#lib/types.js";

  let detail = $state<PlaylistDetail | null>(null);
  let error = $state<string | null>(null);
  let renaming = $state(false);
  let renameValue = $state("");
  let confirmDelete = $state(false);
  let addTo = $state<{ x: number; y: number; trackIds: string[]; trigger?: HTMLElement } | null>(null);
  let menu = $state<{ x: number; y: number; items: ContextMenuItem[] } | null>(null);
  let info = $state<{ itemId: string; name: string; playCount: number } | null>(null);
  let edit = $state<{ itemId: string; name: string } | null>(null);
  let filterQuery = $state("");
  let duplicatePreview = $state(false);
  let duplicatePreviewTracks = $state<PlaylistTrackDto[]>([]);
  let duplicateBusy = $state(false);
  let duplicateError = $state<string | null>(null);
  let duplicateOpen = $state(false);
  let duplicateName = $state("");
  let duplicatePlaylistBusy = $state(false);
  let duplicatePlaylistError = $state<string | null>(null);
  let transferOpen = $state(false);
  /** Bumped by every optimistic mutation so a pre-mutation background fetch
   *  can update the cache without reverting the on-screen change. */
  let gen = 0;

  const playlistId = $derived(page.params.id!);
  const currentTrackId = $derived(player.state.current?.itemId);

  function startRename() {
    if (!detail) return;
    renameValue = detail.playlist.name;
    renaming = true;
  }

  async function commitRename() {
    const name = renameValue.trim();
    renaming = false;
    if (!detail || !name || name === detail.playlist.name) return;
    const id = playlistId;
    gen++;
    detail.playlist.name = name; // optimistic
    try {
      await apiLibrary.renamePlaylist(id, name);
      playlists.refresh();
    } catch (e) {
      // Another playlist is open by now: its page shows its own state.
      if (id !== page.params.id) return;
      error = errorCause(e);
      load(id);
    }
  }

  async function doDelete() {
    confirmDelete = false;
    // The page is reused for the next playlist: the answer is about this
    // one, whichever is open by the time it comes.
    const id = playlistId;
    try {
      await apiLibrary.deletePlaylist(id);
      playlists.refresh();
      swrDelete(`playlist:${id}`);
      toast.show(m.playlist_deleted());
      if (id === page.params.id) goto("/playlists");
    } catch (e) {
      if (id === page.params.id) error = errorCause(e);
    }
  }

  /** A row button's playlist picker; the button's next click closes it again
   *  (a press on it does not count as outside). */
  function openAddTo(e: MouseEvent, trackIds: string[]) {
    e.stopPropagation();
    const trigger = e.currentTarget as HTMLElement;
    addTo = addTo?.trigger === trigger ? null : { ...menuPoint(e), trackIds, trigger };
  }

  function load(id: string) {
    const g = gen;
    apiLibrary
      .getPlaylist(id)
      .then((d) => {
        swrSet(`playlist:${id}`, d);
        // Keep the cache fresh, but don't overwrite an optimistic mutation
        // made while this fetch was in flight.
        if (id === page.params.id && g === gen) detail = d;
      })
      .catch((e) => {
        if (id !== page.params.id) return;
        const msg = errorCause(e);
        if (isNotFound(e)) {
          swrDelete(`playlist:${id}`);
          detail = null;
          error = msg;
        } else if (!detail) {
          error = msg;
        }
      });
  }

  $effect(() => {
    const id = playlistId;
    detail = swrGet<PlaylistDetail>(`playlist:${id}`) ?? null;
    error = null;
    filterQuery = "";
    restoreTo = null;
    duplicatePreview = false;
    duplicateOpen = false;
    transferOpen = false;
    // A prompt, an edit or a menu belongs to the playlist it was opened on: a
    // delete confirmation left open here would otherwise wait, armed, on the
    // next playlist. The draft name goes first — dropping the rename field
    // blurs it, and the blur commits whatever the draft holds.
    confirmDelete = false;
    renameValue = "";
    renaming = false;
    menu = null;
    addTo = null;
    info = null;
    edit = null;
    // `load` snapshots the mutation generation. Keep that synchronous read
    // out of this route-id effect so optimistic edits do not reset the active
    // filter or reopen the cached pre-mutation detail.
    untrack(() => load(id));
  });

  // Nothing to show after a failed load (server down, playlist not cached):
  // try again when the library is poked — the offline banner's Retry and the
  // recovery after an outage do that (net.svelte.ts).
  $effect(() => {
    void library.revision;
    untrack(() => {
      if (!error || detail) return;
      const id = playlistId;
      apiLibrary
        .getPlaylist(id)
        .then((d) => {
          swrSet(`playlist:${id}`, d);
          if (id !== page.params.id || detail) return;
          detail = d;
          error = null;
        })
        .catch(() => {});
    });
  });

  const trackCount = $derived(detail?.tracks.length ?? 0);
  const filterActive = $derived(filterQuery.trim().length > 0);
  // Not the entry id: on Jellyfin 10.11 the two rows of a track that is in the
  // playlist twice share it, and a repeated key would keep the list from
  // rendering at all.
  const rowKeys = $derived(entryRowKeys(detail?.tracks ?? []));
  /** Entry ids several rows share — the server removes all of them at once
   *  and moves only the first (see lib/playlistEntries.ts). */
  const sharedIds = $derived(sharedEntryIds(detail?.tracks ?? []));
  // A move goes by entry id, so while one is shared the server could move
  // another row than the one dragged, or drop it next to the wrong copy.
  const reorderable = $derived(!filterActive && sharedIds.size === 0);
  // Lowercased once per change of the playlist, not on every keystroke.
  const haystacks = $derived(searchTexts(detail?.tracks ?? []));
  const visibleTracks = $derived(
    filterRows(detail?.tracks ?? [], rowKeys, haystacks, filterQuery),
  );
  const duplicateEntryCount = $derived.by(() => {
    const seen = new Set<string>();
    let count = 0;
    for (const entry of detail?.tracks ?? []) {
      if (seen.has(entry.track.id)) count++;
      else seen.add(entry.track.id);
    }
    return count;
  });

  function applyEntryRemoval(entryIds: string[]) {
    if (!detail) return;
    const remove = new Set(entryIds);
    applyTracks(detail.tracks.filter((entry) => !remove.has(entry.entryId)));
  }

  function applyTracks(tracks: PlaylistTrackDto[]) {
    if (!detail) return;
    detail = {
      ...detail,
      playlist: { ...detail.playlist, trackCount: tracks.length },
      tracks,
    };
    swrSet(`playlist:${playlistId}`, detail);
  }

  async function removeEntry(entryId: string) {
    if (!detail) return;
    const id = playlistId;
    // The server removes every entry with this id: say so before it does.
    const copies = sharedIds.get(entryId) ?? 1;
    if (copies > 1) {
      const ok = await confirm.ask({
        title: m.playlist_remove_track(),
        body: m.playlist_remove_repeated({ count: copies }),
        confirmLabel: m.playlist_remove_repeated_confirm({ count: copies }),
        danger: true,
      });
      if (!ok || !detail || id !== page.params.id) return;
    }
    gen++;
    // Optimistic: drop the row now, re-sync from the server on failure.
    applyEntryRemoval([entryId]);
    apiLibrary.playlistRemove(id, [entryId]).catch((e) => {
      if (id === page.params.id) {
        error = errorCause(e);
        load(id);
      }
    });
  }

  function onRowContextMenu(event: MouseEvent, entry: PlaylistTrackDto, index: number) {
    event.preventDefault();
    const { x, y } = menuPoint(event);
    menu = {
      x,
      y,
      items: trackMenuItems([entry.track], {
        // A row plays the playlist from here, which is what clicking it does.
        play: { run: () => player.run(apiLibrary.playPlaylist(playlistId, index)) },
        addToPlaylist: (tracks) => (addTo = { x, y, trackIds: tracks.map((track) => track.id) }),
        info: (track) =>
          (info = { itemId: track.id, name: track.name, playCount: track.playCount }),
        edit: (track) => (edit = { itemId: track.id, name: track.name }),
        // Removal goes by entry id, which is what the server removes by — on
        // Jellyfin 10.11 every copy of a repeated track (removeEntry asks).
        extra: () => [
          {
            label: m.playlist_remove_track(),
            icon: TRACK_ICON.trash,
            action: () => void removeEntry(entry.entryId),
          },
        ],
      }),
    };
  }

  function openDuplicatePreview() {
    if (!detail || duplicateEntryCount === 0) return;
    duplicatePreviewTracks = [...detail.tracks];
    duplicateError = null;
    duplicatePreview = true;
  }

  async function cleanDuplicates() {
    if (!detail || duplicateEntryCount === 0) return;
    const id = playlistId;
    const previous = detail;
    duplicateBusy = true;
    duplicateError = null;
    gen++;
    applyTracks(firstOccurrences(detail.tracks));
    try {
      const { removed, hidden } = await apiLibrary.removePlaylistDuplicates(id);
      if (hidden > 0) {
        // Nothing was changed: the rewrite would have deleted the entries
        // this user cannot see. The dialog stays open and says so.
        if (id === page.params.id) {
          detail = previous;
          swrSet(`playlist:${id}`, previous);
          duplicateError = (
            hidden === 1 ? m.playlist_duplicates_hidden_one : m.playlist_duplicates_hidden_many
          )({ count: hidden });
        }
        return;
      }
      duplicatePreview = false;
      toast.show(
        (removed === 1 ? m.playlist_duplicates_cleaned_one : m.playlist_duplicates_cleaned_many)({
          count: removed,
        }),
      );
      // The server rewrote the playlist; show what it holds now.
      if (id === page.params.id) load(id);
    } catch (e) {
      if (id === page.params.id) {
        detail = previous;
        swrSet(`playlist:${id}`, previous);
        duplicateError = errorMessage(e);
        load(id);
      }
    } finally {
      duplicateBusy = false;
    }
  }

  function openDuplicatePlaylist() {
    if (!detail) return;
    duplicateName = m.playlist_duplicate_default_name({ name: detail.playlist.name });
    duplicatePlaylistError = null;
    duplicateOpen = true;
  }

  async function duplicatePlaylist() {
    const name = duplicateName.trim();
    if (!name || duplicatePlaylistBusy) return;
    duplicatePlaylistBusy = true;
    duplicatePlaylistError = null;
    try {
      const newId = await apiLibrary.duplicatePlaylist(playlistId, name);
      playlists.refresh();
      duplicateOpen = false;
      toast.show(m.playlist_duplicate_done({ name }));
      await goto("/playlist/" + newId);
    } catch (e) {
      duplicatePlaylistError = errorCause(e);
    } finally {
      duplicatePlaylistBusy = false;
    }
  }

  function transferComplete(result: {
    movedEntryIds: string[];
    targetId: string;
    targetName: string;
    moved: boolean;
    added: number | null;
    removeError: AppErrorReport | null;
  }) {
    transferOpen = false;
    swrDelete(`playlist:${result.targetId}`);
    const name = result.targetName;
    if (result.removeError !== null) {
      // Copied, but this playlist could not be changed (it may not be yours)
      // — or only in part: the removal goes a hundred entries at a time, and
      // an answer can be lost after the server acted. Show what it holds now.
      gen++;
      load(playlistId);
      toast.show(m.playlist_transfer_move_partial({ name, message: errorCause(result.removeError) }), {
        kind: "error",
      });
      return;
    }
    if (result.moved) {
      gen++;
      applyEntryRemoval(result.movedEntryIds);
      const count = result.movedEntryIds.length;
      toast.show(
        (count === 1 ? m.playlist_transfer_done_move_one : m.playlist_transfer_done_move_many)({
          count,
          name,
        }),
      );
      return;
    }
    // What the target gained: Jellyfin skips the tracks it already holds.
    const count = result.added ?? result.movedEntryIds.length;
    if (count === 0) {
      const already =
        result.movedEntryIds.length === 1 ? m.playlist_already_has_one : m.playlist_already_has_many;
      toast.show(already({ name }), { kind: "info" });
      return;
    }
    toast.show(
      (count === 1 ? m.playlist_transfer_done_copy_one : m.playlist_transfer_done_copy_many)({
        count,
        name,
      }),
    );
  }

  function onPageKeydown(event: KeyboardEvent) {
    // defaultPrevented: an overlay on top (a confirmation) already handled it.
    if (duplicateOpen && !duplicatePlaylistBusy && !event.defaultPrevented && event.key === "Escape") {
      event.preventDefault();
      duplicateOpen = false;
    }
  }

  /** A row dragged elsewhere (PlaylistTrackList; same pattern as the queue
   *  panel). The server move is remove-then-insert, like `moveItem`. */
  function moveEntry(from: number, to: number) {
    if (!detail) return;
    const id = playlistId;
    const entry = detail.tracks[from];
    if (!entry) return;
    gen++;
    // The cache too: the way back restores a row by its position.
    applyTracks(moveItem(detail.tracks, from, to));
    apiLibrary.playlistMove(id, entry.entryId, to).catch((e) => {
      if (id === page.params.id) {
        error = errorCause(e);
        load(id);
      }
    });
  }

  // Back and Forward come back to where the list was, with the filter it had
  // — in a long list too, which has to be told: it mounts only the rows in
  // view, and has measured none of them when it opens again.
  let trackList = $state<PlaylistTrackList | null>(null);
  let restoreTo = $state<{ id: string; position: ListPosition } | null>(null);
  snapshot<{ id: string; filter: string; position: ListPosition | null }>({
    id: "playlist-page",
    capture: () => ({ id: playlistId, filter: filterQuery, position: trackList?.position() ?? null }),
    restore: (value) => {
      if (value.id !== playlistId) return;
      filterQuery = value.filter;
      if (value.position) restoreTo = { id: value.id, position: value.position };
    },
  });

  // The playlist may still be loading when the way back arrives here.
  $effect(() => {
    const target = restoreTo;
    if (!target || !trackList || !detail) return;
    restoreTo = null;
    if (target.id === playlistId) trackList.restore(target.position);
  });
</script>

<svelte:window onkeydown={onPageKeydown} />

<div>
  {#if error}
    <p class="m-6 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}
  {#if detail}
    <header class="flex items-end gap-6 bg-gradient-to-b from-panel-2 to-panel p-6 pt-10">
      <div class="w-48 shrink-0 shadow-2xl">
        <Cover
          itemId={detail.playlist.id}
          tag={detail.playlist.imageTag}
          size={480}
          alt={detail.playlist.name}
          blurhash={detail.playlist.imageBlurHash}
        />
      </div>
      <div class="min-w-0">
        <p class="text-xs font-bold uppercase tracking-wider">{m.playlist_eyebrow()}</p>
        {#if renaming}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="mt-2 w-full rounded-md border border-edge bg-panel-2 px-2 py-1 text-4xl font-black tracking-tight outline-none focus:border-accent"
            bind:value={renameValue}
            autofocus
            onblur={commitRename}
            onkeydown={(e) => {
              if (e.key === "Enter") commitRename();
              else if (e.key === "Escape") {
                // Restore the name so the blur-triggered commit is a no-op.
                renameValue = detail?.playlist.name ?? "";
                renaming = false;
              }
            }}
          />
        {:else}
          <h1 class="mt-2 truncate text-5xl font-black tracking-tight">{detail.playlist.name}</h1>
        {/if}
        <p class="mt-4 text-sm font-medium text-ink-muted">
          {(trackCount === 1 ? m.playlist_tracks_one : m.playlist_tracks_many)({
            count: trackCount,
          })}
        </p>
      </div>
    </header>

    <div class="flex flex-wrap items-center gap-3 px-6 py-4">
      <button
        class="flex h-13 w-13 items-center justify-center rounded-full bg-accent text-(--color-on-accent) shadow-lg transition-all hover:scale-105 hover:bg-accent-hover"
        onclick={() => player.run(apiLibrary.playPlaylist(playlistId, 0))}
        aria-label={m.album_play()}
        title={m.album_play()}
      >
        <svg viewBox="0 0 24 24" class="h-6 w-6" fill="currentColor" aria-hidden="true">
          <path d="M8 5v14l11-7L8 5z" />
        </svg>
      </button>
      {#if confirmDelete}
        <span class="text-sm text-ink-muted">{m.playlist_delete_confirm({ name: detail.playlist.name })}</span>
        <button
          class="rounded-full bg-red-600 px-4 py-1.5 text-sm font-bold text-white transition-colors hover:bg-red-500"
          onclick={doDelete}
        >
          {m.playlist_delete()}
        </button>
        <button
          class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
          onclick={() => (confirmDelete = false)}
        >
          {m.cancel()}
        </button>
      {:else}
        <button
          class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
          onclick={startRename}
        >
          {m.playlist_rename()}
        </button>
        <button
          class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
          onclick={openDuplicatePlaylist}
        >
          {m.playlist_duplicate_button()}
        </button>
        <button
          class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
          onclick={openDuplicatePreview}
          disabled={duplicateEntryCount === 0}
        >
          {m.playlist_duplicates_button({ count: duplicateEntryCount })}
        </button>
        <button
          class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
          onclick={() => (transferOpen = true)}
          disabled={detail.tracks.length === 0}
        >
          {m.playlist_transfer_button()}
        </button>
        <button
          class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
          onclick={() => (confirmDelete = true)}
        >
          {m.playlist_delete()}
        </button>
      {/if}
    </div>

    <div class="flex flex-wrap items-center gap-3 px-6 pb-3">
      <div class="relative min-w-60 max-w-md flex-1">
        <svg
          viewBox="0 0 24 24"
          class="pointer-events-none absolute top-1/2 left-3 h-4 w-4 -translate-y-1/2 text-ink-muted"
          fill="currentColor"
          aria-hidden="true"
        >
          <path d="M15.5 14h-.79l-.28-.27a6.5 6.5 0 1 0-.7.7l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0A4.5 4.5 0 1 1 14 9.5 4.5 4.5 0 0 1 9.5 14z" />
        </svg>
        <input
          class="w-full rounded-full border border-edge bg-card py-2 pr-10 pl-9 text-sm outline-none transition-colors placeholder:text-ink-muted focus:border-accent"
          bind:value={filterQuery}
          placeholder={m.playlist_filter_placeholder()}
          aria-label={m.playlist_filter_label()}
        />
        {#if filterActive}
          <button
            class="absolute top-1/2 right-2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
            onclick={() => (filterQuery = "")}
            aria-label={m.playlist_filter_clear()}
            title={m.playlist_filter_clear()}
          >
            <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true">
              <path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z" />
            </svg>
          </button>
        {/if}
      </div>
      {#if filterActive}
        <span class="text-xs font-medium text-ink-muted">
          {m.playlist_filter_count({ shown: visibleTracks.length, total: detail.tracks.length })}
        </span>
      {/if}
    </div>
    {#if filterActive}
      <p class="px-6 pb-2 text-xs text-ink-muted">{m.playlist_filter_reorder_disabled()}</p>
    {:else if !reorderable}
      <p class="px-6 pb-2 text-xs text-ink-muted">{m.playlist_duplicates_reorder_disabled()}</p>
    {/if}

    <div class="px-6 pb-6">
      {#if visibleTracks.length === 0 && filterActive}
        <EmptyState
          icon="search"
          title={m.playlist_filter_empty({ term: filterQuery.trim() })}
          action={{ label: m.empty_cta_reset_filters(), onclick: () => (filterQuery = "") }}
          compact
        />
      {:else}
        <PlaylistTrackList
          bind:this={trackList}
          rows={visibleTracks}
          total={trackCount}
          {currentTrackId}
          {reorderable}
          onplay={(index) => player.run(apiLibrary.playPlaylist(playlistId, index))}
          onmenu={onRowContextMenu}
          onaddto={openAddTo}
          onremove={(entryId) => void removeEntry(entryId)}
          onmove={moveEntry}
        />
      {/if}
    </div>
  {:else if !error}
    <p class="p-6 text-sm text-ink-muted">…</p>
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}
{#if info}
  <SongInfo
    itemId={info.itemId}
    name={info.name}
    playCount={info.playCount}
    onclose={() => (info = null)}
  />
{/if}
{#if edit}
  <MetadataEditor
    itemId={edit.itemId}
    displayName={edit.name}
    onclose={() => (edit = null)}
    onsaved={() => {
      edit = null;
      load(playlistId);
    }}
  />
{/if}
{#if addTo}
  <AddToPlaylistMenu
    x={addTo.x}
    y={addTo.y}
    trackIds={addTo.trackIds}
    trigger={addTo.trigger}
    onclose={() => (addTo = null)}
  />
{/if}

{#if duplicatePreview}
  <PlaylistDuplicatesDialog
    tracks={duplicatePreviewTracks}
    busy={duplicateBusy}
    error={duplicateError}
    onclose={() => (duplicatePreview = false)}
    onconfirm={cleanDuplicates}
  />
{/if}

{#if transferOpen && detail}
  <PlaylistTransferDialog
    sourcePlaylistId={playlistId}
    tracks={detail.tracks}
    onclose={() => (transferOpen = false)}
    oncomplete={transferComplete}
  />
{/if}

{#if duplicateOpen && detail}
  <!-- The wrapper stays in place; the overlay moves to <body> (see portal.ts). -->
  <div class="contents">
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    {@attach portal}
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-4"
    onclick={(event) => {
      if (!duplicatePlaylistBusy && event.target === event.currentTarget) duplicateOpen = false;
    }}
  >
    <div
      class="overlay w-full max-w-sm rounded-overlay p-5"
      role="dialog"
      aria-modal="true"
      aria-label={m.playlist_duplicate_title()}
    >
      <form
        onsubmit={(event) => {
          event.preventDefault();
          void duplicatePlaylist();
        }}
      >
        <h2 class="text-md font-bold tracking-tight">{m.playlist_duplicate_title()}</h2>
        <label class="mt-4 block text-xs font-semibold text-ink-muted" for="playlist-duplicate-name">
          {m.playlist_duplicate_name()}
        </label>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          id="playlist-duplicate-name"
          class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm outline-none focus:border-accent"
          bind:value={duplicateName}
          disabled={duplicatePlaylistBusy}
          autofocus
        />
        {#if duplicatePlaylistError}
          <p class="mt-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
            {m.error_generic({ message: duplicatePlaylistError })}
          </p>
        {/if}
        <div class="mt-5 flex justify-end gap-2">
          <button
            type="button"
            class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-40"
            onclick={() => (duplicateOpen = false)}
            disabled={duplicatePlaylistBusy}
          >
            {m.cancel()}
          </button>
          <button
            type="submit"
            class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
            disabled={duplicatePlaylistBusy || !duplicateName.trim()}
          >
            {m.playlist_duplicate_confirm()}
          </button>
        </div>
      </form>
    </div>
  </div>
  </div>
{/if}
