<script lang="ts">
  import { untrack } from "svelte";
  import { apiDiscovery } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import LibraryAlbumCard from "$lib/components/LibraryAlbumCard.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { library } from "$lib/state/library.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import type { AlbumDto, Page } from "$lib/types";

  let {
    title,
    kind,
    startYear = null,
  }: {
    title: string;
    kind: "random" | "long" | "decade";
    startYear?: number | null;
  } = $props();

  const PAGE_SIZE = 60;
  const cacheKey = $derived(`discover:all:${kind}:${startYear ?? ""}`);
  let albums = $state<AlbumDto[]>([]);
  let total = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let generation = 0;

  const hasMore = $derived(total === null || albums.length < total);

  function request(startIndex: number): Promise<Page<AlbumDto>> {
    if (kind === "random") return apiDiscovery.getRandomAlbums(startIndex, PAGE_SIZE);
    if (kind === "long") return apiDiscovery.getLongNotHeard(startIndex, PAGE_SIZE);
    return apiDiscovery.getDecadeAlbums(startYear ?? 0, startIndex, PAGE_SIZE);
  }

  async function loadMore(reset = false) {
    if (loading && !reset) return;
    const seq = reset ? ++generation : generation;
    if (reset) {
      const cached = swrGet<Page<AlbumDto>>(cacheKey);
      albums = cached?.items ?? [];
      total = cached?.total ?? null;
    }
    loading = true;
    error = null;
    try {
      const result = await request(reset ? 0 : albums.length);
      if (seq !== generation) return;
      if (reset) {
        albums = result.items;
        swrSet(cacheKey, result);
      } else {
        const known = new Set(albums.map((album) => album.id));
        albums = [...albums, ...result.items.filter((album) => !known.has(album.id))];
      }
      total = result.total;
    } catch (cause) {
      if (seq === generation) error = String(cause);
    } finally {
      if (seq === generation) loading = false;
    }
  }

  function play(shuffle: boolean) {
    if (kind === "decade" && startYear !== null) {
      player.run(apiDiscovery.playDecade(startYear, shuffle));
    } else {
      player.run(apiDiscovery.playAlbums(albums.map((album) => album.id), shuffle));
    }
  }

  // Reload on mount, whenever the props change (kind/startYear via cacheKey — a
  // same-route decade→decade navigation reuses this component instance and only
  // updates props), and whenever the library changes. Reading cacheKey and
  // library.revision here makes them the only dependencies; the fetch runs
  // untracked so its own state writes can't re-trigger this effect.
  $effect(() => {
    cacheKey;
    library.revision;
    untrack(() => loadMore(true));
  });
</script>

<div class="p-6">
  <a href="/discover" class="mb-4 inline-flex items-center gap-1 text-sm font-semibold text-ink-muted hover:text-ink">
    <span aria-hidden="true">←</span> {m.discover_back()}
  </a>
  <div class="mb-5 flex flex-wrap items-center gap-3">
    <h1 class="mr-auto text-2xl font-bold tracking-tight">{title}</h1>
    <button
      class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
      onclick={() => play(false)}
      disabled={albums.length === 0}
    >
      {m.discover_play()}
    </button>
    <button
      class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-40"
      onclick={() => play(true)}
      disabled={albums.length === 0}
    >
      {m.discover_shuffle()}
    </button>
  </div>

  {#if error}
    <div class="mb-4 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      <span>{m.error_generic({ message: error })}</span>
      <button class="ml-auto font-bold underline" onclick={() => loadMore(albums.length === 0)}>
        {m.discover_retry()}
      </button>
    </div>
  {/if}

  {#if albums.length > 0}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
      {#each albums as album (album.id)}
        <LibraryAlbumCard {album} />
      {/each}
    </div>
  {:else if loading}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
      {#each Array(8) as _, index (index)}
        <CardSkeleton />
      {/each}
    </div>
  {:else if !error}
    <EmptyState
      icon="discover"
      title={m.discover_section_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
    />
  {/if}

  {#if hasMore && albums.length > 0}
    <div class="mt-6 flex justify-center">
      <button
        class="rounded-full border border-edge px-5 py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
        onclick={() => loadMore(false)}
        disabled={loading}
      >
        {loading ? m.discover_loading() : m.discover_load_more()}
      </button>
    </div>
  {/if}
</div>
