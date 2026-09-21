<script lang="ts">
  import { coverUrl } from "$lib/api";
  import { blurhashToDataUrl } from "$lib/blurhash";

  let {
    itemId,
    tag,
    size = 300,
    alt = "",
    blurhash = null,
    round = false,
    class: className = "",
  }: {
    itemId: string | null;
    tag: string | null;
    size?: number;
    alt?: string;
    blurhash?: string | null;
    round?: boolean;
    class?: string;
  } = $props();

  const url = $derived(coverUrl(itemId, tag, size));
  const placeholder = $derived(blurhash ? blurhashToDataUrl(blurhash) : null);
  const shape = $derived(round ? "rounded-full" : "rounded-md");
  let loaded = $state(false);

  // A long-lived Cover (player bar, now-playing, mini) changes props per track;
  // reset so the blurhash shows again instead of the previous cover lingering.
  $effect(() => {
    void url;
    loaded = false;
  });
</script>

{#if url}
  <div class="relative aspect-square w-full overflow-hidden {shape} bg-panel-2 {className}">
    {#if placeholder && !loaded}
      <img
        src={placeholder}
        alt=""
        aria-hidden="true"
        draggable="false"
        class="absolute inset-0 h-full w-full object-cover"
      />
    {/if}
    <img
      src={url}
      {alt}
      loading="lazy"
      draggable="false"
      onload={() => (loaded = true)}
      class="relative h-full w-full object-cover transition-opacity duration-300 {loaded
        ? 'opacity-100'
        : 'opacity-0'}"
    />
  </div>
{:else}
  <div class="aspect-square w-full {shape} bg-panel-2 {className} flex items-center justify-center text-ink-muted">
    <svg viewBox="0 0 24 24" class="h-1/3 w-1/3" fill="currentColor" aria-hidden="true">
      <path d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z" />
    </svg>
  </div>
{/if}
