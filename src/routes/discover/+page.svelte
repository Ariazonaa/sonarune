<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import {
    apiDiscovery,
    type DecadeDto,
    type DiscoverData,
    type DiscoverRow,
    type SimilarArtistsRow,
  } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import LibraryAlbumCard from "$lib/components/LibraryAlbumCard.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import DiscoveryArtistCard from "$lib/components/DiscoveryArtistCard.svelte";
  import DiscoveryTrackCard from "$lib/components/DiscoveryTrackCard.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import { library } from "$lib/state/library.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import type { AlbumDto, GenreDto, TrackDto } from "$lib/types";

  let randomAlbums = $state<DiscoverRow<AlbumDto> | null>(
    swrGet<DiscoverRow<AlbumDto>>("discover:random") ?? null,
  );
  let genres = $state<DiscoverRow<GenreDto> | null>(
    swrGet<DiscoverRow<GenreDto>>("discover:genres") ?? null,
  );
  let mixSeeds = $state<DiscoverRow<TrackDto> | null>(
    swrGet<DiscoverRow<TrackDto>>("discover:mix-seeds") ?? null,
  );
  let similarArtists = $state<SimilarArtistsRow | null>(
    swrGet<SimilarArtistsRow>("discover:similar-artists") ?? null,
  );
  let decades = $state<DiscoverRow<DecadeDto> | null>(
    swrGet<DiscoverRow<DecadeDto>>("discover:decades") ?? null,
  );
  let longNotHeard = $state<DiscoverRow<AlbumDto> | null>(
    swrGet<DiscoverRow<AlbumDto>>("discover:long-not-heard") ?? null,
  );
  let generation = 0;

  function preserveStale<T>(current: DiscoverRow<T> | null, incoming: DiscoverRow<T>): DiscoverRow<T> {
    return incoming.error && current?.items.length ? { items: current.items, error: incoming.error } : incoming;
  }

  function preserveSimilar(
    current: SimilarArtistsRow | null,
    incoming: SimilarArtistsRow,
  ): SimilarArtistsRow {
    const merged = preserveStale(current, incoming);
    return {
      seed: incoming.seed ?? current?.seed ?? null,
      items: merged.items,
      error: merged.error,
    };
  }

  function saveRow<T>(key: string, row: DiscoverRow<T>) {
    if (row.items.length > 0 || !row.error) swrSet(key, { ...row, error: null });
  }

  async function load() {
    const seq = ++generation;
    try {
      const data: DiscoverData = await apiDiscovery.getDiscover();
      if (seq !== generation) return;
      randomAlbums = preserveStale(randomAlbums, data.randomAlbums);
      genres = preserveStale(genres, data.genres);
      mixSeeds = preserveStale(mixSeeds, data.instantMixSeeds);
      similarArtists = preserveSimilar(similarArtists, data.similarArtists);
      decades = preserveStale(decades, data.decades);
      longNotHeard = preserveStale(longNotHeard, data.longNotHeard);
      saveRow("discover:random", randomAlbums);
      saveRow("discover:genres", genres);
      saveRow("discover:mix-seeds", mixSeeds);
      saveRow("discover:similar-artists", similarArtists);
      saveRow("discover:decades", decades);
      saveRow("discover:long-not-heard", longNotHeard);
    } catch (cause) {
      if (seq !== generation) return;
      const message = String(cause);
      randomAlbums = { items: randomAlbums?.items ?? [], error: message };
      genres = { items: genres?.items ?? [], error: message };
      mixSeeds = { items: mixSeeds?.items ?? [], error: message };
      similarArtists = {
        seed: similarArtists?.seed ?? null,
        items: similarArtists?.items ?? [],
        error: message,
      };
      decades = { items: decades?.items ?? [], error: message };
      longNotHeard = { items: longNotHeard?.items ?? [], error: message };
    }
  }

  onMount(load);
  $effect(() => {
    if (library.revision > 0) load();
  });

  function rowAlbums(row: DiscoverRow<AlbumDto> | null, shuffle: boolean) {
    const ids = row?.items.map((album) => album.id) ?? [];
    if (ids.length) player.run(apiDiscovery.playAlbums(ids, shuffle));
  }

  function rowTracks(shuffle: boolean) {
    const ids = mixSeeds?.items.map((track) => track.id) ?? [];
    if (ids.length) player.run(apiDiscovery.playTracks(ids, shuffle));
  }

  function firstGenre(shuffle: boolean) {
    const genre = genres?.items[0];
    if (genre) player.run(apiDiscovery.playGenre(genre.id, shuffle));
  }

  function firstDecade(shuffle: boolean) {
    const decade = decades?.items[0];
    if (decade) player.run(apiDiscovery.playDecade(decade.startYear, shuffle));
  }

  function similarMix() {
    const seed = similarArtists?.seed;
    if (seed) player.run(api.playInstantMix(seed.id));
  }

  function hue(value: string): number {
    let result = 0;
    for (let index = 0; index < value.length; index++) {
      result = (result * 31 + value.charCodeAt(index)) % 360;
    }
    return result;
  }
</script>

{#snippet sectionHeader(
  title: string,
  allHref: string,
  enabled: boolean,
  onplay: () => void,
  onshuffle: () => void,
)}
  <div class="mb-3 flex flex-wrap items-center gap-2">
    <h2 class="mr-auto text-lg font-bold tracking-tight">{title}</h2>
    <button
      class="rounded-full px-3 py-1 text-xs font-bold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-40"
      onclick={onplay}
      disabled={!enabled}
    >
      {m.discover_play()}
    </button>
    <button
      class="rounded-full px-3 py-1 text-xs font-bold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-40"
      onclick={onshuffle}
      disabled={!enabled}
    >
      {m.discover_shuffle()}
    </button>
    <a
      href={allHref}
      class="rounded-full border border-edge px-3 py-1 text-xs font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
    >
      {m.discover_all()}
    </a>
  </div>
{/snippet}

