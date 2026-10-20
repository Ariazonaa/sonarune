<script lang="ts">
  import { onMount, tick } from "svelte";
  import { VList } from "virtua/svelte";
  import { goto } from "$app/navigation";
  import { api, formatDuration } from "#lib/api.js";
  import { apiLibrary } from "#lib/api/library.js";
  import { autoDjReasonLabel } from "#lib/autoDjReason.js";
  import { m } from "#lib/paraglide/messages.js";
  import { player } from "#lib/state/player.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import { downloads } from "#lib/state/downloads.svelte.js";
  import { playlists } from "#lib/state/playlists.svelte.js";
  import { FLIP_ROW_LIMIT, listFlip, uniqueRowKeys } from "#lib/motion.js";
  import {
    currentPosition,
    playOrder,
    playOrderItemIds,
    stopsAfter,
    type QueueEntry,
  } from "#lib/queueOrder.js";
  import { contextMenuKey } from "#lib/menu.js";
  import Cover from "./Cover.svelte";
  import EmptyState from "./EmptyState.svelte";
  import ContextMenu, { type ContextMenuItem } from "./ContextMenu.svelte";
  import AddToPlaylistMenu from "./AddToPlaylistMenu.svelte";
  import SongInfo from "./SongInfo.svelte";
  import type { QueueTrack } from "#lib/types.js";

  const queue = $derived(player.queue);
  const totalDurationMs = $derived(queue.tracks.reduce((sum, track) => sum + track.durationMs, 0));
  // The list shows the queue as it plays — the shuffle order while shuffle is
  // on. An entry's `index` is its stored position (jump, remove); its position
  // in this list is what queue_move takes (play next, drag & drop).
  const entries = $derived(playOrder(queue));
  const currentPos = $derived(currentPosition(queue, entries));

  // A queue past the FLIP limit (a played smart view runs to thousands of
  // tracks) renders only the rows in view: building every row with its cover
  // takes seconds there, and so does each track change that touches them all.
  // A shorter queue stays a plain list, whose reorders animate — past the
  // limit listFlip does not animate anyway.
  const virtual = $derived(entries.length > FLIP_ROW_LIMIT);
  let vlist = $state<VList<QueueEntry> | null>(null);
  /** How far (in rows) the long list still scrolls smoothly to the playing
   *  track; farther, it jumps. A few panel heights. */
  const SMOOTH_SCROLL_ROWS = 50;

  // Scrolling to what is playing. The row is found in the DOM rather than
  // bound: only one row at a time is the current one, and which row that is
  // changes with every track. The long list keeps it mounted wherever it is
  // scrolled (see `pinnedRows`).
  let listEl = $state<HTMLElement | null>(null);
  let currentOffscreen = $state(false);
  const currentRow = () => listEl?.querySelector<HTMLElement>('[data-current="true"]') ?? null;

  /** Put the playing entry in the middle of the panel. Instant when the panel
   *  just opened — an animation out of nowhere reads as the list moving by
   *  itself — and smooth when someone asked for it and can follow the motion.
   *  Only the list scrolls: scrollIntoView would scroll every container above
   *  it too, the app shell included. */
  function revealCurrent(behavior: ScrollBehavior) {
    if (virtual) {
      if (currentPos < 0 || !vlist) return;
      // Smooth only over a short way: a long smooth scroll mounts every row
      // it passes, the very cost the long list exists to avoid.
      const first = vlist.findItemIndex(vlist.getScrollOffset());
      const near = Math.abs(currentPos - first) <= SMOOTH_SCROLL_ROWS;
      vlist.scrollToIndex(currentPos, { align: "center", smooth: behavior === "smooth" && near });
      return;
    }
    const list = listEl;
    const row = currentRow();
    if (!list || !row) return;
    const listTop = list.getBoundingClientRect().top + list.clientTop;
    const box = row.getBoundingClientRect();
    const top = list.scrollTop + box.top - listTop - (list.clientHeight - box.height) / 2;
    list.scrollTo({ top, behavior });
  }

  onMount(() => revealCurrent("instant"));

  // Is the playing entry in view? Decides whether the pill shows, and whether
  // the list follows the music: while it is on screen the next track scrolls
  // into place, and while the user is reading somewhere else it does not.
  $effect(() => {
    queue.index;
    entries.length;
    const root = listEl;
    const row = currentRow();
    if (!root || !row) {
      currentOffscreen = false;
      return;
    }
    const observer = new IntersectionObserver(
      ([entry]) => (currentOffscreen = !entry.isIntersecting),
      { root, threshold: 0.6 },
    );
    observer.observe(row);
    return () => observer.disconnect();
  });

  let followedIndex = player.queue.index;
  $effect(() => {
    const index = queue.index;
    if (index === followedIndex) return;
    const follow = !currentOffscreen;
    followedIndex = index;
    // The row for the new index exists only after this update is applied.
    if (follow) void tick().then(() => revealCurrent("smooth"));
  });

  // Growing past the limit (Auto-DJ appending, say) or shrinking back swaps
  // the list for the other kind, which starts at the top: a playing row that
  // was in view goes back into view.
  let shownVirtual: boolean | null = null;
  $effect(() => {
    const mode = virtual;
    if (shownVirtual === null || mode === shownVirtual) {
      shownVirtual = mode;
      return;
    }
    shownVirtual = mode;
    if (!currentOffscreen) void tick().then(() => revealCurrent("instant"));
  });

  // "Jump to the playing track" from the palette or the shortcut. The panel
  // may have been opened by that same request, and then `onMount` has already
  // put the row in place — hence the counter: only a change means jump.
  let lastReveal = player.revealCurrent;
  $effect(() => {
    const token = player.revealCurrent;
    if (token === lastReveal) return;
    lastReveal = token;
    revealCurrent("smooth");
  });

  // A square in a circle: the sign for "and then it is quiet".
  const ICON_STOP =
    "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm0 18a8 8 0 1 1 0-16 8 8 0 0 1 0 16zM9 9h6v6H9V9z";

  let menu = $state<{ x: number; y: number; items: ContextMenuItem[]; label?: string; trigger?: HTMLElement } | null>(null);
  let addTo = $state<{ x: number; y: number; trackIds: string[] } | null>(null);
  let info = $state<{ itemId: string; name: string } | null>(null);

  // The stop, read once for the whole list: the player state is replaced on
  // every 400 ms tick, and a row reading it would re-run with each one. Plain
  // values, so a row re-runs only when the stop itself changes.
  const armedStop = $derived(player.state.stopAfter);
  const armedStopEntry = $derived(player.state.stopAfterEntry);
  const armedStopItem = $derived(player.state.stopAfterItem);
  const armedStopTitle = $derived(
    player.state.stopAfter === "album" ? m.queue_stop_after_album() : m.queue_stop_after_track(),
  );

  // Inline "save queue as playlist" naming state.
  let naming = $state(false);
  let playlistName = $state("");

  async function saveAsPlaylist() {
    const name = playlistName.trim();
    // As the list shows it: under shuffle, the shuffled and rearranged order.
    const trackIds = playOrderItemIds(player.queue);
    naming = false;
    playlistName = "";
    if (!name || trackIds.length === 0) return;
    try {
      await apiLibrary.createPlaylist(name, trackIds);
      playlists.refresh();
      toast.show(m.playlist_created());
    } catch (e) {
      player.error = String(e);
    }
  }

  function openQueueMenu(event: MouseEvent) {
    const trigger = event.currentTarget as HTMLElement;
    // The button toggles its menu: a press on it does not count as outside.
    if (menu?.trigger === trigger) {
      menu = null;
      return;
    }
    const rect = trigger.getBoundingClientRect();
    menu = {
      x: rect.left,
      y: rect.bottom + 4,
      trigger,
      label: m.queue_more_actions(),
      items: [
        {
          label: m.queue_remove_played({ count: queue.playedCount }),
          icon: "M13 3a9 9 0 1 1-8.95 8H2l3-3 3 3H5.05A7 7 0 1 0 13 5V3zm-1 4h2v5.41l3.29 3.3-1.41 1.41L12 13.24V7z",
          disabled: queue.playedCount === 0,
          action: () => player.run(player.removePlayed()),
        },
        {
          label: m.queue_remove_duplicates({ count: queue.duplicateCount }),
          icon: "M7 7h11v11H7V7zm2 2v7h7V9H9zM4 4h11v2H6v9H4V4z",
          disabled: queue.duplicateCount === 0,
          action: () => player.run(player.removeDuplicates()),
        },
        {
          label: m.queue_clear(),
          icon: "M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zm3.46-7.12 1.42-1.42L12 11.59l1.12-1.13 1.42 1.42L13.41 13l1.13 1.12-1.42 1.42L12 14.41l-1.12 1.13-1.42-1.42L10.59 13l-1.13-1.12zM15.5 4l-1-1h-5l-1 1H5v2h14V4z",
          action: () => player.run(player.clearQueue()),
        },
      ],
    };
  }

  function onRowContextMenu(event: MouseEvent, track: QueueTrack, index: number) {
    event.preventDefault();
    // For display/disabling only — the actions below re-resolve against the
    // live queue at click time (the current track can advance while the menu
    // is open, and rows can shift).
    const position = entries.findIndex((entry) => entry.index === index);
    const playNextSlot = position > currentPos ? currentPos + 1 : currentPos;
    // The row the user right-clicked, by entry identity, at click time. The
    // per-entry id tells two copies of the same track apart (an item id would
    // hit the other copy); an entry restored from an older build has none and
    // only still matches where it was.
    const liveIndex = () => {
      const tracks = player.queue.tracks;
      if (!track.entryId) return tracks[index]?.itemId === track.itemId ? index : -1;
      return tracks[index]?.entryId === track.entryId
        ? index
        : tracks.findIndex((t) => t.entryId === track.entryId);
    };
    menu = {
      x: event.clientX,
      y: event.clientY,
      items: [
        {
          // Armed on this row already? Then the entry takes it back off —
          // otherwise there would be no way to undo it from here.
          label:
            player.state.stopAfter === "track" && stopsAfter(player.state, track)
              ? m.queue_stop_after_track_off()
              : m.queue_stop_after_track(),
          icon: ICON_STOP,
          action: () =>
            player.run(
              api.playerSetStopAfter(
                player.state.stopAfter === "track" && stopsAfter(player.state, track)
                  ? "off"
                  : "track",
                track.itemId,
                track.entryId,
              ),
            ),
        },
        {
          label:
            player.state.stopAfter === "album" && stopsAfter(player.state, track)
              ? m.queue_stop_after_album_off()
              : m.queue_stop_after_album(),
          icon: ICON_STOP,
          disabled: !track.albumId,
          action: () =>
            player.run(
              api.playerSetStopAfter(
                player.state.stopAfter === "album" && stopsAfter(player.state, track)
                  ? "off"
                  : "album",
                track.itemId,
                track.entryId,
              ),
            ),
        },
        {
          label: m.queue_play_now(),
          icon: "M8 5v14l11-7L8 5z",
          disabled: index === queue.index,
          action: () => {
            const i = liveIndex();
            if (i >= 0) player.run(api.queueJump(i));
          },
        },
        {
          label: m.album_play_next(),
          icon: "M3 10h11v2H3v-2zm0-4h11v2H3V6zm0 8h7v2H3v-2zm13-1v8l6-4-6-4z",
          disabled: index === queue.index || playNextSlot === position,
          action: () => {
            const i = liveIndex();
            const q = player.queue;
            const live = playOrder(q);
            const from = live.findIndex((entry) => entry.index === i);
            const current = currentPosition(q, live);
            // queue_move removes then inserts: moving from before the current
            // track shifts its position down by one first.
            const slot = from > current ? current + 1 : current;
            if (i >= 0 && from >= 0 && i !== q.index && from !== slot) {
              player.run(api.queueMove(from, slot));
            }
          },
        },
        {
          label: m.queue_remove(),
          icon: "M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z",
          action: () => {
            const i = liveIndex();
            if (i >= 0) player.run(api.queueRemove(i, track.itemId, track.entryId));
          },
        },
        {
          label: m.add_to_playlist(),
          icon: "M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z",
          action: () => (addTo = { x: event.clientX, y: event.clientY, trackIds: [track.itemId] }),
        },
        {
          label: m.ctx_instant_mix(),
          icon: "M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.03 3.03L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z",
          action: () => player.run(api.playInstantMix(track.itemId)),
        },
        ...track.artists.map((a) => ({
          label: track.artists.length === 1 ? m.ctx_go_to_artist() : `${m.ctx_go_to_artist()}: ${a.name}`,
          icon: "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
          action: () => goto(`/artist/${a.id}`),
        })),
        {
          label: m.ctx_info(),
          icon: "M11 7h2v2h-2V7zm0 4h2v6h-2v-6zm1-9C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8z",
          action: () => (info = { itemId: track.itemId, name: track.name }),
        },
        {
          label: m.ctx_download(),
          icon: "M5 20h14v-2H5v2zM19 9h-4V3H9v6H5l7 7 7-7z",
          action: () =>
            downloads
              .enqueue([{ itemId: track.itemId, name: track.name }])
              .then(() => toast.show(m.download_queued_one(), { kind: "info" }))
              .catch((e) => (player.error = String(e))),
        },
      ],
    };
  }

  // Native HTML5 drag & drop reorder, in list (= play-order) positions.
  // `dropAt` is the slot the dragged track would land in (insert-before
  // semantics; length = append at the end).
  let dragFrom = $state<number | null>(null);
  let dropAt = $state<number | null>(null);

  // Rows the long list keeps mounted off screen: the playing one, which the
  // pill and the follow-the-music logic watch, and the one being dragged — a
  // drag ends on its source row, and dragging scrolls the list.
  const pinnedRows = $derived([currentPos, dragFrom ?? -1].filter((position) => position >= 0));

  function onDrop() {
    if (dragFrom !== null && dropAt !== null) {
      // queue_move uses remove-then-insert semantics: dropping below the
      // origin shifts the target up by one.
      const to = dropAt > dragFrom ? dropAt - 1 : dropAt;
      if (to !== dragFrom) {
        player.run(api.queueMove(dragFrom, to));
      }
    }
    dragFrom = null;
    dropAt = null;
  }

  function onDragOver(event: DragEvent, index: number) {
    event.preventDefault();
    // Measure the row itself, not the whole entry: the "up next" heading sits
    // in the same entry and would shift the midpoint.
    const item = event.currentTarget as HTMLElement;
    const row = (item.lastElementChild as HTMLElement | null) ?? item;
    const rect = row.getBoundingClientRect();
    dropAt = event.clientY < rect.top + rect.height / 2 ? index : index + 1;
  }

  // Queue entries carry a per-entry id, which survives moves — the stable
  // key FLIP and the long list's rows need. Entries from older builds may
  // lack it.
  const rowKeys = $derived(uniqueRowKeys(entries.map((entry) => entry.track.entryId)));
