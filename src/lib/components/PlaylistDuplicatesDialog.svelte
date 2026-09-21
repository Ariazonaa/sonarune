<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { portal } from "$lib/portal";
  import type { PlaylistTrackDto } from "$lib/types";

  let {
    tracks,
    busy = false,
    error = null,
    onclose,
    onconfirm,
  }: {
    tracks: PlaylistTrackDto[];
    busy?: boolean;
    error?: string | null;
    onclose: () => void;
    onconfirm: (entryIds: string[]) => void;
  } = $props();

  const groups = $derived.by(() => {
    const byTrack = new Map<
      string,
      { track: PlaylistTrackDto["track"]; positions: number[]; entryIds: string[] }
    >();
    for (const [index, entry] of tracks.entries()) {
      const group = byTrack.get(entry.track.id);
      if (group) {
        group.positions.push(index + 1);
        group.entryIds.push(entry.entryId);
      } else {
        byTrack.set(entry.track.id, {
          track: entry.track,
          positions: [index + 1],
          entryIds: [entry.entryId],
        });
      }
    }
    return [...byTrack.values()].filter((group) => group.entryIds.length > 1);
  });

  // Keep the first occurrence of every item and remove later playlist entries
  // by their unique entry ids. Never use the (possibly duplicated) item id for
  // a mutation.
  const removeEntryIds = $derived(groups.flatMap((group) => group.entryIds.slice(1)));

  function onKeydown(event: KeyboardEvent) {
    // defaultPrevented: an overlay on top (a confirmation) already handled it.
    if (!busy && !event.defaultPrevented && event.key === "Escape") {
      event.preventDefault();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- The wrapper stays in place; the overlay moves to <body> (see portal.ts). -->
<div class="contents">
<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div
  {@attach portal}
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-4"
  onclick={(event) => {
    if (!busy && event.target === event.currentTarget) onclose();
  }}
>
  <div
    class="overlay flex max-h-[80vh] w-full max-w-xl flex-col overflow-hidden rounded-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={m.playlist_duplicates_title()}
  >
    <div class="flex items-start justify-between gap-3 border-b border-edge px-5 py-4">
      <div class="min-w-0">
        <h2 class="text-md font-bold tracking-tight">{m.playlist_duplicates_title()}</h2>
        <p class="mt-1 text-sm text-ink-muted">
          {(removeEntryIds.length === 1
            ? m.playlist_duplicates_preview_one
            : m.playlist_duplicates_preview_many)({ count: removeEntryIds.length })}
        </p>
      </div>
      <button
        class="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-40"
        onclick={onclose}
        disabled={busy}
        aria-label={m.dismiss()}
        title={m.dismiss()}
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
          <path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z" />
        </svg>
      </button>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto p-3">
      {#if groups.length === 0}
        <p class="px-2 py-4 text-sm text-ink-muted">{m.playlist_duplicates_none()}</p>
      {:else}
        <ul class="space-y-1">
          {#each groups as group (group.track.id)}
            <li class="rounded-lg bg-panel px-3 py-2.5">
              <div class="flex items-start justify-between gap-4">
                <div class="min-w-0">
                  <p class="truncate text-sm font-semibold">{group.track.name}</p>
                  <p class="truncate text-xs text-ink-muted">
                    {group.track.artist}{group.track.album ? ` · ${group.track.album}` : ""}
                  </p>
                </div>
                <span class="shrink-0 rounded-full bg-panel-2 px-2 py-0.5 text-xs font-semibold text-ink-muted">
                  {m.playlist_duplicates_occurrences({ count: group.positions.length })}
                </span>
              </div>
              <p class="mt-1.5 text-xs text-ink-muted">
                {m.playlist_duplicates_positions({
                  keep: group.positions[0],
                  remove: group.positions.slice(1).map((position) => `#${position}`).join(", "),
                })}
              </p>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    {#if error}
      <p class="mx-5 mb-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
        {m.error_generic({ message: error })}
      </p>
    {/if}

    <div class="flex justify-end gap-2 border-t border-edge px-5 py-4">
      <button
        class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-40"
        onclick={onclose}
        disabled={busy}
      >
        {m.cancel()}
      </button>
      <button
        class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
        onclick={() => onconfirm(removeEntryIds)}
        disabled={busy || removeEntryIds.length === 0}
      >
        {m.playlist_duplicates_confirm({ count: removeEntryIds.length })}
      </button>
    </div>
  </div>
</div>
</div>
