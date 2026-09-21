<script lang="ts">
  import ContextMenu from "./ContextMenu.svelte";
  import AddToPlaylistMenu from "./AddToPlaylistMenu.svelte";
  import type { AlbumMenu } from "$lib/state/albumMenu.svelte";

  let { menu }: { menu: AlbumMenu } = $props();
</script>

{#if menu.menu}
  <ContextMenu x={menu.menu.x} y={menu.menu.y} items={menu.menu.items} onclose={() => (menu.menu = null)} />
{/if}
{#if menu.addTo}
  <AddToPlaylistMenu
    x={menu.addTo.x}
    y={menu.addTo.y}
    trackIds={menu.addTo.trackIds}
    onclose={() => (menu.addTo = null)}
  />
{/if}
