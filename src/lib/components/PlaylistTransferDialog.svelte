<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import { portal } from "$lib/portal";
  import type { PlaylistDto, PlaylistTrackDto } from "$lib/types";

  let {
    sourcePlaylistId,
    tracks,
    onclose,
    oncomplete,
  }: {
    sourcePlaylistId: string;
    tracks: PlaylistTrackDto[];
    onclose: () => void;
    oncomplete: (args: {
      movedEntryIds: string[];
      targetId: string;
      targetName: string;
      moved: boolean;
    }) => void;
  } = $props();

  let dialog = $state<HTMLElement | null>(null);
  let searchInput = $state<HTMLInputElement | null>(null);
  let targets = $state<PlaylistDto[] | null>(null);
  let targetId = $state("");
  let query = $state("");
  let selectedEntryIds = $state<Set<string>>(new Set());
  let moveEntries = $state(false);
  let loadingTargets = $state(true);
  let busy = $state(false);
  let error = $state<string | null>(null);

  let closing = false;
  let targetRequest = 0;
  let transferRequest = 0;
  let previousFocus: HTMLElement | null = null;
  let observedSource: string | null = null;

  const normalizedQuery = $derived(query.trim().toLocaleLowerCase());
  const visibleTracks = $derived.by(() => {
    if (!normalizedQuery) return tracks;
    return tracks.filter(({ track }) =>
      [track.name, track.artist, track.album].some((value) =>
        value.toLocaleLowerCase().includes(normalizedQuery),
      ),
    );
  });
  const selectedCount = $derived(selectedEntryIds.size);
  const canSubmit = $derived(
    !busy && !loadingTargets && targetId.length > 0 && selectedCount > 0,
  );

  $effect(() => {
    const source = sourcePlaylistId;
    const request = ++targetRequest;

    if (observedSource !== null && source !== observedSource) {
      transferRequest++;
      query = "";
      selectedEntryIds = new Set();
      moveEntries = false;
      busy = false;
    }
    observedSource = source;

    targets = null;
    targetId = "";
    loadingTargets = true;
    error = null;

    apiLibrary
      .getPlaylists()
      .then((playlists) => {
        if (closing || request !== targetRequest || source !== sourcePlaylistId) return;
        const available = playlists.filter((playlist) => playlist.id !== source);
        targets = available;
        targetId = available[0]?.id ?? "";
      })
      .catch((cause) => {
        if (closing || request !== targetRequest || source !== sourcePlaylistId) return;
        targets = [];
        error = String(cause);
      })
      .finally(() => {
        if (!closing && request === targetRequest && source === sourcePlaylistId) {
          loadingTargets = false;
        }
      });
  });

  // Drop selections that no longer exist if the parent refreshes the source
  // playlist while the dialog is open.
  $effect(() => {
    const valid = new Set(tracks.map((entry) => entry.entryId));
    if ([...selectedEntryIds].some((entryId) => !valid.has(entryId))) {
      selectedEntryIds = new Set([...selectedEntryIds].filter((entryId) => valid.has(entryId)));
    }
  });

  onMount(() => {
    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    searchInput?.focus();
  });

  onDestroy(() => {
    closing = true;
    targetRequest++;
    transferRequest++;
    previousFocus?.focus();
  });

  function requestClose() {
    if (busy || closing) return;
    closing = true;
    targetRequest++;
    transferRequest++;
    onclose();
  }

  function toggleEntry(entryId: string) {
    if (busy) return;
    const next = new Set(selectedEntryIds);
    if (next.has(entryId)) next.delete(entryId);
    else next.add(entryId);
    selectedEntryIds = next;
  }

  function selectVisible() {
    if (busy) return;
    const next = new Set(selectedEntryIds);
    for (const entry of visibleTracks) next.add(entry.entryId);
    selectedEntryIds = next;
  }

  function clearVisible() {
    if (busy) return;
    const next = new Set(selectedEntryIds);
    for (const entry of visibleTracks) next.delete(entry.entryId);
    selectedEntryIds = next;
  }

  function focusableElements(): HTMLElement[] {
    if (!dialog) return [];
    return Array.from(
      dialog.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((element) => !element.hasAttribute("hidden"));
  }

  function onKeydown(event: KeyboardEvent) {
    // A key an overlay on top already handled (a confirmation's Escape).
    if (event.defaultPrevented) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      if (!busy) {
        requestClose();
      }
      return;
    }
    if (event.key !== "Tab" || !dialog) return;

    const focusable = focusableElements();
    if (focusable.length === 0) {
      event.preventDefault();
      dialog.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (
      event.shiftKey &&
      (document.activeElement === first || !dialog.contains(document.activeElement))
    ) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  async function submit() {
    if (!canSubmit || closing) return;
    const target = targets?.find((playlist) => playlist.id === targetId);
    if (!target) return;

    // Filter the source array instead of iterating the Set so duplicate tracks
    // remain independently addressable and their original order is preserved.
    const entryIds = tracks
      .filter((entry) => selectedEntryIds.has(entry.entryId))
      .map((entry) => entry.entryId);
    if (entryIds.length === 0) return;

    const source = sourcePlaylistId;
    const destination = target.id;
    const targetName = target.name;
    const moved = moveEntries;
    const request = ++transferRequest;
    busy = true;
    error = null;

    try {
      await apiLibrary.transferPlaylistEntries(source, destination, entryIds, moved);
    } catch (cause) {
      if (!closing && request === transferRequest && source === sourcePlaylistId) {
        error = String(cause);
        busy = false;
      }
      return;
    }

    if (closing || request !== transferRequest || source !== sourcePlaylistId) return;
    busy = false;
    closing = true;
    oncomplete({ movedEntryIds: entryIds, targetId: destination, targetName, moved });
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- The wrapper stays in place; the overlay moves to <body> (see portal.ts). -->
<div class="contents">
<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div
  {@attach portal}
  class="fixed inset-0 z-50 flex items-center justify-center bg-base/80 p-4"
  onclick={(event) => {
    if (event.target === event.currentTarget && !busy) requestClose();
  }}
>
  <div
    bind:this={dialog}
    class="overlay flex max-h-[min(46rem,90vh)] w-full max-w-2xl flex-col overflow-hidden rounded-overlay"
    role="dialog"
    aria-modal="true"
    aria-labelledby="playlist-transfer-title"
    aria-busy={busy}
    tabindex="-1"
  >
    <header class="flex items-center justify-between gap-3 border-b border-edge px-5 py-4">
      <h2 id="playlist-transfer-title" class="text-md font-bold tracking-tight">
        {m.playlist_transfer_title()}
      </h2>
      <button
        class="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:cursor-not-allowed disabled:opacity-50"
        type="button"
        disabled={busy}
        onclick={requestClose}
        aria-label={m.cancel()}
        title={m.cancel()}
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
          <path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z" />
        </svg>
      </button>
    </header>

    <div class="flex min-h-0 flex-1 flex-col gap-4 p-5">
      {#if error}
        <p class="rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink" role="alert">
          {m.error_generic({ message: error })}
        </p>
      {/if}

      <div>
        <label
          for="playlist-transfer-destination"
          class="mb-1.5 block text-xs font-semibold text-ink-muted"
        >
          {m.playlist_transfer_destination()}
        </label>
        {#if loadingTargets}
          <div class="skeleton h-9 w-full rounded-md" aria-hidden="true"></div>
        {:else if targets?.length === 0}
          <p class="rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink-muted">
            {m.playlist_transfer_no_targets()}
          </p>
        {:else if targets}
          <select
            id="playlist-transfer-destination"
            class="w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none focus:border-accent disabled:opacity-50"
            bind:value={targetId}
            disabled={busy}
          >
            {#each targets as target (target.id)}
              <option value={target.id}>{target.name}</option>
            {/each}
          </select>
        {/if}
      </div>

      <div class="flex min-h-0 flex-1 flex-col gap-2">
        <label for="playlist-transfer-search" class="sr-only">
          {m.playlist_transfer_search()}
        </label>
        <input
          bind:this={searchInput}
          id="playlist-transfer-search"
          class="w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none placeholder:text-ink-muted focus:border-accent disabled:opacity-50"
          type="search"
          placeholder={m.playlist_transfer_search()}
          bind:value={query}
          disabled={busy}
        />

        <div class="flex flex-wrap items-center gap-2 text-xs">
          <button
            class="rounded-full bg-panel-2 px-3 py-1.5 font-semibold text-ink-muted transition-colors hover:text-ink disabled:cursor-not-allowed disabled:opacity-50"
            type="button"
            disabled={busy || visibleTracks.length === 0}
            onclick={selectVisible}
          >
            {m.playlist_transfer_select_all()}
          </button>
          <button
            class="rounded-full bg-panel-2 px-3 py-1.5 font-semibold text-ink-muted transition-colors hover:text-ink disabled:cursor-not-allowed disabled:opacity-50"
            type="button"
            disabled={busy || visibleTracks.length === 0}
            onclick={clearVisible}
          >
            {m.playlist_transfer_clear()}
          </button>
          <span class="ml-auto font-medium text-ink-muted" aria-live="polite">
            {m.playlist_transfer_selected({ count: selectedCount })}
          </span>
        </div>

        <div class="min-h-40 flex-1 overflow-y-auto rounded-lg border border-edge bg-panel">
          {#if visibleTracks.length === 0}
            <p class="px-4 py-8 text-center text-sm text-ink-muted">
              {m.playlist_transfer_no_matches()}
            </p>
          {:else}
            <ul class="p-1.5">
              {#each visibleTracks as entry (entry.entryId)}
                <li>
                  <label
                    class="flex cursor-pointer items-center gap-3 rounded-md px-3 py-2 transition-colors hover:bg-panel-2 has-[:focus-visible]:bg-panel-2 {busy
                      ? 'cursor-not-allowed opacity-50'
                      : ''}"
                  >
                    <input
                      class="h-4 w-4 shrink-0 accent-(--color-accent)"
                      type="checkbox"
                      checked={selectedEntryIds.has(entry.entryId)}
                      disabled={busy}
                      onchange={() => toggleEntry(entry.entryId)}
                    />
                    <span class="min-w-0 flex-1">
                      <span class="block truncate text-sm font-medium text-ink">
                        {entry.track.name}
                      </span>
                      <span class="block truncate text-xs text-ink-muted">
                        {entry.track.artist}{entry.track.album
                          ? " · " + entry.track.album
                          : ""}
                      </span>
                    </span>
                  </label>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>

      <div
        class="flex rounded-lg bg-panel-2 p-1"
        role="radiogroup"
        aria-label={m.playlist_transfer_title()}
      >
        <label
          class="flex flex-1 cursor-pointer items-center justify-center gap-2 rounded-md px-3 py-2 text-sm font-semibold transition-colors {moveEntries
            ? 'text-ink-muted hover:text-ink'
            : 'bg-card text-ink'}"
        >
          <input
            class="h-4 w-4 accent-(--color-accent)"
            type="radio"
            name="playlist-transfer-mode"
            value="copy"
            checked={!moveEntries}
            disabled={busy}
            onchange={() => (moveEntries = false)}
          />
          {m.playlist_transfer_copy()}
        </label>
        <label
          class="flex flex-1 cursor-pointer items-center justify-center gap-2 rounded-md px-3 py-2 text-sm font-semibold transition-colors {moveEntries
            ? 'bg-card text-ink'
            : 'text-ink-muted hover:text-ink'}"
        >
          <input
            class="h-4 w-4 accent-(--color-accent)"
            type="radio"
            name="playlist-transfer-mode"
            value="move"
            checked={moveEntries}
            disabled={busy}
            onchange={() => (moveEntries = true)}
          />
          {m.playlist_transfer_move()}
        </label>
      </div>
    </div>

    <footer class="flex items-center justify-end gap-2 border-t border-edge px-5 py-4">
      <button
        class="rounded-full px-4 py-2 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:cursor-not-allowed disabled:opacity-50"
        type="button"
        disabled={busy}
        onclick={requestClose}
      >
        {m.cancel()}
      </button>
      <button
        class="flex min-w-28 items-center justify-center gap-2 rounded-full bg-accent px-4 py-2 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
        type="button"
        disabled={!canSubmit}
        onclick={submit}
      >
        {#if busy}
          <span
            class="h-4 w-4 animate-spin rounded-full border-2 border-(--color-on-accent)/30 border-t-(--color-on-accent)"
            aria-hidden="true"
          ></span>
        {/if}
        {moveEntries ? m.playlist_transfer_confirm_move() : m.playlist_transfer_confirm_copy()}
      </button>
    </footer>
  </div>
</div>
</div>
