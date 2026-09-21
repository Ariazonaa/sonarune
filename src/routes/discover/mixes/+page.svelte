<script lang="ts">
  import { onMount } from "svelte";
  import { apiDiscovery } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import DiscoveryTrackCard from "$lib/components/DiscoveryTrackCard.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import type { Page, TrackDto } from "$lib/types";

  const PAGE_SIZE = 60;
  let tracks = $state<TrackDto[]>(swrGet<Page<TrackDto>>("discover:all:mixes")?.items ?? []);
  let total = $state<number | null>(swrGet<Page<TrackDto>>("discover:all:mixes")?.total ?? null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let generation = 0;
  const hasMore = $derived(total === null || tracks.length < total);

  async function loadMore(reset = false) {
    if (loading && !reset) return;
    const seq = reset ? ++generation : generation;
    loading = true;
    if (reset) error = null;
    try {
      const result = await apiDiscovery.getMixSeeds(reset ? 0 : tracks.length, PAGE_SIZE);
      if (seq !== generation) return;
      if (reset) {
        tracks = result.items;
        swrSet("discover:all:mixes", result);
      } else {
        const known = new Set(tracks.map((track) => track.id));
        tracks = [...tracks, ...result.items.filter((track) => !known.has(track.id))];
      }
      total = result.total;
    } catch (cause) {
      if (seq === generation) error = String(cause);
    } finally {
      if (seq === generation) loading = false;
    }
  }

  onMount(() => loadMore(true));
</script>

<div class="p-6">
  <a href="/discover" class="mb-4 inline-flex items-center gap-1 text-sm font-semibold text-ink-muted hover:text-ink">
    <span aria-hidden="true">←</span> {m.discover_back()}
  </a>
  <div class="mb-5 flex flex-wrap items-center gap-3">
    <h1 class="mr-auto text-2xl font-bold tracking-tight">{m.discover_mix_seeds()}</h1>
    <button
      class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted hover:text-ink disabled:opacity-40"
      onclick={() => player.run(apiDiscovery.playTracks(tracks.map((track) => track.id), false))}
      disabled={tracks.length === 0}
    >{m.discover_play()}</button>
    <button
      class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) hover:bg-accent-hover disabled:opacity-40"
      onclick={() => player.run(apiDiscovery.playTracks(tracks.map((track) => track.id), true))}
      disabled={tracks.length === 0}
    >{m.discover_shuffle()}</button>
  </div>
  {#if error}
    <p class="mb-4 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}
  {#if tracks.length}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
      {#each tracks as track (track.id)}<DiscoveryTrackCard {track} />{/each}
    </div>
  {:else if loading}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
      {#each Array(8) as _, index (index)}<CardSkeleton />{/each}
    </div>
  {:else if !error}
    <EmptyState
      icon="discover"
      title={m.discover_section_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
    />
  {/if}
  {#if hasMore && tracks.length}
    <div class="mt-6 flex justify-center">
      <button
        class="rounded-full border border-edge px-5 py-2 text-sm font-bold text-ink-muted hover:text-ink disabled:opacity-40"
        onclick={() => loadMore(false)}
        disabled={loading}
      >{loading ? m.discover_loading() : m.discover_load_more()}</button>
    </div>
  {/if}
</div>