{#snippet rowState(row: DiscoverRow<unknown> | null)}
  {#if row?.error}
    <div class="mb-3 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      <span>{m.discover_section_error({ message: row.error })}</span>
      <button class="ml-auto shrink-0 font-bold underline" onclick={load}>{m.discover_retry()}</button>
    </div>
  {/if}
  {#if row && row.items.length === 0 && !row.error}
    <EmptyState icon="discover" title={m.discover_section_empty()} compact />
  {/if}
{/snippet}

{#snippet loadingRow(round = false)}
  <div data-card-row class="flex gap-4 overflow-hidden pb-2">
    {#each Array(6) as _, index (index)}
      <div class="w-44 shrink-0"><CardSkeleton {round} /></div>
    {/each}
  </div>
{/snippet}

<div class="p-6">
  <h1 class="text-2xl font-bold tracking-tight">{m.nav_discover()}</h1>
  <p class="mt-1 mb-7 max-w-2xl text-sm text-ink-muted">{m.discover_subtitle()}</p>

  <section class="mb-8">
    {@render sectionHeader(
      m.discover_random_albums(),
      "/discover/random",
      Boolean(randomAlbums?.items.length),
      () => rowAlbums(randomAlbums, false),
      () => rowAlbums(randomAlbums, true),
    )}
    {@render rowState(randomAlbums)}
    {#if randomAlbums?.items.length}
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each randomAlbums.items as album (album.id)}
          <div class="w-44 shrink-0"><LibraryAlbumCard {album} /></div>
        {/each}
      </div>
    {:else if !randomAlbums}
      {@render loadingRow()}
    {/if}
  </section>

  <section class="mb-8">
    {@render sectionHeader(
      m.discover_genres(),
      "/genres",
      Boolean(genres?.items.length),
      () => firstGenre(false),
      () => firstGenre(true),
    )}
    {@render rowState(genres)}
    {#if genres?.items.length}
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each genres.items as genre (genre.id)}
          <a
            href="/?genre={genre.id}"
            class="relative flex h-28 w-44 shrink-0 items-end overflow-hidden rounded-lg p-3 shadow-lg transition-transform hover:scale-[1.02]"
            style="background: linear-gradient(135deg, hsl({hue(genre.name)} 55% 42%), hsl({(hue(genre.name) + 40) % 360} 55% 28%));"
          >
            <span class="text-md font-bold text-white drop-shadow">{genre.name}</span>
          </a>
        {/each}
      </div>
    {:else if !genres}
      {@render loadingRow()}
    {/if}
  </section>

  <section class="mb-8">
    {@render sectionHeader(
      m.discover_mix_seeds(),
      "/discover/mixes",
      Boolean(mixSeeds?.items.length),
      () => rowTracks(false),
      () => rowTracks(true),
    )}
    {@render rowState(mixSeeds)}
    {#if mixSeeds?.items.length}
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each mixSeeds.items as track (track.id)}
          <div class="w-44 shrink-0"><DiscoveryTrackCard {track} /></div>
        {/each}
      </div>
    {:else if !mixSeeds}
      {@render loadingRow()}
    {/if}
  </section>

  <section class="mb-8">
    {@render sectionHeader(
      similarArtists?.seed
        ? m.discover_similar_to({ name: similarArtists.seed.name })
        : m.discover_similar_artists(),
      "/discover/artists",
      Boolean(similarArtists?.seed),
      similarMix,
      similarMix,
    )}
    {@render rowState(similarArtists)}
    {#if similarArtists?.items.length}
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each similarArtists.items as artist (artist.id)}
          <div class="w-44 shrink-0"><DiscoveryArtistCard {artist} /></div>
        {/each}
      </div>
    {:else if !similarArtists}
      {@render loadingRow(true)}
    {/if}
  </section>

  <section class="mb-8">
    {@render sectionHeader(
      m.discover_decades(),
      "/discover/decades",
      Boolean(decades?.items.length),
      () => firstDecade(false),
      () => firstDecade(true),
    )}
    {@render rowState(decades)}
    {#if decades?.items.length}
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each decades.items as decade (decade.startYear)}
          <a
            href="/discover/decade/{decade.startYear}"
            class="relative flex h-28 w-44 shrink-0 items-end overflow-hidden rounded-lg p-3 shadow-lg transition-transform hover:scale-[1.02]"
            style="background: linear-gradient(135deg, hsl({(decade.startYear * 7) % 360} 58% 43%), hsl({(decade.startYear * 7 + 55) % 360} 62% 25%));"
          >
            <span class="text-xl font-black text-white drop-shadow">{decade.label}</span>
          </a>
        {/each}
      </div>
    {:else if !decades}
      {@render loadingRow()}
    {/if}
  </section>

  <section class="mb-8">
    {@render sectionHeader(
      m.discover_long_not_heard(),
      "/discover/long-not-heard",
      Boolean(longNotHeard?.items.length),
      () => rowAlbums(longNotHeard, false),
      () => rowAlbums(longNotHeard, true),
    )}
    {@render rowState(longNotHeard)}
    {#if longNotHeard?.items.length}
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each longNotHeard.items as album (album.id)}
          <div class="w-44 shrink-0"><LibraryAlbumCard {album} /></div>
        {/each}
      </div>
    {:else if !longNotHeard}
      {@render loadingRow()}
    {/if}
  </section>
</div>
