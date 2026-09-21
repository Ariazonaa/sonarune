<script lang="ts">
  import { goto } from "$app/navigation";
  import { api, formatDuration } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import { player } from "$lib/state/player.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { downloads } from "$lib/state/downloads.svelte";
  import { listFlip, uniqueRowKeys } from "$lib/motion";
  import { currentPosition, playOrder } from "$lib/queueOrder";
  import { contextMenuKey } from "$lib/menu";
  import Cover from "./Cover.svelte";
  import EmptyState from "./EmptyState.svelte";
  import ContextMenu, { type ContextMenuItem } from "./ContextMenu.svelte";
  import AddToPlaylistMenu from "./AddToPlaylistMenu.svelte";
  import SongInfo from "./SongInfo.svelte";
  import type { QueueTrack } from "$lib/types";

  const queue = $derived(player.queue);
  const totalDurationMs = $derived(queue.tracks.reduce((sum, track) => sum + track.durationMs, 0));
  // The list shows the queue as it plays — the shuffle order while shuffle is
  // on. An entry's `index` is its stored position (jump, remove); its position
  // in this list is what queue_move takes (play next, drag & drop).
  const entries = $derived(playOrder(queue));
  const currentPos = $derived(currentPosition(queue, entries));

  let menu = $state<{ x: number; y: number; items: ContextMenuItem[]; label?: string } | null>(null);
  let addTo = $state<{ x: number; y: number; trackIds: string[] } | null>(null);
  let info = $state<{ itemId: string; name: string } | null>(null);

  function autoDjReason(track: QueueTrack): string | null {
    if (!track.autoDjReason) return null;
    const kind = {
      track: m.settings_autodj_seed_track(),
      artist: m.settings_autodj_seed_artist(),
      genre: m.settings_autodj_seed_genre(),
      album: m.settings_autodj_seed_album(),
      playlist: m.settings_autodj_seed_playlist(),
    }[track.autoDjReason.kind];
    return track.autoDjReason.label
      ? m.queue_autodj_reason_detail({ kind, label: track.autoDjReason.label })
      : m.queue_autodj_reason({ kind });
  }

  // Inline "save queue as playlist" naming state.
  let naming = $state(false);
  let playlistName = $state("");

  async function saveAsPlaylist() {
    const name = playlistName.trim();
    const trackIds = player.queue.tracks.map((t) => t.itemId);
    naming = false;
    playlistName = "";
    if (!name || trackIds.length === 0) return;
    try {
      await apiLibrary.createPlaylist(name, trackIds);
      toast.show(m.playlist_created());
    } catch (e) {
      player.error = String(e);
    }
  }

  function openQueueMenu(event: MouseEvent) {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    menu = {
      x: rect.left,
      y: rect.bottom + 4,
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
            if (i >= 0) player.run(api.queueRemove(i, track.itemId));
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
    // Measure the row itself, not the <li>: the "up next" heading sits in the
    // same <li> and would shift the midpoint.
    const item = event.currentTarget as HTMLElement;
    const row = (item.lastElementChild as HTMLElement | null) ?? item;
    const rect = row.getBoundingClientRect();
    dropAt = event.clientY < rect.top + rect.height / 2 ? index : index + 1;
  }

  // Queue entries carry a per-entry id, which survives moves — the stable
  // key FLIP needs. Entries from older builds may lack it.
  const rowKeys = $derived(uniqueRowKeys(entries.map((entry) => entry.track.entryId)));
</script>

<aside class="surface flex w-80 shrink-0 flex-col rounded-panel bg-panel">
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
          if (e.key === "Enter") saveAsPlaylist();
          else if (e.key === "Escape") naming = false;
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

  {#if queue.tracks.length === 0}
    <EmptyState
      icon="playlist"
      title={m.queue_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
      compact
    />
  {:else}
    <ul class="min-h-0 flex-1 overflow-y-auto p-2" ondragleave={() => (dropAt = null)}>
      {#each entries as { track, index }, i (rowKeys[i])}
        <li
          animate:listFlip={{ rows: entries.length }}
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
          <!-- Inside the <li>: an animated element has to be the only child
               of its {#each}. -->
          {#if i === currentPos + 1}
            <p class="px-2 pt-3 pb-1 text-[10px] font-bold uppercase tracking-wider text-ink-muted">
              {m.queue_up_next()}
            </p>
          {/if}
          <div
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
                <p class="truncate text-[10px] text-accent">{autoDjReason(track)}</p>
              {/if}
            </div>
          </button>
          <span class="text-[11px] text-ink-muted tabular-nums">
            {formatDuration(track.durationMs)}
          </span>
          <button
            class="invisible rounded p-1 text-ink-muted hover:text-ink group-hover:visible"
            onclick={() => player.run(api.queueRemove(index, track.itemId))}
            aria-label={m.queue_remove()}
            title={m.queue_remove()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
              <path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/>
            </svg>
          </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</aside>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} label={menu.label} onclose={() => (menu = null)} />
{/if}
{#if addTo}
  <AddToPlaylistMenu x={addTo.x} y={addTo.y} trackIds={addTo.trackIds} onclose={() => (addTo = null)} />
{/if}
{#if info}
  <SongInfo itemId={info.itemId} name={info.name} onclose={() => (info = null)} />
{/if}
