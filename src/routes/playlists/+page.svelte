<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import ContextMenu, { type ContextMenuItem } from "$lib/components/ContextMenu.svelte";
  import { contextMenuKey } from "$lib/menu";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { library } from "$lib/state/library.svelte";
  import { player } from "$lib/state/player.svelte";
  // Aliased: this file has its own `playlists` — the list it renders.
  import { playlists as playlistStore } from "$lib/state/playlists.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { confirm } from "$lib/state/confirm.svelte";
  import type { PlaylistDto } from "$lib/types";

  let playlists = $state<PlaylistDto[] | null>(null);
  let error = $state<string | null>(null);
  let creating = $state(false);
  let newName = $state("");
  let menu = $state<{ x: number; y: number; items: ContextMenuItem[] } | null>(null);

  function openMenu(e: MouseEvent, playlist: PlaylistDto) {
    e.preventDefault();
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        {
          label: m.album_play_now(),
          icon: "M8 5v14l11-7L8 5z",
          action: () => player.run(apiLibrary.playPlaylist(playlist.id, 0)),
        },
        {
          label: m.playlist_delete(),
          icon: "M6 7h12l-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 7zm3-3h6l1 2H8l1-2z",
          action: async () => {
            const ok = await confirm.ask({
              title: m.delete_confirm_title(),
              body: m.playlist_delete_confirm({ name: playlist.name }),
              confirmLabel: m.playlist_delete(),
              danger: true,
            });
            if (!ok) return;
            apiLibrary
              .deletePlaylist(playlist.id)
              .then(() => playlistStore.refresh())
              .then(() => {
                toast.show(m.playlist_deleted());
                load();
              })
              .catch((err) => (error = String(err)));
          },
        },
      ],
    };
  }

  function load() {
    apiLibrary
      .getPlaylists()
      .then((p) => (playlists = p))
      .catch((e) => (error = String(e)));
  }

  onMount(load);

  // Refetch when the server library changes (a playlist may have been
  // created/removed server-side).
  $effect(() => {
    if (library.revision > 0) load();
  });

  async function createPlaylist() {
    const name = newName.trim();
    if (!name) return;
    creating = false;
    newName = "";
    try {
      const id = await apiLibrary.createPlaylist(name, []);
      playlistStore.refresh();
      toast.show(m.playlist_created());
      goto(`/playlist/${id}`);
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="p-6">
  <div class="mb-5 flex items-center justify-between gap-4">
    <h1 class="text-2xl font-bold tracking-tight">{m.nav_playlists()}</h1>
    {#if creating}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="w-56 rounded-full border border-edge bg-panel-2 px-4 py-1.5 text-sm outline-none focus:border-accent"
        placeholder={m.palette_playlist_name()}
        bind:value={newName}
        autofocus
        onblur={() => (creating = false)}
        onkeydown={(e) => {
          if (e.key === "Enter") createPlaylist();
          else if (e.key === "Escape") creating = false;
        }}
      />
    {:else}
      <div class="flex shrink-0 items-center gap-2">
        <button
          class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover"
          onclick={() => (creating = true)}
        >
          {m.playlist_new()}
        </button>
      </div>
    {/if}
  </div>

  {#if error}
    <p class="mb-4 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if playlists?.length === 0}
    <EmptyState
      icon="playlist"
      title={m.playlists_empty()}
      action={{ label: m.playlist_new(), onclick: () => (creating = true) }}
    />
  {:else if playlists}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(170px,1fr))] gap-4">
      {#each playlists as playlist (playlist.id)}
        <a
          href="/playlist/{playlist.id}"
          oncontextmenu={(e) => openMenu(e, playlist)}
          {@attach contextMenuKey}
          class="group rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2"
        >
          <div class="overflow-hidden rounded-md shadow-lg">
            <Cover
              itemId={playlist.id}
              tag={playlist.imageTag}
              size={360}
              alt={playlist.name}
              blurhash={playlist.imageBlurHash}
            />
          </div>
          <p class="mt-3 truncate text-sm font-semibold">{playlist.name}</p>
          <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
            {(playlist.trackCount === 1 ? m.playlist_tracks_one : m.playlist_tracks_many)({
              count: playlist.trackCount,
            })}
          </p>
        </a>
      {/each}
    </div>
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}
