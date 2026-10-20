<script lang="ts">
  import { onMount } from "svelte";
  import { VList } from "virtua/svelte";
  import { api } from "#lib/api.js";
  import { appendPage, pageUntil, type PagedList } from "#lib/paging.js";
  import { m } from "#lib/paraglide/messages.js";
  import AlphabetRail from "#lib/components/AlphabetRail.svelte";
  import Cover from "#lib/components/Cover.svelte";
  import NewContentPill from "#lib/components/NewContentPill.svelte";
  import ContextMenu, { type ContextMenuItem } from "#lib/components/ContextMenu.svelte";
  import { contextMenuKey } from "#lib/menu.js";
  import { apiLibrary } from "#lib/api/library.js";
  import { player } from "#lib/state/player.svelte.js";
  import { onLibraryChange } from "#lib/state/library.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import { layoutPreferences } from "#lib/state/layout.svelte.js";
  import type { ArtistDto } from "#lib/types.js";

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
          label: artist.isFavorite ? m.favorite_remove() : m.favorite_add(),
          icon: "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
          action: () => toggleFavorite(artist),
        },
      ],
    };
  }

  // Optimistic like the album menu: flip the card's DTO (it is reactive), and
  // flip it back if the server refuses. The cards show no heart, so the toast
  // is the feedback.
  function toggleFavorite(artist: ArtistDto) {
    const next = !artist.isFavorite;
    artist.isFavorite = next;
    apiLibrary
      .setFavorite(artist.id, next)
      .then(() => toast.show(next ? m.favorite_added() : m.favorite_removed()))
      .catch((err) => {
        artist.isFavorite = !next;
        player.error = String(err);
      });
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
  /** Bumped by every letter jump and by a refresh: an older jump stops paging
   *  and must not scroll anywhere after that. */
  let jumpSeq = 0;
  /** The page fetch in flight, so a letter jump can wait for it. */
  let pending: Promise<boolean> | null = null;

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

  function loadMore(): Promise<boolean> {
    if (loading || !hasMore) return Promise.resolve(false);
    pending = fetchPage();
    return pending;
  }

  async function fetchPage(): Promise<boolean> {
    const seq = reloadSeq;
    loading = true;
    error = null;
    try {
      const page = await api.getArtists(artists.length, PAGE_SIZE);
      if (seq !== reloadSeq) return false; // a library refresh superseded this
      const merged = appendPage(artists, page.items, page.total, (artist) => artist.id);
      artists = merged.items;
      total = merged.total;
      return merged.grew;
    } catch (e) {
      if (seq === reloadSeq) error = String(e);
      return false;
    } finally {
      if (seq === reloadSeq) {
        loading = false;
        pending = null;
      }
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

  const paged: PagedList = {
    length: () => artists.length,
    hasMore: () => hasMore,
    pending: () => pending,
    loadMore,
  };

  async function jumpToLetter(letter: string) {
    const jump = ++jumpSeq;
    const current = () => jump === jumpSeq;
    try {
      const index = letter === "#" ? 0 : await api.letterIndex("artists", letter);
      // Page in until the target row exists, then scroll there, unless a
      // later letter took over meanwhile.
      if (!(await pageUntil(index, paged, current))) return;
      list?.scrollToIndex(Math.floor(Math.min(index, Math.max(artists.length - 1, 0)) / cols), {
        align: "start",
      });
    } catch (e) {
      if (current()) error = String(e);
    }
  }

  onMount(() => {
    loadMore().then(() => maybeLoadMore());
  });

  function refresh() {
    staleHint = false;
    reloadSeq++;
    jumpSeq++; // a jump's row number belongs to the list being replaced
    artists = [];
    total = null;
    loading = false;
    list?.scrollTo(0);
    loadMore().then((ok) => ok && maybeLoadMore());
  }

  // Reload live near the top, else offer a refresh pill.
  onLibraryChange(() => {
    if (!list || list.getScrollOffset() < 400) refresh();
    else staleHint = true;
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
