<script lang="ts">
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import { player } from "$lib/state/player.svelte";
  import type { TrackDto } from "$lib/types";

  let { track }: { track: TrackDto } = $props();
</script>

<article class="group min-w-0 rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2">
  <div class="relative">
    <div class="overflow-hidden rounded-md shadow-lg">
      <Cover
        itemId={track.imageItemId}
        tag={track.imageTag}
        size={360}
        alt={track.album || track.name}
        blurhash={track.imageBlurHash}
      />
    </div>
    <button
      class="absolute right-2 bottom-2 flex h-10 w-10 translate-y-2 items-center justify-center rounded-full bg-accent text-(--color-on-accent) opacity-0 shadow-xl transition-all group-hover:translate-y-0 group-hover:opacity-100 focus-visible:translate-y-0 focus-visible:opacity-100"
      onclick={() => player.run(api.playInstantMix(track.id))}
      aria-label={m.album_instant_mix()}
      title={m.album_instant_mix()}
    >
      <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
        <path d="M10.59 9.17 5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z" />
      </svg>
    </button>
  </div>
  <p class="mt-3 truncate text-sm font-semibold">{track.name}</p>
  <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
    {track.artist}{track.album ? ` · ${track.album}` : ""}
  </p>
</article>
