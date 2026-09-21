<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { apiDiscovery, type SimilarArtistsRow } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import DiscoveryArtistCard from "$lib/components/DiscoveryArtistCard.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";

  let row = $state<SimilarArtistsRow | null>(
    swrGet<SimilarArtistsRow>("discover:all:artists") ?? null,
  );
  let error = $state<string | null>(null);
  let generation = 0;

  async function load() {
    const seq = ++generation;
    try {
      const result = await apiDiscovery.getSimilarArtists(null, 100);
      if (seq !== generation) return;
      row = result;
      error = result.error;
      if (result.items.length || !result.error) swrSet("discover:all:artists", { ...result, error: null });
    } catch (cause) {
      if (seq === generation) error = String(cause);
    }
  }
  onMount(load);
</script>

<div class="p-6">
  <a href="/discover" class="mb-4 inline-flex items-center gap-1 text-sm font-semibold text-ink-muted hover:text-ink">
    <span aria-hidden="true">←</span> {m.discover_back()}
  </a>
  <div class="mb-5 flex flex-wrap items-center gap-3">
    <h1 class="mr-auto text-2xl font-bold tracking-tight">
      {row?.seed ? m.discover_similar_to({ name: row.seed.name }) : m.discover_similar_artists()}
    </h1>
    <button
      class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) hover:bg-accent-hover disabled:opacity-40"
      onclick={() => row?.seed && player.run(api.playInstantMix(row.seed.id))}
      disabled={!row?.seed}
    >{m.discover_shuffle()}</button>
  </div>
  {#if error}
    <div class="mb-4 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      <span>{m.error_generic({ message: error })}</span>
      <button class="ml-auto font-bold underline" onclick={load}>{m.discover_retry()}</button>
    </div>
  {/if}
  {#if row?.items.length}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10rem,1fr))] gap-4">
      {#each row.items as artist (artist.id)}<DiscoveryArtistCard {artist} />{/each}
    </div>
  {:else if !row}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10rem,1fr))] gap-4">
      {#each Array(8) as _, index (index)}<CardSkeleton />{/each}
    </div>
  {:else if !error}
    <EmptyState
      icon="discover"
      title={m.discover_section_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
    />
  {/if}
</div>
