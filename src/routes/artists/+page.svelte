<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { VList } from "virtua/svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import AlphabetRail from "$lib/components/AlphabetRail.svelte";
  import Cover from "$lib/components/Cover.svelte";
  import NewContentPill from "$lib/components/NewContentPill.svelte";
  import ContextMenu, { type ContextMenuItem } from "$lib/components/ContextMenu.svelte";
  import { contextMenuKey } from "$lib/menu";
  import { apiLibrary } from "$lib/api/library";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { layoutPreferences } from "$lib/state/layout.svelte";
  import type { ArtistDto } from "$lib/types";

  let artistMenu = $state<{ x: number; y: number; items: ContextMenuItem[] } | null>(null);
  function openArtistMenu(e: MouseEvent, artist: ArtistDto) {
    e.preventDefault();
    artistMenu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        {
          label: m.artist_radio(),
          icon: "M12 2a10 10 0 0 0-10 10v8a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-8A10 10 0 0 0 12 2zm0 3a7 7 0 0 1 6.32 4H5.68A7 7 0 0 1 12 5zm0 13a3 3 0 1 1 0-6 3 3 0 0 1 0 6z",
          action: () => player.run(api.playInstantMix(artist.id)),
        },
        {
          label: m.favorite_add(),
          icon: "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
          action: () =>
            apiLibrary
              .setFavorite(artist.id, true)
              .then(() => toast.show(m.favorite_added()))
              .catch((err) => (player.error = String(err))),
        },
      ],
    };
  }

  const PAGE_SIZE = 100;
  const LOAD_AHEAD_PX = 800;

  let artists = $state<ArtistDto[]>([]);
  let total = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let gridWidth = $state(0);
  let list = $state<VList<ArtistDto[]> | null>(null);
  let staleHint = $state(false);
  /** Bumped on a library refresh so an in-flight page discards itself. */
  let reloadSeq = 0;

  const hasMore = $derived(total === null || artists.length < total);
  const cols = $derived(
    Math.max(2, Math.floor(gridWidth / layoutPreferences.cardPixels.stride)),
  );
  const rows = $derived.by(() => {
    const chunked: ArtistDto[][] = [];
    for (let i = 0; i < artists.length; i += cols) {
      chunked.push(artists.slice(i, i + cols));
    }
    return chunked;
  });

  async function loadMore(): Promise<boolean> {
    if (loading || !hasMore) return false;
    const seq = reloadSeq;
    loading = true;
    error = null;
    try {
      const page = await api.getArtists(artists.length, PAGE_SIZE);
      if (seq !== reloadSeq) return false; // a library refresh superseded this
      // Items can shift between page fetches; duplicate keys would crash the
      // keyed list.
      const seen = new Set(artists.map((a) => a.id));
      artists = [...artists, ...page.items.filter((i) => !seen.has(i.id))];
      total = page.total;
      return true;
    } catch (e) {
      if (seq === reloadSeq) error = String(e);
      return false;
    } finally {
      if (seq === reloadSeq) loading = false;
    }
  }

  function maybeLoadMore() {
    if (!list || loading || !hasMore) return;
    const remaining = list.getScrollSize() - list.getScrollOffset() - list.getViewportSize();
    if (remaining < LOAD_AHEAD_PX) {
      // Only chain on success — a failing server must not retry in a loop.
      loadMore().then((ok) => {
        if (ok) maybeLoadMore();
      });
    }
  }

  async function jumpToLetter(letter: string) {
    try {
      const index = letter === "#" ? 0 : await api.letterIndex("artists", letter);
      // Page in until the target row exists; bail when a fetch fails or the
      // list stops growing (end reached / concurrent load in progress).
      while (artists.length <= index && hasMore) {
        const before = artists.length;
        if (!(await loadMore()) || artists.length === before) break;
      }
      list?.scrollToIndex(Math.floor(Math.min(index, Math.max(artists.length - 1, 0)) / cols), {
        align: "start",
      });
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    loadMore().then(() => maybeLoadMore());
  });

  function refresh() {
    staleHint = false;
    reloadSeq++;
    artists = [];
    total = null;
    loading = false;
    list?.scrollTo(0);
    loadMore().then((ok) => ok && maybeLoadMore());
  }

  // Reload live near the top, else offer a refresh pill. Only
  // `library.revision` is tracked; the reload's state reads are untracked.
  $effect(() => {
    const rev = library.revision;
    untrack(() => {
      if (rev === 0) return;
      if (!list || list.getScrollOffset() < 400) refresh();
      else staleHint = true;
    });
  });
</script>

<div class="flex h-full min-h-0 flex-col p-6 pb-0">
  <h1 class="mb-5 shrink-0 text-2xl font-bold tracking-tight">{m.nav_artists()}</h1>

  {#if error}
    <p class="mb-4 shrink-0 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}

  <div class="flex min-h-0 flex-1 gap-1">
    <div class="min-h-0 flex-1" bind:clientWidth={gridWidth}>
      {#if gridWidth > 0}
      <VList
        bind:this={list}
        data={rows}
        getKey={(row, index) => row[0]?.id ?? index}
        style="height: 100%;"
        onscroll={maybeLoadMore}
      >
        {#snippet children(row)}
          <div
            class="grid gap-4 pb-4"
            style="grid-template-columns: repeat({cols}, minmax(0, 1fr));"
          >
            {#each row as artist (artist.id)}
              <a
                href="/artist/{artist.id}"
                oncontextmenu={(e) => openArtistMenu(e, artist)}
                {@attach contextMenuKey}
                class="group rounded-lg bg-card p-3 text-center transition-colors duration-200 hover:bg-panel-2"
              >
                <div class="overflow-hidden rounded-full shadow-lg">
                  <Cover
                    itemId={artist.id}
                    tag={artist.imageTag}
                    size={360}
                    alt={artist.name}
                    blurhash={artist.imageBlurHash}
                    round
                  />
                </div>
                <p class="mt-3 truncate text-sm font-semibold">{artist.name}</p>
                <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
                  {m.artist_eyebrow()}
                </p>
              </a>
            {/each}
          </div>
        {/snippet}
      </VList>
      {/if}
    </div>
    <AlphabetRail onselect={jumpToLetter} />
  </div>
</div>

{#if staleHint}
  <NewContentPill onrefresh={refresh} />
{/if}

{#if artistMenu}
  <ContextMenu
    x={artistMenu.x}
    y={artistMenu.y}
    items={artistMenu.items}
    onclose={() => (artistMenu = null)}
  />
{/if}
