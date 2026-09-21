<script lang="ts">
  import type { ArtistRef } from "$lib/types";

  // Renders artist names as links to /artist/{id}; falls back to the plain
  // display string when the server didn't supply linkable artist ids.
  let {
    artists,
    fallback,
    class: cls = "",
  }: { artists: ArtistRef[]; fallback: string; class?: string } = $props();

  // Artist links often sit inside a clickable row — don't trigger the row.
  const stop = (e: MouseEvent) => e.stopPropagation();
</script>

{#if artists.length > 0}{#each artists as a, i (a.id)}<a
      href="/artist/{a.id}"
      class="hover:underline {cls}"
      onclick={stop}>{a.name}</a>{i < artists.length - 1 ? ", " : ""}{/each}{:else}<span class={cls}>{fallback}</span>{/if}
