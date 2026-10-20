<script lang="ts">
  import { goto } from "$app/navigation";
  import type { ArtistRef } from "#lib/types.js";

  // Renders artist names as links to /artist/{id}; falls back to the plain
  // display string when the server didn't supply linkable artist ids.
  let {
    artists,
    fallback,
    class: cls = "",
  }: { artists: ArtistRef[]; fallback: string; class?: string } = $props();

  // Artist links often sit inside a clickable row — the click must not reach
  // the row. Stopping it also hides it from SvelteKit's router, which listens
  // on <html>, and the WebView would then load the link as a new document:
  // the whole app restarts (session restore, startup view, every panel and
  // cache gone). So a plain click navigates here, the way the router would;
  // a modified one keeps the WebView's default, as for any other link.
  function follow(event: MouseEvent, href: string) {
    event.stopPropagation();
    if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    void goto(href);
  }
</script>

{#if artists.length > 0}{#each artists as a, i (a.id)}<a
      href="/artist/{a.id}"
      class="hover:underline {cls}"
      onclick={(e) => follow(e, `/artist/${a.id}`)}>{a.name}</a>{i < artists.length - 1 ? ", " : ""}{/each}{:else}<span class={cls}>{fallback}</span>{/if}
