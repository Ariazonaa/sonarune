<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import AlbumContextMenu from "$lib/components/AlbumContextMenu.svelte";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { createAlbumMenu } from "$lib/state/albumMenu.svelte";
  import { contextMenuKey } from "$lib/menu";
  import type { AlbumDto } from "$lib/types";

  const am = createAlbumMenu();
  const PAGE = 60;
  const LOAD_AHEAD_PX = 800;

  let albums = $state<AlbumDto[]>([]);
  let total = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let scroller = $state<HTMLElement | null>(null);

  const hasMore = $derived(total === null || albums.length < total);
  /** Bumped on reset so a page fetch still in flight discards itself. */
  let requestSeq = 0;

  /** Fetch the next page; true when it added albums. */
  async function loadMore(): Promise<boolean> {
    if (loading || !hasMore) return false;
    loading = true;
    const seq = requestSeq;
    error = null;
    try {
      const page = await api.getAlbums(albums.length, PAGE, null, "dateAdded", false);
      if (seq !== requestSeq) return false; // reset happened mid-flight
      const seen = new Set(albums.map((a) => a.id));
      const fresh = page.items.filter((i) => !seen.has(i.id));
      albums = [...albums, ...fresh];
      total = page.total;
      // A page of nothing new would request the same offset forever.
      return fresh.length > 0;
    } catch (e) {
      if (seq === requestSeq) error = String(e);
      return false;
    } finally {
      if (seq === requestSeq) loading = false;
    }
  }

  /** Load a page, then keep going while the grid still doesn't reach past
   *  the viewport: a first page that fits a tall or wide window never
   *  scrolls, so `onscroll` alone would never ask for the second. */
  function loadAndFill() {
    // Only chain on success — a failing server must not retry in a loop.
    loadMore().then(async (ok) => {
      if (!ok) return;
      await tick(); // measure with the new cards laid out
      maybeLoadMore();
    });
  }

  function maybeLoadMore() {
    if (!scroller || loading || !hasMore) return;
    const remaining = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    if (remaining < LOAD_AHEAD_PX) loadAndFill();
  }

  // Reset to the top and reload the first page. `requestSeq` invalidates any
  // page still in flight; clearing `loading` lets the fresh load through.
  function reload() {
    requestSeq++;
    albums = [];
    total = null;
    loading = false;
    loadAndFill();
  }

  onMount(loadAndFill);

  // New albums land at the top — reload when the library changes. Only
  // `library.revision` is a dependency (the reset reads are untracked, so a
  // loading/total change can't re-trigger this effect).
  $effect(() => {
    const rev = library.revision;
    untrack(() => {
      if (rev > 0) reload();
    });
  });
</script>

<!-- A larger window can leave the grid short of the viewport again. -->
<svelte:window onresize={maybeLoadMore} />

<div class="flex h-full min-h-0 flex-col p-6 pb-0">
  <h1 class="mb-5 shrink-0 text-2xl font-bold tracking-tight">{m.nav_recently_added()}</h1>

  {#if error}
    <p class="mb-4 shrink-0 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}

  <div bind:this={scroller} class="min-h-0 flex-1 overflow-y-auto" onscroll={maybeLoadMore}>
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4 pb-6">
      {#each albums as album (album.id)}
        <a
          href="/album/{album.id}"
          oncontextmenu={(e) => am.open(e, album)}
          {@attach contextMenuKey}
          class="group rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2"
        >
          <div class="relative">
            <div class="overflow-hidden rounded-md shadow-lg">
              <Cover itemId={album.id} tag={album.imageTag} size={360} alt={album.name} blurhash={album.imageBlurHash} />
            </div>
            <button
              class="absolute right-2 bottom-2 flex h-11 w-11 translate-y-2 items-center justify-center rounded-full bg-accent text-(--color-on-accent) opacity-0 shadow-xl transition-all duration-200 group-hover:translate-y-0 group-hover:opacity-100 hover:scale-105 hover:bg-accent-hover"
              onclick={(e) => {
                e.preventDefault();
                e.stopPropagation();
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
      {/each}
      {#if loading && albums.length === 0}
        {#each Array(12) as _, i (i)}
          <CardSkeleton />
        {/each}
      {/if}
    </div>
  </div>
</div>

<AlbumContextMenu menu={am} />
