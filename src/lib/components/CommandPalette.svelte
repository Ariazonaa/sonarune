<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { goto } from "$app/navigation";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "./Cover.svelte";
  import { palette } from "$lib/state/palette.svelte";
  import { player } from "$lib/state/player.svelte";
  import type { SearchResults, TrackDto } from "$lib/types";

  interface Entry {
    id: string;
    group: "commands" | "results";
    label: string;
    sub?: string;
    icon?: string;
    cover?: { itemId: string | null; tag: string | null; blurhash: string | null; round?: boolean };
    disabled?: boolean;
    run: () => void;
  }

  const I = {
    play: "M8 5v14l11-7L8 5z",
    next: "M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z",
    prev: "M6 6h2v12H6V6zm3.5 6l8.5 6V6l-8.5 6z",
    queue: "M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zM17 6v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V8h3V6h-5z",
    mic: "M12 2a4 4 0 0 0-4 4v6a4 4 0 0 0 8 0V6a4 4 0 0 0-4-4zm-6 10a6 6 0 0 0 5 5.92V21h2v-3.08A6 6 0 0 0 18 12h-2a4 4 0 0 1-8 0H6z",
    bars: "M3 13h2v4H3v-4zm4-6h2v10H7V7zm4 3h2v7h-2v-7zm4-5h2v12h-2V5zm4 8h2v4h-2v-4z",
    expand: "M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z",
    home: "M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z",
    grid: "M3 3h8v8H3V3zm10 0h8v8h-8V3zM3 13h8v8H3v-8zm10 0h8v8h-8v-8z",
    person: "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
    note: "M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z",
    playlist: "M3 6h13v2H3V6zm0 4h13v2H3v-2zm0 4h9v2H3v-2zm13 0v-4h2v4h4v2h-4v4h-2v-4h-4v-2h4z",
    search: "M15.5 14h-.79l-.28-.27a6.5 6.5 0 1 0-.7.7l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0A4.5 4.5 0 1 1 14 9.5 4.5 4.5 0 0 1 9.5 14z",
    sliders: "M3 17v2h6v-2H3zM3 5v2h10V5H3zm10 16v-2h8v-2h-8v-2h-2v6h2zM7 9v2H3v2h4v2h2V9H7zm14 4v-2H11v2h10zm-6-4h2V7h4V5h-4V3h-2v6z",
    save: "M17 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V7l-4-4zm-5 16a3 3 0 1 1 0-6 3 3 0 0 1 0 6zm3-10H5V5h10v4z",
    moon: "M12.1 22c-5.5 0-10-4.5-10-10 0-5 3.7-9.2 8.6-9.9.5-.1.9.4.7.9-.3.8-.4 1.6-.4 2.5 0 4.1 3.3 7.4 7.4 7.4.9 0 1.7-.1 2.5-.4.5-.2 1 .3.9.7-.8 4.9-5 8.8-9.7 8.8z",
    mix: "M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.03 3.03L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z",
    target:
      "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zm8.94 3A9 9 0 0 0 13 3.06V1h-2v2.06A9 9 0 0 0 3.06 11H1v2h2.06A9 9 0 0 0 11 20.94V23h2v-2.06A9 9 0 0 0 20.94 13H23v-2h-2.06zM12 19a7 7 0 1 1 0-14 7 7 0 0 1 0 14z",
  };

  let query = $state("");
  let activeIndex = $state(0);
  let results = $state<SearchResults | null>(null);
  /** Inline "save queue as playlist" mode. */
  let naming = $state(false);
  let playlistName = $state("");
  let listEl = $state<HTMLElement | null>(null);

  let timer: ReturnType<typeof setTimeout> | undefined;
  let requestId = 0;

  function close() {
    palette.close();
  }

  function go(path: string) {
    close();
    goto(path);
  }

  /** Play a searched track in its album context (same as the search page). */
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

  async function savePlaylist() {
    const name = playlistName.trim();
    const trackIds = player.queue.tracks.map((t) => t.itemId);
    if (!name || trackIds.length === 0) return;
    close();
    // Command lands with another slice's merge; failures surface as banner.
    player.run(invoke<string>("create_playlist", { name, trackIds }));
  }

  function sleep(minutes: number | null, endOfTrack: boolean) {
    close();
    player.run(api.setSleepTimer(minutes, endOfTrack));
  }

  const commands = $derived.by((): Entry[] => {
    const queueEmpty = player.queue.tracks.length === 0;
    const current = player.state.current;
    const sleepLabel = (detail: string) => `${m.sleep_timer()}: ${detail}`;
    return [
      { label: m.cmd_play_pause(), icon: I.play, run: () => { close(); player.run(player.toggle()); } },
      { label: m.cmd_next(), icon: I.next, run: () => { close(); player.run(player.next()); } },
      { label: m.cmd_prev(), icon: I.prev, run: () => { close(); player.run(player.prev()); } },
      { label: m.cmd_toggle_queue(), icon: I.queue, run: () => { close(); player.toggleQueuePanel(); } },
      { label: m.cmd_toggle_lyrics(), icon: I.mic, run: () => { close(); player.toggleLyricsPanel(); } },
      { label: m.cmd_open_visualizer(), icon: I.bars, run: () => go("/visualizer") },
      { label: m.cmd_open_fullscreen(), icon: I.expand, run: () => go("/now-playing") },
      { label: m.cmd_go_home(), icon: I.home, run: () => go("/home") },
      { label: m.cmd_go_albums(), icon: I.grid, run: () => go("/") },
      { label: m.cmd_go_artists(), icon: I.person, run: () => go("/artists") },
      { label: m.cmd_go_songs(), icon: I.note, run: () => go("/songs") },
      { label: m.cmd_go_playlists(), icon: I.playlist, run: () => go("/playlists") },
      { label: m.cmd_go_search(), icon: I.search, run: () => go("/search") },
      { label: m.cmd_go_settings(), icon: I.sliders, run: () => go("/settings") },
      {
        label: m.queue_save_playlist(),
        icon: I.save,
        disabled: queueEmpty,
        run: () => {
          naming = true;
          playlistName = "";
        },
      },
      { label: sleepLabel(m.sleep_end_of_track()), icon: I.moon, run: () => sleep(null, true) },
      { label: sleepLabel(m.sleep_minutes({ minutes: 15 })), icon: I.moon, run: () => sleep(15, false) },
      { label: sleepLabel(m.sleep_minutes({ minutes: 30 })), icon: I.moon, run: () => sleep(30, false) },
      { label: sleepLabel(m.sleep_minutes({ minutes: 60 })), icon: I.moon, run: () => sleep(60, false) },
      { label: sleepLabel(m.sleep_off()), icon: I.moon, run: () => sleep(null, false) },
      {
        label: m.queue_jump_to_current(),
        icon: I.target,
        disabled: !current,
        run: () => {
          close();
          player.jumpToCurrent();
        },
      },
      {
        label: m.cmd_instant_mix_current(),
        icon: I.mix,
        disabled: !current,
        run: () => {
          const itemId = current?.itemId;
          close();
          if (itemId) player.run(api.playInstantMix(itemId));
        },
      },
    ].map((c, i) => ({ ...c, id: `cmd-${i}`, group: "commands" as const }));
  });

  /** Every whitespace-separated query word must appear in the label. */
  function matches(label: string, terms: string[]): boolean {
    const lower = label.toLowerCase();
    return terms.every((t) => lower.includes(t));
  }

  const entries = $derived.by((): Entry[] => {
    const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const cmds = terms.length === 0 ? commands : commands.filter((c) => matches(c.label, terms));
    const out = [...cmds];
    if (results && terms.length > 0) {
      for (const album of results.albums.slice(0, 4)) {
        out.push({
          id: `album-${album.id}`,
          group: "results",
          label: album.name,
          sub: album.artist,
          cover: { itemId: album.id, tag: album.imageTag, blurhash: album.imageBlurHash },
          run: () => go(`/album/${album.id}`),
        });
      }
      for (const track of results.tracks.slice(0, 4)) {
        out.push({
          id: `track-${track.id}`,
          group: "results",
          label: track.name,
          sub: `${track.artist} · ${track.album}`,
          cover: { itemId: track.imageItemId, tag: track.imageTag, blurhash: track.imageBlurHash },
          disabled: !track.albumId,
          run: () => {
            close();
            playTrack(track);
          },
        });
      }
      for (const artist of results.artists.slice(0, 3)) {
        out.push({
          id: `artist-${artist.id}`,
          group: "results",
          label: artist.name,
          sub: m.artist_eyebrow(),
          cover: { itemId: artist.id, tag: artist.imageTag, blurhash: artist.imageBlurHash, round: true },
          run: () => go(`/artist/${artist.id}`),
        });
      }
    }
    return out;
  });

  // Debounced search (250 ms) with a stale-response guard.
  $effect(() => {
    const term = query.trim();
    clearTimeout(timer);
    const id = ++requestId;
    if (!term) {
      results = null;
      return;
    }
    timer = setTimeout(async () => {
      try {
        const r = await api.search(term);
        if (id === requestId) results = r;
      } catch {
        if (id === requestId) results = null;
      }
    }, 250);
    return () => clearTimeout(timer);
  });

  // Reset the highlight when the query or search results change. Not keyed on
  // `entries` — that recomputes on every 400 ms player tick and would keep
  // snapping the selection back to the top.
  $effect(() => {
    void query;
    void results;
    activeIndex = 0;
  });

  $effect(() => {
    const el = listEl?.querySelector(`[data-index="${activeIndex}"]`);
    el?.scrollIntoView({ block: "nearest" });
  });

  function move(delta: number) {
    if (entries.length === 0) return;
    let next = activeIndex;
    for (let step = 0; step < entries.length; step++) {
      next = (next + delta + entries.length) % entries.length;
      if (!entries[next].disabled) break;
    }
    activeIndex = next;
  }

  function onKeydown(event: KeyboardEvent) {
    // A key an overlay on top already handled (a confirmation's Escape).
    if (event.defaultPrevented) return;
    if (event.key === "Escape") {
      event.preventDefault();
      if (naming) naming = false;
      else close();
      return;
    }
    if (naming) {
      if (event.key === "Enter") {
        event.preventDefault();
        savePlaylist();
      }
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const entry = entries[activeIndex];
      if (entry && !entry.disabled) entry.run();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div
  class="fixed inset-0 z-40 flex justify-center bg-black/75 pt-[14vh]"
  onclick={(e) => {
    if (e.target === e.currentTarget) close();
  }}
>
  <div
    class="overlay flex h-fit max-h-[62vh] w-full max-w-xl flex-col overflow-hidden rounded-overlay"
    role="dialog"
    aria-modal="true"
  >
    {#if naming}
      <div class="flex items-center gap-3 px-4 py-3">
        <svg viewBox="0 0 24 24" class="h-5 w-5 shrink-0 text-ink-muted" fill="currentColor" aria-hidden="true">
          <path d={I.save} />
        </svg>
        <input
          type="text"
          bind:value={playlistName}
          placeholder={m.palette_playlist_name()}
          class="w-full bg-transparent text-md outline-none placeholder:text-ink-muted"
          {@attach (node: HTMLInputElement) => node.focus()}
        />
      </div>
      <p class="border-t border-edge px-4 py-2.5 text-xs text-ink-muted">
        {m.queue_save_playlist()} · {m.palette_key_enter()} ↵
      </p>
    {:else}
      <div class="flex items-center gap-3 px-4 py-3">
        <svg viewBox="0 0 24 24" class="h-5 w-5 shrink-0 text-ink-muted" fill="currentColor" aria-hidden="true">
          <path d={I.search} />
        </svg>
        <input
          type="text"
          bind:value={query}
          placeholder={m.palette_placeholder()}
          class="w-full bg-transparent text-md outline-none placeholder:text-ink-muted"
          {@attach (node: HTMLInputElement) => node.focus()}
        />
      </div>

      <div bind:this={listEl} class="min-h-0 overflow-y-auto border-t border-edge p-1.5">
        {#if entries.length === 0}
          <p class="px-3 py-4 text-sm text-ink-muted">{m.palette_no_results()}</p>
        {/if}
        {#each entries as entry, i (entry.id)}
          {#if i === 0 && entry.group === "commands"}
            <p class="px-3 pb-1 pt-2 text-[11px] font-bold tracking-wide text-ink-muted uppercase">
              {m.palette_commands()}
            </p>
          {/if}
          {#if entry.group === "results" && (i === 0 || entries[i - 1].group === "commands")}
            <p class="px-3 pb-1 pt-2 text-[11px] font-bold tracking-wide text-ink-muted uppercase">
              {m.palette_results()}
            </p>
          {/if}
          <button
            class="flex w-full items-center gap-3 rounded-md px-3 py-2 text-left text-sm transition-colors
              {i === activeIndex ? 'bg-ink/10' : ''}
              {entry.disabled ? 'opacity-40' : ''}"
            data-index={i}
            disabled={entry.disabled}
            onmousemove={() => {
              if (!entry.disabled) activeIndex = i;
            }}
            onclick={() => {
              if (!entry.disabled) entry.run();
            }}
          >
            {#if entry.cover}
              <div class="w-9 shrink-0">
                <Cover
                  itemId={entry.cover.itemId}
                  tag={entry.cover.tag}
                  size={96}
                  alt=""
                  blurhash={entry.cover.blurhash}
                  round={entry.cover.round ?? false}
                />
              </div>
            {:else if entry.icon}
              <svg viewBox="0 0 24 24" class="h-4.5 w-4.5 shrink-0 text-ink-muted" fill="currentColor" aria-hidden="true">
                <path d={entry.icon} />
              </svg>
            {/if}
            <span class="min-w-0 flex-1">
              <span class="block truncate">{entry.label}</span>
              {#if entry.sub}
                <span class="block truncate text-xs text-ink-muted">{entry.sub}</span>
              {/if}
            </span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>
