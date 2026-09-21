<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { api } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import SyncStatus from "$lib/components/SyncStatus.svelte";
  import { layoutPreferences } from "$lib/state/layout.svelte";
  import { playlists } from "$lib/state/playlists.svelte";
  import { player } from "$lib/state/player.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { isOurDrag, readDrag } from "$lib/dragToPlaylist";
  import type { PlaylistDto } from "$lib/types";

  const links = [
    {
      href: "/home",
      label: m.nav_home(),
      // house
      icon: "M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8h5z",
    },
    {
      href: "/",
      label: m.nav_albums(),
      // grid
      icon: "M3 3h8v8H3V3zm10 0h8v8h-8V3zM3 13h8v8H3v-8zm10 0h8v8h-8v-8z",
    },
    {
      href: "/recently-added",
      label: m.nav_recently_added(),
      // clock
      icon: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm0 18a8 8 0 1 1 0-16 8 8 0 0 1 0 16zm.5-13H11v6l5.25 3.15.75-1.23-4.5-2.67V7z",
    },
    {
      href: "/discover",
      label: m.nav_discover(),
      // compass
      icon: "M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm3.5 6.5-2.1 4.9-4.9 2.1 2.1-4.9 4.9-2.1zm-3.5 2.4a1.1 1.1 0 1 0 0 2.2 1.1 1.1 0 0 0 0-2.2z",
    },
    {
      href: "/artists",
      label: m.nav_artists(),
      // person
      icon: "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
    },
    {
      href: "/songs",
      label: m.nav_songs(),
      // note
      icon: "M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z",
    },
    {
      href: "/playlists",
      label: m.nav_playlists(),
      // queue-music
      icon: "M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zm14-8v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V10h3V8h-5z",
    },
    {
      href: "/smart",
      label: m.nav_smart_views(),
      // filter spark
      icon: "M3 5h18l-7 8v5l-4 2v-7L3 5zm15.5 9 .75 1.75L21 16.5l-1.75.75L18.5 19l-.75-1.75L16 16.5l1.75-.75.75-1.75z",
    },
    {
      href: "/collections",
      label: m.nav_collections(),
      // stacked albums
      icon: "M4 5h14v14H4V5zm2 2v10h10V7H6zm-4 2h1v12h12v1H2V9zm5 0h8v2H7V9zm0 4h8v2H7v-2z",
    },
    {
      href: "/favorites",
      label: m.nav_favorites(),
      // heart
      icon: "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
    },
    {
      href: "/genres",
      label: m.nav_genres(),
      // label / tag
      icon: "M17.63 5.84C17.27 5.33 16.67 5 16 5L5 5.01C3.9 5.01 3 5.9 3 7v10c0 1.1.9 1.99 2 1.99L16 19c.67 0 1.27-.33 1.63-.84L22 12l-4.37-6.16z",
    },
    {
      href: "/stats",
      label: m.nav_stats(),
      // bar chart
      icon: "M5 9.2h3V19H5V9.2zM10.6 5h2.8v14h-2.8V5zm5.6 8H19v6h-2.8v-6z",
    },
    {
      href: "/search",
      label: m.nav_search(),
      // magnifier
      icon: "M15.5 14h-.79l-.28-.27a6.5 6.5 0 1 0-.7.7l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0A4.5 4.5 0 1 1 14 9.5 4.5 4.5 0 0 1 9.5 14z",
    },
    {
      href: "/settings",
      label: m.nav_settings(),
      // sliders
      icon: "M3 17v2h6v-2H3zM3 5v2h10V5H3zm10 16v-2h8v-2h-8v-2h-2v6h2zM7 9v2H3v2h4v2h2V9H7zm14 4v-2H11v2h10zm-6-4h2V7h4V5h-4V3h-2v6z",
    },
  ];

  onMount(() => playlists.refresh());

  /** The playlist a drag is hovering, so it can light up. */
  let dropTarget = $state<string | null>(null);

  async function onDrop(event: DragEvent, playlist: PlaylistDto) {
    const payload = readDrag(event);
    dropTarget = null;
    if (!payload) return;
    event.preventDefault();
    try {
      // An album arrives as an id: the card that was dragged does not know its
      // tracks, and asking now costs one request instead of one per album
      // dragged past.
      const trackIds =
        payload.trackIds ?? (await api.getAlbum(payload.albumId!)).tracks.map((track) => track.id);
      if (trackIds.length === 0) return;
      await apiLibrary.playlistAdd(playlist.id, trackIds);
      playlists.refresh();
      toast.show(m.add_to_playlist_done({ name: playlist.name }));
    } catch (e) {
      player.error = String(e);
    }
  }
</script>

<aside
  class="surface flex min-h-0 shrink-0 flex-col gap-1 overflow-y-auto rounded-panel bg-panel p-3"
  style="width: {layoutPreferences.active.sidebarWidth}px;"
>
  <div class="mb-4 flex items-center gap-2 px-3 pt-2">
    <div class="flex h-7 w-7 items-center justify-center rounded-full bg-accent">
      <BrandMark class="h-4 w-4 text-(--color-on-accent)" />
    </div>
    <span class="text-lg font-extrabold tracking-tight">{m.app_name()}</span>
  </div>

  {#each links as link (link.href)}
    <a
      href={link.href}
      class="flex items-center gap-4 rounded-md px-3 py-2 text-sm font-bold transition-colors
        {page.url.pathname === link.href
        ? 'text-ink'
        : 'text-ink-muted hover:text-ink'}"
    >
      <svg viewBox="0 0 24 24" class="h-6 w-6" fill="currentColor" aria-hidden="true">
        <path d={link.icon} />
      </svg>
      {link.label}
    </a>
  {/each}

  {#if playlists.items.length > 0}
    <div class="mt-4 border-t border-edge pt-3">
      <a
        href="/playlists"
        class="flex items-center justify-between px-3 pb-1 text-[11px] font-bold uppercase tracking-wider text-ink-muted transition-colors hover:text-ink"
      >
        {m.nav_playlists()}
        <span class="tabular-nums">{playlists.items.length}</span>
      </a>
      {#each playlists.items as playlist (playlist.id)}
        <a
          href="/playlist/{playlist.id}"
          class="block truncate rounded-md px-3 py-1.5 text-sm transition-colors
            {page.url.pathname === `/playlist/${playlist.id}`
            ? 'text-ink'
            : 'text-ink-muted hover:text-ink'}
            {dropTarget === playlist.id ? 'bg-accent text-(--color-on-accent)' : ''}"
          ondragover={(event) => {
            // Only ours: a file or a link dragged in must not look droppable.
            if (!isOurDrag(event)) return;
            event.preventDefault();
            if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
            dropTarget = playlist.id;
          }}
          ondragleave={() => {
            if (dropTarget === playlist.id) dropTarget = null;
          }}
          ondrop={(event) => void onDrop(event, playlist)}
        >
          {playlist.name}
        </a>
      {/each}
    </div>
  {/if}

  <SyncStatus />
</aside>
