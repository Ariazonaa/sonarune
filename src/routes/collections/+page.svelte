<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { apiDiscovery, type CollectionDto } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { library } from "$lib/state/library.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import type { Page } from "$lib/types";

  const PAGE_SIZE = 100;
  const cached = swrGet<Page<CollectionDto>>("collections");
  let collections = $state<CollectionDto[]>(cached?.items ?? []);
  let total = $state<number | null>(cached?.total ?? null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let generation = 0;

  const hasMore = $derived(total === null || collections.length < total);

  async function loadMore(reset = false) {
    if (loading && !reset) return;
    const seq = reset ? ++generation : generation;
    loading = true;
    if (reset) error = null;
    try {
      const result = await apiDiscovery.getCollections(reset ? 0 : collections.length, PAGE_SIZE);
      if (seq !== generation) return;
      if (reset) {
        collections = result.items;
        swrSet("collections", result);
      } else {
        const known = new Set(collections.map((collection) => collection.id));
        collections = [
          ...collections,
          ...result.items.filter((collection) => !known.has(collection.id)),
        ];
      }
      total = result.total;
    } catch (cause) {
      if (seq === generation && (!reset || collections.length === 0)) error = String(cause);
    } finally {
      if (seq === generation) loading = false;
    }
  }

  onMount(() => loadMore(true));
  // Only `library.revision` is a dependency: the reload reads and writes
  // `loading`, which would otherwise re-trigger this effect forever.
  $effect(() => {
    if (library.revision > 0) untrack(() => loadMore(true));
  });
</script>

<div class="p-6">
  <div class="mb-5">
    <div class="flex items-center gap-3">
      <h1 class="text-2xl font-bold tracking-tight">{m.nav_collections()}</h1>
      <span class="rounded-full bg-panel-2 px-2 py-0.5 text-[0.65rem] font-bold uppercase tracking-wide text-ink-muted">
        {m.collection_read_only()}
      </span>
    </div>
    <p class="mt-1 max-w-2xl text-sm text-ink-muted">{m.collections_subtitle()}</p>
  </div>

  {#if error}
    <div class="mb-4 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      <span>{m.error_generic({ message: error })}</span>
      <button class="ml-auto font-bold underline" onclick={() => loadMore(collections.length === 0)}>
        {m.discover_retry()}
      </button>
    </div>
  {/if}

  {#if collections.length > 0}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] gap-4">
      {#each collections as collection (collection.id)}
        <article class="group min-w-0 rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2">
          <div class="relative">
            <a href="/collection/{collection.id}" class="block overflow-hidden rounded-md shadow-lg">
                <Cover
                  itemId={collection.id}
                  tag={collection.imageTag ?? null}
                  size={360}
                  alt={collection.name}
                  blurhash={collection.imageBlurHash ?? null}
                />
            </a>
            <div class="absolute right-2 bottom-2 flex translate-y-2 gap-1.5 opacity-0 transition-all group-hover:translate-y-0 group-hover:opacity-100 group-focus-within:translate-y-0 group-focus-within:opacity-100">
              <button
                class="flex h-10 w-10 items-center justify-center rounded-full bg-panel-2/95 text-ink shadow-xl hover:text-accent"
                onclick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  player.run(apiDiscovery.playCollection(collection.id, true));
                }}
                aria-label={m.discover_shuffle()}
                title={m.discover_shuffle()}
              >
                <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                  <path d="M10.59 9.17 5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z" />
                </svg>
              </button>
              <button
                class="flex h-10 w-10 items-center justify-center rounded-full bg-accent text-(--color-on-accent) shadow-xl hover:bg-accent-hover"
                onclick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  player.run(apiDiscovery.playCollection(collection.id, false));
                }}
                aria-label={m.album_play()}
                title={m.album_play()}
              >
                <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
                  <path d="M8 5v14l11-7L8 5z" />
                </svg>
              </button>
            </div>
          </div>
          <a href="/collection/{collection.id}" class="mt-3 block truncate text-sm font-semibold hover:underline">
            {collection.name}
          </a>
          <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
            {(collection.albumCount === 1 ? m.collections_album_one : m.collections_album_many)({
              count: collection.albumCount,
            })}
          </p>
        </article>
      {/each}
    </div>
  {:else if loading}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] gap-4">
      {#each Array(8) as _, index (index)}<CardSkeleton />{/each}
    </div>
  {:else if !error}
    <EmptyState icon="collection" title={m.collections_empty()} hint={m.collections_empty_hint()} />
  {/if}

  {#if hasMore && collections.length > 0}
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
