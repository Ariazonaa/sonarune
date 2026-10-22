<script lang="ts">
  import { untrack } from "svelte";
  import { Virtualizer } from "virtua/svelte";
  import { formatDuration } from "#lib/api.js";
  import { m } from "#lib/paraglide/messages.js";
  import { dropSlot, moveTarget } from "#lib/dragReorder.js";
  import { isLongList, offsetIn, scrollParent, type ListPosition } from "#lib/longList.js";
  import { contextMenuKey } from "#lib/menu.js";
  import { listFlip } from "#lib/motion.js";
  import type { PlaylistRow } from "#lib/playlistEntries.js";
  import { activatesRow } from "#lib/rowKeys.js";
  import type { PlaylistTrackDto } from "#lib/types.js";
  import Cover from "./Cover.svelte";

  let {
    rows,
    total,
    currentTrackId,
    reorderable,
    onplay,
    onmenu,
    onaddto,
    onremove,
    onmove,
  }: {
    /** The rows to show: all entries, or those the filter lets through. */
    rows: PlaylistRow[];
    /** Entries in the playlist. Picks the kind of list, so typing in the
     *  filter never swaps one for the other. */
    total: number;
    currentTrackId: string | undefined;
    /** Rows can be dragged to another place. */
    reorderable: boolean;
    /** Play the playlist from the entry at this position. */
    onplay: (index: number) => void;
    onmenu: (event: MouseEvent, entry: PlaylistTrackDto, index: number) => void;
    onaddto: (event: MouseEvent, trackIds: string[]) => void;
    onremove: (entryId: string) => void;
    /** Move the entry at `from` to `to` (remove-then-insert positions). */
    onmove: (from: number, to: number) => void;
  } = $props();

  // A long playlist (a materialized smart view runs to thousands of entries)
  // mounts only the rows in view, in the page's own scroller — the header
  // scrolls away above it as before. A shorter one is a plain list whose
  // reorders animate.
  const virtual = $derived(isLongList(total));

  let wrapper = $state<HTMLElement | null>(null);
  let scroller = $state<HTMLElement | null>(null);
  let vlist = $state<Virtualizer<PlaylistRow> | null>(null);
  /** How far below the top of the scroller the list starts (the header). */
  let startMargin = $state(0);

  function measure() {
    if (wrapper && scroller) startMargin = offsetIn(wrapper, scroller);
  }

  $effect(() => {
    if (!wrapper) return;
    const found = scrollParent(wrapper);
    scroller = found;
    if (!found) return;
    untrack(measure);
    // A narrower window wraps the buttons above the list: it starts lower.
    const observer = new ResizeObserver(() => measure());
    observer.observe(found);
    return () => observer.disconnect();
  });

  // The filter's hint lines above the list come and go with the rows.
  $effect(() => {
    void rows;
    measure();
  });

  // virtua counts from the top, but the page's scroller may stand further
  // down already — it keeps its offset from page to page. Let the long list
  // see where it is.
  $effect(() => {
    const element = scroller;
    if (!vlist || !element) return;
    const timer = setTimeout(() => {
      if (element.scrollTop > 0) element.dispatchEvent(new Event("scroll"));
    });
    return () => clearTimeout(timer);
  });

  let restoring = $state<ListPosition | null>(null);

  /** Where the list is scrolled to, for the way back. */
  export function position(): ListPosition | null {
    if (!scroller) return null;
    const top = scroller.scrollTop;
    if (!virtual || !vlist || top < startMargin || rows.length === 0) {
      return { top, index: -1, within: 0 };
    }
    const index = vlist.findItemIndex(top);
    return { top, index, within: top - startMargin - vlist.getItemOffset(index) };
  }

  /** Scroll back to `at` once the rows are there. */
  export function restore(at: ListPosition) {
    restoring = at;
  }

  $effect(() => {
    const at = restoring;
    if (!at || !scroller || (virtual && !vlist)) return;
    restoring = null;
    // The long list has measured none of the rows above: it goes by the row,
    // and virtua scrolls on until the rows on the way are measured.
    if (virtual && vlist && at.index >= 0 && at.index < rows.length) {
      vlist.scrollToIndex(at.index, { align: "start", offset: at.within });
    } else {
      scroller.scrollTop = at.top;
    }
  });

  // Native HTML5 drag & drop reorder, in playlist positions (`originalIndex`):
  // `dropAt` is the slot the dragged row would land in (see dragReorder.ts).
  // The dragged row is held by its key, not its position: a reload of the
  // playlist during the drag can move it, or take it away.
  let drag = $state<{ key: string } | null>(null);
  let dropAt = $state<number | null>(null);
  /** The focused row: the long list keeps it mounted wherever it is
   *  scrolled, so focus and Tab go on from there, as in the plain list. */
  let focusedKey = $state<string | null>(null);

  const positions = $derived(virtual ? new Map(rows.map((row, at) => [row.key, at])) : null);
  // Rows the long list keeps mounted off screen: the focused one, and the one
  // being dragged — a drag ends on its source row, and dragging scrolls.
  const pinned = $derived.by(() => {
    const at = [focusedKey, drag?.key ?? null]
      .map((key) => (key === null ? undefined : positions?.get(key)))
      .filter((position) => position !== undefined);
    return [...new Set(at)];
  });

  function onDragStart(event: DragEvent, row: PlaylistRow) {
    if (!reorderable) return;
    drag = { key: row.key };
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  }

  /** A row a drag of this list comes over takes it, as `dragover` does. Left
   *  alone, the drop is lost when the list scrolls under a still pointer —
   *  held at the edge of `<main>`: each row that slides under it gets a
   *  `dragenter`, which nothing accepts, and Chromium lets go of the drop
   *  although every `dragover` said move (WebView2 155, 22.10.2026). */
  function onDragEnter(event: DragEvent) {
    if (drag && reorderable) event.preventDefault();
  }

  function onDragOver(event: DragEvent, row: PlaylistRow) {
    // Only a row of this list is a move: a file or a track from elsewhere
    // is not taken.
    if (!drag || !reorderable) return;
    event.preventDefault();
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    dropAt = dropSlot(event.clientY, rect, row.originalIndex);
  }

  function onDrop(event: DragEvent, row: PlaylistRow) {
    const dragged = drag;
    if (dragged && reorderable) {
      event.preventDefault();
      // The dragged entry where it stands now; gone, the drop moves nothing.
      const source = rows.find((candidate) => candidate.key === dragged.key);
      // Where the row is let go, which the last dragover (they come every
      // 50 ms or so) may not have seen yet.
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
      const slot = dropSlot(event.clientY, rect, row.originalIndex);
      const to = source ? moveTarget(source.originalIndex, slot) : null;
      if (source && to !== null) onmove(source.originalIndex, to);
    }
    endDrag();
  }

  /** The pointer left the list — not just one row for the next. */
  function onDragLeave(event: DragEvent) {
    const next = event.relatedTarget;
    if (!(next instanceof Node) || !wrapper?.contains(next)) dropAt = null;
  }

  function endDrag() {
    drag = null;
    dropAt = null;
  }

  function onFocusIn(event: FocusEvent) {
    const row = (event.target as HTMLElement).closest<HTMLElement>("[data-row-key]");
    focusedKey = row?.dataset.rowKey ?? null;
  }

  function onFocusOut(event: FocusEvent) {
    const next = event.relatedTarget;
    if (!(next instanceof Node) || !wrapper?.contains(next)) focusedKey = null;
  }
