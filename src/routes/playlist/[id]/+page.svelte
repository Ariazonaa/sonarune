<script lang="ts">
  import { untrack } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { formatDuration } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import AddToPlaylistMenu from "$lib/components/AddToPlaylistMenu.svelte";
  import { menuPoint } from "$lib/menu";
  import PlaylistDuplicatesDialog from "$lib/components/PlaylistDuplicatesDialog.svelte";
  import PlaylistTransferDialog from "$lib/components/PlaylistTransferDialog.svelte";
  import { player } from "$lib/state/player.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { listFlip } from "$lib/motion";
  import { portal } from "$lib/portal";
  import { swrGet, swrSet, swrDelete } from "$lib/swr";
  import type { PlaylistDetail, PlaylistTrackDto } from "$lib/types";

  let detail = $state<PlaylistDetail | null>(null);
  let error = $state<string | null>(null);
  let renaming = $state(false);
  let renameValue = $state("");
  let confirmDelete = $state(false);
  let addTo = $state<{ x: number; y: number; trackIds: string[] } | null>(null);
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
    } catch (e) {
      error = String(e);
      load(id);
    }
  }

  async function doDelete() {
    confirmDelete = false;
    try {
      await apiLibrary.deletePlaylist(playlistId);
      swrDelete(`playlist:${playlistId}`);
      toast.show(m.playlist_deleted());
      goto("/playlists");
    } catch (e) {
      error = String(e);
    }
  }

  function openAddTo(e: MouseEvent, trackIds: string[]) {
    e.stopPropagation();
    addTo = { ...menuPoint(e), trackIds };
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
        const msg = String(e);
        if (msg.includes("returned 404")) {
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
    duplicatePreview = false;
    duplicateOpen = false;
    transferOpen = false;
    // `load` snapshots the mutation generation. Keep that synchronous read
    // out of this route-id effect so optimistic edits do not reset the active
    // filter or reopen the cached pre-mutation detail.
    untrack(() => load(id));
  });

  const trackCount = $derived(detail?.tracks.length ?? 0);
  const filterActive = $derived(filterQuery.trim().length > 0);
  const visibleTracks = $derived.by(() => {
    const query = filterQuery.trim().toLocaleLowerCase();
    const indexed = (detail?.tracks ?? []).map((entry, originalIndex) => ({
      entry,
      originalIndex,
    }));
    if (!query) return indexed;
    return indexed.filter(({ entry }) =>
      [entry.track.name, entry.track.artist, entry.track.album].some((value) =>
        value.toLocaleLowerCase().includes(query),
      ),
    );
  });
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
    const tracks = detail.tracks.filter((entry) => !remove.has(entry.entryId));
    detail = {
      ...detail,
      playlist: { ...detail.playlist, trackCount: tracks.length },
      tracks,
    };
    swrSet(`playlist:${playlistId}`, detail);
  }

  function removeEntry(entryId: string) {
    if (!detail) return;
    const id = playlistId;
    gen++;
    // Optimistic: drop the row now, re-sync from the server on failure.
    applyEntryRemoval([entryId]);
    apiLibrary.playlistRemove(id, [entryId]).catch((e) => {
      if (id === page.params.id) {
        error = String(e);
        load(id);
      }
    });
  }

  function openDuplicatePreview() {
    if (!detail || duplicateEntryCount === 0) return;
    duplicatePreviewTracks = [...detail.tracks];
    duplicateError = null;
    duplicatePreview = true;
  }

  async function cleanDuplicates(entryIds: string[]) {
    if (!detail || entryIds.length === 0) return;
    const id = playlistId;
    const previous = detail;
    duplicateBusy = true;
    duplicateError = null;
    gen++;
    applyEntryRemoval(entryIds);
    try {
      await apiLibrary.playlistRemove(id, entryIds);
      duplicatePreview = false;
      toast.show(
        (entryIds.length === 1 ? m.playlist_duplicates_cleaned_one : m.playlist_duplicates_cleaned_many)({
          count: entryIds.length,
        }),
      );
    } catch (e) {
      if (id === page.params.id) {
        detail = previous;
        swrSet(`playlist:${id}`, previous);
        duplicateError = String(e);
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
      duplicateOpen = false;
      toast.show(m.playlist_duplicate_done({ name }));
      await goto("/playlist/" + newId);
    } catch (e) {
      duplicatePlaylistError = String(e);
    } finally {
      duplicatePlaylistBusy = false;
    }
  }

  function transferComplete(result: {
    movedEntryIds: string[];
    targetId: string;
    targetName: string;
    moved: boolean;
  }) {
    transferOpen = false;
    swrDelete(`playlist:${result.targetId}`);
    if (result.moved) {
      gen++;
      applyEntryRemoval(result.movedEntryIds);
    }
    const count = result.movedEntryIds.length;
    const done = result.moved
      ? (count === 1 ? m.playlist_transfer_done_move_one : m.playlist_transfer_done_move_many)
      : (count === 1 ? m.playlist_transfer_done_copy_one : m.playlist_transfer_done_copy_many);
    toast.show(done({ count, name: result.targetName }));
  }

  function onPageKeydown(event: KeyboardEvent) {
    // defaultPrevented: an overlay on top (a confirmation) already handled it.
    if (duplicateOpen && !duplicatePlaylistBusy && !event.defaultPrevented && event.key === "Escape") {
      event.preventDefault();
      duplicateOpen = false;
    }
  }

  // Native HTML5 drag & drop reorder (same pattern as the queue panel).
  // `dropAt` is the slot the dragged row would land in (insert-before
  // semantics; length = append at the end).
  let dragFrom = $state<number | null>(null);
  let dropAt = $state<number | null>(null);

  function onDrop() {
    if (detail && dragFrom !== null && dropAt !== null) {
      // The server move is remove-then-insert: dropping below the origin
      // shifts the target up by one.
      const to = dropAt > dragFrom ? dropAt - 1 : dropAt;
      if (to !== dragFrom) {
        const id = playlistId;
        const entry = detail.tracks[dragFrom];
        const reordered = [...detail.tracks];
        reordered.splice(to, 0, ...reordered.splice(dragFrom, 1));
        gen++;
        detail.tracks = reordered;
        apiLibrary.playlistMove(id, entry.entryId, to).catch((e) => {
          if (id === page.params.id) {
            error = String(e);
            load(id);
          }
        });
      }
    }
    dragFrom = null;
    dropAt = null;
  }

  function onDragOver(event: DragEvent, index: number) {
    event.preventDefault();
    const row = event.currentTarget as HTMLElement;
    const rect = row.getBoundingClientRect();
    dropAt = event.clientY < rect.top + rect.height / 2 ? index : index + 1;
  }
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
        <table class="w-full border-collapse text-sm">
          <tbody ondragleave={() => (dropAt = null)}>
            {#each visibleTracks as visible (visible.entry.entryId)}
              {@const entry = visible.entry}
              {@const originalIndex = visible.originalIndex}
              <tr
                animate:listFlip={{ rows: visibleTracks.length }}
                class="group relative cursor-pointer outline-none transition-colors hover:bg-ink/10 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent
                  {entry.track.id === currentTrackId ? 'text-accent' : ''}
                  {dragFrom === originalIndex ? 'opacity-40' : ''}"
                tabindex="0"
                role="button"
                aria-label={entry.track.name}
                draggable={!filterActive}
                ondragstart={(event) => {
                  if (filterActive) return;
                  dragFrom = originalIndex;
                  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
                }}
                ondragover={(event) => {
                  if (!filterActive) onDragOver(event, originalIndex);
                }}
                ondrop={() => {
                  if (!filterActive) onDrop();
                }}
                ondragend={() => {
                  dragFrom = null;
                  dropAt = null;
                }}
                onclick={() => player.run(apiLibrary.playPlaylist(playlistId, originalIndex))}
                onkeydown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    player.run(apiLibrary.playPlaylist(playlistId, originalIndex));
                  }
                }}
              >
                <td class="w-10 rounded-l-md px-2 py-2 text-right text-ink-muted tabular-nums">
                  {#if dropAt === originalIndex && dragFrom !== null}
                    <div class="pointer-events-none absolute inset-x-1 top-0 h-0.5 rounded bg-accent"></div>
                  {/if}
                  {#if dropAt === originalIndex + 1 && dragFrom !== null}
                    <div class="pointer-events-none absolute inset-x-1 bottom-0 h-0.5 rounded bg-accent"></div>
                  {/if}
                  {originalIndex + 1}
                </td>
                <td class="w-11 py-1.5 pl-1">
                  <div class="w-9">
                    <Cover
                      itemId={entry.track.imageItemId}
                      tag={entry.track.imageTag}
                      size={96}
                      alt=""
                      blurhash={entry.track.imageBlurHash}
                    />
                  </div>
                </td>
                <td class="px-3 py-2">
                  <p class="font-medium {entry.track.id === currentTrackId ? '' : 'text-ink'}">
                    {entry.track.name}
                  </p>
                  <p class="text-xs text-ink-muted">
                    {entry.track.artist}{entry.track.album ? ` · ${entry.track.album}` : ""}
                  </p>
                </td>
                <td class="w-16 px-1 py-2">
                  <span class="invisible flex justify-end gap-0.5 group-hover:visible">
                    <button
                      class="rounded p-1 text-ink-muted hover:text-ink"
                      onclick={(event) => openAddTo(event, [entry.track.id])}
                      aria-haspopup="menu"
                      aria-label={m.add_to_playlist()}
                      title={m.add_to_playlist()}
                    >
                      <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                        <path d="M14 10H3v2h11v-2zm0-4H3v2h11V6zM3 16h7v-2H3v2zm15-4v3h-3v2h3v3h2v-3h3v-2h-3v-3h-2z" />
                      </svg>
                    </button>
                    <button
                      class="rounded p-1 text-ink-muted hover:text-ink"
                      onclick={(event) => {
                        event.stopPropagation();
                        removeEntry(entry.entryId);
                      }}
                      aria-label={m.playlist_remove_track()}
                      title={m.playlist_remove_track()}
                    >
                      <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                        <path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" />
                      </svg>
                    </button>
                  </span>
                </td>
                <td class="w-16 rounded-r-md px-2 py-2 text-right text-ink-muted tabular-nums">
                  {formatDuration(entry.track.durationMs)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  {:else if !error}
    <p class="p-6 text-sm text-ink-muted">…</p>
  {/if}
</div>

{#if addTo}
  <AddToPlaylistMenu
    x={addTo.x}
    y={addTo.y}
    trackIds={addTo.trackIds}
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
