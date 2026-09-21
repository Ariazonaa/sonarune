<script lang="ts">
  import { coverUrl } from "$lib/api";

  /**
   * A blurred copy of a cover behind a hero area. Place it as the first child
   * of an `isolate relative overflow-hidden` container: it sits at -z-10 and
   * fills that container. The scrim is made of the base token, so it lightens
   * in light mode and darkens in dark mode — text stays readable over very
   * bright and very dark artwork alike.
   */
  interface Props {
    itemId: string | null;
    tag: string | null;
    /** How strongly the art shows through (0–1). */
    strength?: number;
    /** Off where the container dims the art itself (now playing). */
    scrim?: boolean;
  }

  let { itemId, tag, strength = 0.75, scrim = true }: Props = $props();

  const src = $derived(coverUrl(itemId, tag, 360));
  // Fade in once decoded, instead of popping in (or flashing a broken image).
  let loadedSrc = $state<string | null>(null);
</script>

{#if src}
  <div class="pointer-events-none absolute inset-0 -z-10 overflow-hidden" aria-hidden="true">
    <img
      {src}
      alt=""
      class="h-full w-full scale-125 object-cover blur-3xl saturate-150 transition-opacity duration-700"
      style:opacity={loadedSrc === src ? strength : 0}
      onload={() => (loadedSrc = src)}
    />
    {#if scrim}
      <div class="absolute inset-0 bg-base/40"></div>
      <div class="absolute inset-0 bg-gradient-to-b from-transparent to-base/70"></div>
    {/if}
  </div>
{/if}
