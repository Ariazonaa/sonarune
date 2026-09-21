<script lang="ts">
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import { player } from "$lib/state/player.svelte";
  import type { AlbumDto } from "$lib/types";

  let { album }: { album: AlbumDto } = $props();
</script>

<a
  href="/album/{album.id}"
  class="group block min-w-0 rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2"
>
  <div class="relative">
    <div class="overflow-hidden rounded-md shadow-lg">
      <Cover
        itemId={album.id}
        tag={album.imageTag}
        size={360}
        alt={album.name}
        blurhash={album.imageBlurHash}
      />
    </div>
    <button
      class="absolute right-2 bottom-2 flex h-10 w-10 translate-y-2 items-center justify-center rounded-full bg-accent text-(--color-on-accent) opacity-0 shadow-xl transition-all duration-200 group-hover:translate-y-0 group-hover:opacity-100 hover:scale-105 hover:bg-accent-hover focus-visible:translate-y-0 focus-visible:opacity-100"
      onclick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        player.run(api.playAlbum(album.id, 0));
      }}
      aria-label={m.album_play()}
      title={m.album_play()}
    >
      <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
        <path d="M8 5v14l11-7L8 5z" />
      </svg>
    </button>
  </div>
  <p class="mt-3 truncate text-sm font-semibold">{album.name}</p>
  <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
    {album.artist}{album.year ? ` · ${album.year}` : ""}
  </p>
</a>