</script>

<aside class="surface relative flex w-80 shrink-0 flex-col rounded-panel bg-panel">
  <div class="flex items-center justify-between gap-2 px-4 py-3">
    {#if naming}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="w-full rounded-full border border-edge bg-panel-2 px-3 py-1 text-xs outline-none focus:border-accent"
        placeholder={m.palette_playlist_name()}
        bind:value={playlistName}
        autofocus
        onblur={() => (naming = false)}
        onkeydown={(e) => {
          // Claimed, so the view behind the panel does not act on the key
          // too (now playing closes on an Escape nobody took).
          if (e.key === "Enter") {
            e.preventDefault();
            saveAsPlaylist();
          } else if (e.key === "Escape") {
            e.preventDefault();
            naming = false;
          }
        }}
      />
    {:else}
      <div class="min-w-0">
        <h2 class="text-sm font-bold">{m.queue_title()}</h2>
        <p class="truncate text-[11px] text-ink-muted tabular-nums">
          {queue.tracks.length === 1
            ? m.queue_summary_one({ duration: formatDuration(totalDurationMs) })
            : m.queue_summary_many({
                count: queue.tracks.length,
                duration: formatDuration(totalDurationMs),
              })}
        </p>
      </div>
      <div class="flex items-center gap-1">
        {#if queue.tracks.length > 0}
          <button
            class="rounded-md p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
            onclick={() => {
              naming = true;
              playlistName = "";
            }}
            aria-label={m.queue_save_playlist()}
            title={m.queue_save_playlist()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M17 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V7l-4-4zm-5 16a3 3 0 1 1 0-6 3 3 0 0 1 0 6zm3-10H5V5h10v4z" />
            </svg>
          </button>
        {/if}
        {#if queue.canUndo}
          <button
            class="rounded-md p-1 text-accent transition-colors hover:bg-panel-2 hover:text-accent-hover"
            onclick={() => player.run(player.undoQueue())}
            aria-label={m.queue_undo()}
            title={m.queue_undo()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M7.5 9H15a5 5 0 0 1 0 10h-4v-2h4a3 3 0 0 0 0-6H7.5l3 3L9 15.5 3.5 10 9 4.5 10.5 6l-3 3z" />
            </svg>
          </button>
        {/if}
        {#if queue.tracks.length > 0}
          <button
            class="rounded-md p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
            onclick={openQueueMenu}
            aria-haspopup="menu"
            aria-label={m.queue_more_actions()}
            title={m.queue_more_actions()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z" />
            </svg>
          </button>
        {/if}
      </div>
    {/if}
  </div>

  <!-- One entry of the list, in either kind of list. `i` is its play-order
       position, `index` its stored one. -->
  {#snippet row({ track, index }: QueueEntry, i: number)}
    <div
      role="listitem"
      oncontextmenu={(e) => onRowContextMenu(e, track, index)}
      {@attach contextMenuKey}
      draggable="true"
      ondragstart={(e) => {
        dragFrom = i;
        if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
      }}
      ondragover={(e) => onDragOver(e, i)}
      ondrop={onDrop}
      ondragend={() => {
        dragFrom = null;
        dropAt = null;
      }}
    >
      {#if i === currentPos + 1}
        <p class="px-2 pt-3 pb-1 text-[10px] font-bold uppercase tracking-wider text-ink-muted">
          {m.queue_up_next()}
        </p>
      {/if}
      <div
        data-current={index === queue.index}
        class="group relative flex items-center gap-2 rounded-md px-2 py-1.5
          {index === queue.index ? 'bg-panel-2' : 'hover:bg-panel-2/60'}
          {dragFrom === i ? 'opacity-40' : ''}"
      >
      {#if dropAt === i && dragFrom !== null}
        <div class="pointer-events-none absolute inset-x-1 top-0 h-0.5 rounded bg-accent"></div>
      {/if}
      {#if dropAt === i + 1 && dragFrom !== null}
        <div class="pointer-events-none absolute inset-x-1 bottom-0 h-0.5 rounded bg-accent"></div>
      {/if}
      <button
        class="flex min-w-0 flex-1 items-center gap-2 text-left"
        onclick={() => player.run(api.queueJump(index))}
      >
        <div class="w-9 shrink-0">
          <Cover itemId={track.imageItemId} tag={track.imageTag} size={96} alt="" blurhash={track.imageBlurHash} />
        </div>
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm {index === queue.index ? 'text-accent' : ''}">
            {track.name}
          </p>
          <p class="truncate text-xs text-ink-muted">{track.artist}</p>
          {#if track.autoDjReason}
            <p class="truncate text-[10px] text-accent">{autoDjReasonLabel(track.autoDjReason)}</p>
          {/if}
        </div>
      </button>
      {#if stopsAfter({ stopAfter: armedStop, stopAfterEntry: armedStopEntry, stopAfterItem: armedStopItem }, track)}
        <!-- The armed stop belongs on the row it was armed on: nothing
             else in the list says where the music will end. -->
        <span
          class="shrink-0 text-accent"
          title={armedStopTitle}
          aria-label={m.queue_stop_after_armed()}
        >
          <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true">
            <path d={ICON_STOP} />
          </svg>
        </span>
      {/if}
      <span class="text-[11px] text-ink-muted tabular-nums">
        {formatDuration(track.durationMs)}
      </span>
      <button
        class="invisible rounded p-1 text-ink-muted hover:text-ink group-hover:visible"
        onclick={() => player.run(api.queueRemove(index, track.itemId, track.entryId))}
        aria-label={m.queue_remove()}
        title={m.queue_remove()}
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
          <path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/>
        </svg>
      </button>
      </div>
    </div>
  {/snippet}

  {#if queue.tracks.length === 0}
    <EmptyState
      icon="playlist"
      title={m.queue_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
      compact
    />
  {:else if virtual}
    <div
      bind:this={listEl}
      class="min-h-0 flex-1 py-2"
      class:dragging={dragFrom !== null}
      role="list"
      ondragleave={() => (dropAt = null)}
    >
      <VList
        bind:this={vlist}
        data={entries}
        getKey={(_, i) => rowKeys[i]}
        keepMounted={pinnedRows}
        class="px-2"
        style="height: 100%;"
      >
        {#snippet children(entry, i)}
          {@render row(entry, i)}
        {/snippet}
      </VList>
    </div>
  {:else}
    <div
      bind:this={listEl}
      class="min-h-0 flex-1 overflow-y-auto p-2"
      role="list"
      ondragleave={() => (dropAt = null)}
    >
      {#each entries as entry, i (rowKeys[i])}
        <!-- A wrapper for the animation alone: an animated element has to be
             the only child of its {#each}. -->
        <div animate:listFlip={{ rows: entries.length }}>
          {@render row(entry, i)}
        </div>
      {/each}
    </div>
  {/if}

  <!-- Only while the playing entry is scrolled out of sight: it is an offer to
       go back to it, not a permanent control. -->
  {#if currentOffscreen}
    <button
      class="absolute bottom-3 left-1/2 z-10 flex -translate-x-1/2 items-center gap-1.5 whitespace-nowrap rounded-full bg-accent px-3 py-1.5 text-xs font-bold text-(--color-on-accent) shadow-xl transition-transform hover:scale-105"
      onclick={() => revealCurrent("smooth")}
    >
      <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true">
        <path
          d="M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zm8.94 3A9 9 0 0 0 13 3.06V1h-2v2.06A9 9 0 0 0 3.06 11H1v2h2.06A9 9 0 0 0 11 20.94V23h2v-2.06A9 9 0 0 0 20.94 13H23v-2h-2.06zM12 19a7 7 0 1 1 0-14 7 7 0 0 1 0 14z"
        />
      </svg>
      {m.queue_jump_to_current()}
    </button>
  {/if}
</aside>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} label={menu.label} trigger={menu.trigger} onclose={() => (menu = null)} />
{/if}
{#if addTo}
  <AddToPlaylistMenu x={addTo.x} y={addTo.y} trackIds={addTo.trackIds} onclose={() => (addTo = null)} />
{/if}
{#if info}
  <SongInfo itemId={info.itemId} name={info.name} onclose={() => (info = null)} />
{/if}

<style>
  /* The long list's rows box (VList's viewport > virtua's container). virtua
     takes pointer events off it while the list scrolls, until 150 ms after the
     last scroll — and a drag scrolls the list when it nears an edge. A row
     that cannot be hit cannot take the drop, so while a drag runs, the rows
     stay targets. */
  .dragging > :global(div) > :global(div) {
    pointer-events: auto !important;
  }
</style>
