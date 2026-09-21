<script lang="ts">
  import { page } from "$app/state";
  import { apiDiscovery, type CollectionDetail } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import LibraryAlbumCard from "$lib/components/LibraryAlbumCard.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { library } from "$lib/state/library.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";

  let detail = $state<CollectionDetail | null>(null);
  let error = $state<string | null>(null);
  let generation = 0;
  const collectionId = $derived(page.params.id!);

  function load(id: string) {
    const seq = ++generation;
    const key = `collection:${id}`;
    detail = swrGet<CollectionDetail>(key) ?? null;
    error = null;
    apiDiscovery
      .getCollection(id)
      .then((result) => {
        if (seq !== generation || id !== page.params.id) return;
        detail = result;
        swrSet(key, result);
      })
      .catch((cause) => {
        if (seq === generation && id === page.params.id && !detail) error = String(cause);
      });
  }

  $effect(() => {
    const id = collectionId;
    void library.revision;
    load(id);
  });
</script>

<div>
  {#if error}
    <div class="m-6 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      <span>{m.error_generic({ message: error })}</span>
      <button class="ml-auto font-bold underline" onclick={() => load(collectionId)}>
        {m.discover_retry()}
      </button>
    </div>
  {:else if detail}
    <header class="flex items-end gap-6 bg-gradient-to-b from-panel-2 to-panel p-6 pt-10">
      <div class="w-48 shrink-0 shadow-2xl">
        <Cover
          itemId={detail.collection.id}
          tag={detail.collection.imageTag ?? null}
          size={480}
          alt={detail.collection.name}
          blurhash={detail.collection.imageBlurHash ?? null}
        />
      </div>
      <div class="min-w-0">
        <div class="flex flex-wrap items-center gap-2">
          <p class="text-xs font-bold uppercase tracking-wider">{m.collection_eyebrow()}</p>
          <span class="rounded-full bg-panel-2 px-2 py-0.5 text-[0.65rem] font-bold uppercase tracking-wide text-ink-muted">
            {m.collection_read_only()}
          </span>
        </div>
        <h1 class="mt-2 truncate text-5xl font-black tracking-tight">{detail.collection.name}</h1>
        <p class="mt-4 text-sm font-medium text-ink-muted">
          {(detail.collection.albumCount === 1 ? m.collections_album_one : m.collections_album_many)({
            count: detail.collection.albumCount,
          })}
        </p>
      </div>
    </header>

    <div class="flex flex-wrap items-center gap-3 px-6 py-4">
      <button
        class="flex h-13 w-13 items-center justify-center rounded-full bg-accent text-(--color-on-accent) shadow-lg transition-all hover:scale-105 hover:bg-accent-hover"
        onclick={() => player.run(apiDiscovery.playCollection(collectionId, false))}
        aria-label={m.album_play()}
        title={m.album_play()}
      >
        <svg viewBox="0 0 24 24" class="h-6 w-6" fill="currentColor" aria-hidden="true">
          <path d="M8 5v14l11-7L8 5z" />
        </svg>
      </button>
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={() => player.run(apiDiscovery.playCollection(collectionId, true))}
      >
        {m.discover_shuffle()}
      </button>
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={() => player.run(apiDiscovery.enqueueCollection(collectionId, true))}
      >
        {m.album_play_next()}
      </button>
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={() => player.run(apiDiscovery.enqueueCollection(collectionId, false))}
      >
        {m.album_add_to_queue()}
      </button>
    </div>

    <section class="px-6 pb-6">
      <h2 class="mb-3 text-lg font-bold tracking-tight">{m.collection_albums()}</h2>
      {#if detail.albums.length > 0}
        <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
          {#each detail.albums as album (album.id)}<LibraryAlbumCard {album} />{/each}
        </div>
      {:else}
        <EmptyState
          icon="collection"
          title={m.collection_empty()}
          action={{ label: m.nav_collections(), href: "/collections" }}
          compact
        />
      {/if}
    </section>
  {:else}
    <div class="p-6">
      <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
        {#each Array(8) as _, index (index)}<CardSkeleton />{/each}
      </div>
    </div>
  {/if}
</div>