</script>

<!-- A drag can end anywhere — on a row, outside the window, on something
     that is not a row, or by Escape. The source row says so (`dragend`
     bubbles here), the long list keeps that row mounted, and a press shows
     a drag that went unreported is over: nothing stays marked. -->
<svelte:window ondragend={endDrag} ondrop={endDrag} onpointerdown={() => drag && endDrag()} />

{#snippet rowView(row: PlaylistRow)}
  {@const entry = row.entry}
  {@const index = row.originalIndex}
  {@const playing = entry.track.id === currentTrackId}
  <div
    data-list-row
    data-row-key={row.key}
    class="group relative flex cursor-pointer items-center rounded-md py-2 text-sm outline-none transition-colors hover:bg-ink/10 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent
      {playing ? 'text-accent' : ''}
      {drag?.key === row.key ? 'opacity-40' : ''}"
    tabindex="0"
    role="button"
    aria-label={entry.track.name}
    draggable={reorderable}
    ondragstart={(event) => onDragStart(event, row)}
    ondragenter={onDragEnter}
    ondragover={(event) => onDragOver(event, row)}
    ondrop={(event) => onDrop(event, row)}
    ondragend={endDrag}
    oncontextmenu={(event) => onmenu(event, entry, index)}
    {@attach contextMenuKey}
    onclick={() => onplay(index)}
    onkeydown={(event) => {
      if (activatesRow(event)) {
        event.preventDefault();
        onplay(index);
      }
    }}
  >
    {#if drag && dropAt === index}
      <div class="pointer-events-none absolute inset-x-1 top-0 h-0.5 rounded bg-accent"></div>
    {/if}
    {#if drag && dropAt === index + 1}
      <div class="pointer-events-none absolute inset-x-1 bottom-0 h-0.5 rounded bg-accent"></div>
    {/if}
    <span class="w-10 shrink-0 px-2 text-right text-ink-muted tabular-nums">{index + 1}</span>
    <div class="w-11 shrink-0 pl-1">
      <div class="w-9">
        <Cover
          itemId={entry.track.imageItemId}
          tag={entry.track.imageTag}
          size={96}
          alt=""
          blurhash={entry.track.imageBlurHash}
        />
      </div>
    </div>
    <div class="min-w-0 flex-1 px-3">
      <p class="truncate font-medium {playing ? '' : 'text-ink'}">{entry.track.name}</p>
      <p class="truncate text-xs text-ink-muted">
        {entry.track.artist}{entry.track.album ? ` · ${entry.track.album}` : ""}
      </p>
    </div>
    <div class="w-16 shrink-0 px-1">
      <span class="invisible flex justify-end gap-0.5 group-hover:visible">
        <button
          class="rounded p-1 text-ink-muted hover:text-ink"
          onclick={(event) => onaddto(event, [entry.track.id])}
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
            onremove(entry.entryId);
          }}
          aria-label={m.playlist_remove_track()}
          title={m.playlist_remove_track()}
        >
          <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
            <path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" />
          </svg>
        </button>
      </span>
    </div>
    <span class="w-16 shrink-0 px-2 text-right text-ink-muted tabular-nums">
      {formatDuration(entry.track.durationMs)}
    </span>
  </div>
{/snippet}

<div
  bind:this={wrapper}
  class:dragging={drag !== null}
  role="presentation"
  ondragleave={onDragLeave}
  onfocusin={onFocusIn}
  onfocusout={onFocusOut}
>
  {#if virtual}
    {#if scroller}
      <Virtualizer
        bind:this={vlist}
        data={rows}
        getKey={(row) => row.key}
        scrollRef={scroller}
        {startMargin}
        keepMounted={pinned}
        onscroll={measure}
      >
        {#snippet children(row)}
          {@render rowView(row)}
        {/snippet}
      </Virtualizer>
    {/if}
  {:else}
    {#each rows as row (row.key)}
      <!-- A wrapper for the animation alone: an animated element has to be
           the only child of its {#each}. -->
      <div animate:listFlip={{ rows: rows.length }}>
        {@render rowView(row)}
      </div>
    {/each}
  {/if}
</div>

<style>
  /* The long list's rows box (virtua's container). virtua takes pointer
     events off it while the page scrolls, until 150 ms after the last scroll
     — and a drag scrolls the page when it nears an edge. A row that cannot be
     hit cannot take the drop, so while a drag runs, the rows stay targets. */
  .dragging > :global(div) {
    pointer-events: auto !important;
  }
</style>
