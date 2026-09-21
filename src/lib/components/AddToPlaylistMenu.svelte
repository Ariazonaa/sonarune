<script lang="ts">
  import { apiLibrary } from "$lib/api/library";
  import { player } from "$lib/state/player.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { m } from "$lib/paraglide/messages";
  import type { PlaylistDto } from "$lib/types";
  import Popover from "./Popover.svelte";

  let {
    x,
    y,
    trackIds,
    onclose,
  }: {
    x: number;
    y: number;
    trackIds: string[];
    onclose: () => void;
  } = $props();

  let playlists = $state<PlaylistDto[] | null>(null);
  let creating = $state(false);
  let newName = $state("");

  $effect(() => {
    apiLibrary
      .getPlaylists()
      .then((p) => (playlists = p))
      .catch((e) => {
        player.error = String(e);
        onclose();
      });
  });

  async function addTo(pl: PlaylistDto) {
    onclose();
    try {
      await apiLibrary.playlistAdd(pl.id, trackIds);
      toast.show(m.add_to_playlist_done({ name: pl.name }));
    } catch (e) {
      player.error = String(e);
    }
  }

  async function createNew() {
    const name = newName.trim();
    if (!name) return;
    onclose();
    try {
      await apiLibrary.createPlaylist(name, trackIds);
      toast.show(m.add_to_playlist_done({ name }));
    } catch (e) {
      player.error = String(e);
    }
  }
</script>

<!-- A menu (keys handled by Popover): "New playlist…" and the playlists are
     its items. The name field that replaces "New playlist…" keeps its own
     keys; Enter creates, Escape and Tab close the menu. -->
<Popover at={{ x, y }} {onclose} label={m.add_to_playlist()} class="flex max-h-80 w-60 flex-col">
  <!-- Visual heading only: the menu's accessible name says the same. -->
  <p class="px-3 pt-1.5 pb-1 text-xs font-semibold uppercase tracking-wider text-ink-muted" aria-hidden="true">
    {m.add_to_playlist()}
  </p>

  {#if creating}
    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="mx-1 mb-1 rounded-md border border-edge bg-base px-2.5 py-1.5 text-sm outline-none focus:border-accent"
      placeholder={m.palette_playlist_name()}
      aria-label={m.palette_playlist_name()}
      bind:value={newName}
      autofocus
      onkeydown={(e) => {
        if (e.key === "Enter") createNew();
      }}
    />
  {:else}
    <button
      class="flex w-full items-center gap-2.5 rounded-md px-3 py-1.5 text-left text-sm text-accent outline-none transition-colors hover:bg-ink/10 focus-visible:bg-ink/10 focus-visible:ring-1 focus-visible:ring-accent focus-visible:ring-inset"
      role="menuitem"
      tabindex="-1"
      onclick={() => (creating = true)}
    >
      <svg viewBox="0 0 24 24" class="h-4 w-4 shrink-0" fill="currentColor" aria-hidden="true">
        <path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z" />
      </svg>
      <span class="truncate">{m.add_to_playlist_new()}</span>
    </button>
  {/if}

  <div class="min-h-0 flex-1 overflow-y-auto" role="none">
    {#each playlists ?? [] as pl (pl.id)}
      <button
        class="flex w-full items-center gap-2.5 rounded-md px-3 py-1.5 text-left text-sm outline-none transition-colors hover:bg-ink/10 focus-visible:bg-ink/10 focus-visible:ring-1 focus-visible:ring-accent focus-visible:ring-inset"
        role="menuitem"
        tabindex="-1"
        onclick={() => addTo(pl)}
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4 shrink-0 text-ink-muted" fill="currentColor" aria-hidden="true">
          <path d="M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zM17 6v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V8h3V6h-5z" />
        </svg>
        <span class="truncate">{pl.name}</span>
      </button>
    {/each}
  </div>
</Popover>
