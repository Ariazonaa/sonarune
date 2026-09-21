<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { library } from "$lib/state/library.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import type { GenreDto } from "$lib/types";

  let genres = $state<GenreDto[] | null>(swrGet<GenreDto[]>("genres") ?? null);
  let error = $state<string | null>(null);

  function load() {
    api
      .getGenres()
      .then((g) => {
        swrSet("genres", g);
        genres = g;
        error = null;
      })
      .catch((e) => {
        if (!genres) error = String(e);
      });
  }

  onMount(load);
  $effect(() => {
    if (library.revision > 0) load();
  });

  // Deterministic hue from the genre name so cards are varied but stable.
  function hue(name: string): number {
    let h = 0;
    for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) % 360;
    return h;
  }
</script>

<div class="p-6">
  <h1 class="mb-5 text-2xl font-bold tracking-tight">{m.nav_genres()}</h1>

  {#if error}
    <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if genres?.length === 0}
    <EmptyState icon="genre" title={m.genres_empty()} hint={m.empty_library_hint()} />
  {:else if genres}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] gap-4">
      {#each genres as genre (genre.id)}
        <a
          href="/?genre={genre.id}"
          class="relative flex h-28 items-end overflow-hidden rounded-lg p-3 shadow-lg transition-transform hover:scale-[1.02]"
          style="background: linear-gradient(135deg, hsl({hue(genre.name)} 55% 42%), hsl({(hue(genre.name) + 40) % 360} 55% 28%));"
        >
          <span class="text-md font-bold text-white drop-shadow">{genre.name}</span>
        </a>
      {/each}
    </div>
  {/if}
</div>
