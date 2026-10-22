<script lang="ts">
  import { onMount } from "svelte";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import { VList } from "virtua/svelte";
  import { api } from "#lib/api.js";
  import { errorCause } from "#lib/appErrors.js";
  import { appendPage, pageUntil, type PagedList } from "#lib/paging.js";
  import { m } from "#lib/paraglide/messages.js";
  import AlphabetRail from "#lib/components/AlphabetRail.svelte";
  import Cover from "#lib/components/Cover.svelte";
  import NewContentPill from "#lib/components/NewContentPill.svelte";
  import AlbumContextMenu from "#lib/components/AlbumContextMenu.svelte";
  import CardSkeleton from "#lib/components/CardSkeleton.svelte";
  import EmptyState from "#lib/components/EmptyState.svelte";
  import { player } from "#lib/state/player.svelte.js";
  import { startDrag } from "#lib/dragToPlaylist.js";
  import { onLibraryChange } from "#lib/state/library.svelte.js";
  import { createAlbumMenu } from "#lib/state/albumMenu.svelte.js";
  import { layoutPreferences } from "#lib/state/layout.svelte.js";
  import { contextMenuKey } from "#lib/menu.js";
  import type { AlbumDto, GenreDto } from "#lib/types.js";

  const am = createAlbumMenu();

  const PAGE_SIZE = 100;
  /** Start fetching the next page this many px before the end. */
  const LOAD_AHEAD_PX = 800;
  /** Card min width incl. gap; determines the responsive column count. */

  let albums = $state<AlbumDto[]>([]);
  let total = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let gridWidth = $state(0);
  let list = $state<VList<AlbumDto[]> | null>(null);
  let genres = $state<GenreDto[]>([]);
  const SORT_OPTIONS = ["name", "dateAdded", "year", "random", "playCount"] as const;
  const PLAYED_OPTIONS = ["all", "unplayed", "played"] as const;

  /** The genre the URL asks for: `/?genre=<id>` from the Genres page and the
   *  Discover hub; a plain `/` (the sidebar) means all of them. */
  function urlGenre(): string | null {
    return page.url.searchParams.get("genre") || null;
  }

  /** The sort the URL asks for, when it names a known one. */
  function urlSort(): string | null {
    const value = page.url.searchParams.get("sort");
    return value && (SORT_OPTIONS as readonly string[]).includes(value) ? value : null;
  }

  // From the URL right away: the first page must be fetched with them.
  let activeGenre = $state<string | null>(urlGenre());
  let sort = $state<string>(urlSort() ?? "name");
  let favoritesOnly = $state(false);
  let played = $state<(typeof PLAYED_OPTIONS)[number]>("all");
  const playedLabel = (p: string) =>
    p === "unplayed" ? m.played_unplayed() : p === "played" ? m.played_played() : m.played_all();
  const sortLabel = (s: string) =>
    s === "dateAdded"
      ? m.sort_date_added()
      : s === "year"
        ? m.sort_year()
        : s === "random"
          ? m.sort_random()
          : s === "playCount"
            ? m.sort_play_count()
            : m.sort_name();
  /** The library changed while scrolled down; a refresh pill is offered. */
  let staleHint = $state(false);
  /** Bumped on genre change so in-flight page fetches discard themselves. */
  let requestSeq = 0;
  /** Bumped by every letter jump and by a reload: an older jump stops paging
   *  and must not scroll anywhere after that. */
  let jumpSeq = 0;
  /** The page fetch in flight, so a letter jump can wait for it. */
  let pending: Promise<boolean> | null = null;

  const hasMore = $derived(total === null || albums.length < total);
  /** Whether a filter, not the library, can be why the grid is empty. */
  const filtered = $derived(activeGenre !== null || favoritesOnly || played !== "all");
  // `letterIndex` counts albums before a letter by ascending SortName within
  // the genre only, so the rail would land on unrelated rows under any other
  // sort or with the favorites/played filters active.
  const letterJumpAvailable = $derived(sort === "name" && !favoritesOnly && played === "all");
  const cols = $derived(
    Math.max(2, Math.floor(gridWidth / layoutPreferences.cardPixels.stride)),
  );
  /** Virtualized per row: chunk the flat album list into rows of `cols`. */
  const rows = $derived.by(() => {
    const chunked: AlbumDto[][] = [];
    for (let i = 0; i < albums.length; i += cols) {
      chunked.push(albums.slice(i, i + cols));
    }
    return chunked;
  });

  async function fetchPage(): Promise<boolean> {
    const seq = requestSeq;
    loading = true;
    error = null;
    try {
      const page = await api.getAlbums(
        albums.length,
        PAGE_SIZE,
        activeGenre,
        sort,
        favoritesOnly,
        played,
      );
      if (seq !== requestSeq) return false; // genre switched mid-flight
      // A page that adds nothing (the list shifted, or an album vanished after
      // a play-sorted grid was scanned) ends paging.
      const merged = appendPage(albums, page.items, page.total, (album) => album.id);
      albums = merged.items;
      total = merged.total;
      return merged.grew;
    } catch (e) {
      if (seq === requestSeq) error = errorCause(e);
      return false;
    } finally {
      if (seq === requestSeq) {
        loading = false;
        pending = null;
      }
    }
  }

  /** Start a page fetch, kept as `pending` for a letter jump to wait on. */
  function startPage(): Promise<boolean> {
    pending = fetchPage();
    return pending;
  }

  function loadMore(): Promise<boolean> {
    if (loading || !hasMore) return Promise.resolve(false);
    return startPage();
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

  // Reset the list and reload the first page from the top (`requestSeq`
  // discards any page still in flight). Shared by genre/sort/filter changes,
  // manual refresh, and library-change reloads.
  function reloadFromTop() {
    staleHint = false;
    requestSeq++;
    jumpSeq++; // a jump's row number belongs to the list being replaced
    albums = [];
    total = null;
    list?.scrollTo(0);
    startPage().then((ok) => {
      if (ok) maybeLoadMore();
    });
  }

  function selectGenre(genreId: string | null) {
    if (genreId === activeGenre) return;
    activeGenre = genreId;
    reloadFromTop();
  }

  function selectSort(value: string) {
    if (value === sort) return;
    sort = value;
    reloadFromTop();
  }

  function toggleFavoritesOnly() {
    favoritesOnly = !favoritesOnly;
    reloadFromTop();
  }

  function selectPlayed(value: (typeof PLAYED_OPTIONS)[number]) {
    if (value === played) return;
    played = value;
    reloadFromTop();
  }

  /** The way out of an empty filtered grid: everything, in the current sort. */
  function clearFilters() {
    activeGenre = null;
    favoritesOnly = false;
    played = "all";
    reloadFromTop();
  }

  const refresh = reloadFromTop;

  // On a server library change: reload live when near the top, otherwise show
  // a pill so a deep scroll position isn't yanked away.
  onLibraryChange(() => {
    if (!list || list.getScrollOffset() < 400) refresh();
    else staleHint = true;
  });

  const paged: PagedList = {
    length: () => albums.length,
    hasMore: () => hasMore,
    pending: () => pending,
    loadMore,
  };

  async function jumpToLetter(letter: string) {
    const jump = ++jumpSeq;
    const current = () => jump === jumpSeq;
    try {
      const index = letter === "#" ? 0 : await api.letterIndex("albums", letter, activeGenre);
      // Page in until the target row exists, then scroll there, unless a
      // later letter (or another genre) took over meanwhile.
      if (!(await pageUntil(index, paged, current))) return;
      list?.scrollToIndex(Math.floor(Math.min(index, Math.max(albums.length - 1, 0)) / cols), {
        align: "start",
      });
    } catch (e) {
      if (current()) error = errorCause(e);
    }
  }

  // SvelteKit keeps this page when only the query changes (the sidebar's
  // "Albums" from /?genre=…, Back and Forward between the two), so every
  // navigation applies the URL, not just the first. A URL without ?sort=
  // leaves the sort the user picked.
  afterNavigate(() => {
    const genre = urlGenre();
    const nextSort = urlSort() ?? sort;
    if (genre === activeGenre && nextSort === sort) return;
    activeGenre = genre;
    sort = nextSort;
    reloadFromTop();
  });

  onMount(() => {
    loadMore().then(() => maybeLoadMore());
    api.getGenres().then((g) => (genres = g)).catch(() => {});
  });
</script>

<div class="flex h-full min-h-0 flex-col p-6 pb-0">
  <div class="mb-4 flex shrink-0 flex-wrap items-center gap-3">
    <h1 class="text-2xl font-bold tracking-tight">{m.nav_albums()}</h1>
    <div class="ml-auto flex items-center gap-2">
      <select
        class="rounded-md border border-edge bg-card px-2 py-1.5 text-xs font-semibold text-ink outline-none focus:border-accent"
        value={played}
        onchange={(e) =>
          selectPlayed((e.currentTarget as HTMLSelectElement).value as (typeof PLAYED_OPTIONS)[number])}
      >
        {#each PLAYED_OPTIONS as option (option)}
          <option value={option}>{playedLabel(option)}</option>
        {/each}
      </select>
      <button
        class="rounded-full px-3 py-1.5 text-xs font-semibold transition-colors {favoritesOnly
          ? 'bg-accent text-(--color-on-accent)'
          : 'bg-card text-ink-muted hover:bg-panel-2 hover:text-ink'}"
        onclick={toggleFavoritesOnly}
        aria-pressed={favoritesOnly}
      >
        {m.albums_favorites_only()}
      </button>
      <label class="flex items-center gap-1.5 text-xs text-ink-muted">
        <span>{m.albums_sort()}</span>
        <select
          class="rounded-md border border-edge bg-card px-2 py-1.5 text-xs font-semibold text-ink outline-none focus:border-accent"
          value={sort}
          onchange={(e) => selectSort((e.currentTarget as HTMLSelectElement).value)}
        >
          {#each SORT_OPTIONS as option (option)}
            <option value={option}>{sortLabel(option)}</option>
          {/each}
        </select>
      </label>
    </div>
  </div>

  {#if genres.length > 0}
    <div class="chip-row mb-4 flex shrink-0 gap-2 overflow-x-auto pb-1">
      <button
        class="shrink-0 rounded-full px-3.5 py-1.5 text-xs font-semibold transition-colors {activeGenre === null
          ? 'bg-accent text-(--color-on-accent)'
          : 'bg-card text-ink-muted hover:bg-panel-2 hover:text-ink'}"
        onclick={() => selectGenre(null)}
      >
        {m.albums_all_genres()}
      </button>
      {#each genres as genre (genre.id)}
        <button
          class="shrink-0 rounded-full px-3.5 py-1.5 text-xs font-semibold transition-colors {activeGenre === genre.id
            ? 'bg-accent text-(--color-on-accent)'
            : 'bg-card text-ink-muted hover:bg-panel-2 hover:text-ink'}"
          onclick={() => selectGenre(genre.id)}
        >
          {genre.name}
        </button>
      {/each}
    </div>
  {/if}

  {#if error}
    <p class="mb-4 shrink-0 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}

  {#if total === 0 && filtered}
    <EmptyState
      icon="filter"
      title={m.albums_empty_filtered()}
      action={{ label: m.albums_clear_filters(), onclick: clearFilters }}
    />
  {:else if total === 0}
    <EmptyState icon="albums" title={m.albums_empty()} hint={m.empty_library_hint()} />
  {/if}

  <div class="flex min-h-0 flex-1 gap-1">
    <div class="min-h-0 flex-1" bind:clientWidth={gridWidth}>
      {#if gridWidth > 0}
        {#if loading && albums.length === 0 && !error}
          <div class="grid gap-4" style="grid-template-columns: repeat({cols}, minmax(0, 1fr));">
            {#each Array(cols * 3) as _, i (i)}
              <CardSkeleton />
            {/each}
          </div>
        {:else}
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
              {#each row as album (album.id)}
                <a
                  href="/album/{album.id}"
                  draggable="true"
                  ondragstart={(e) => startDrag(e, { albumId: album.id, label: album.name })}
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
                    <button
                      class="absolute right-3.5 bottom-14 flex h-8 w-8 translate-y-2 items-center justify-center rounded-full bg-panel-2/90 text-ink opacity-0 shadow-xl transition-all duration-200 group-hover:translate-y-0 group-hover:opacity-100 hover:scale-105 hover:text-accent"
                      onclick={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        player.run(api.playInstantMix(album.id));
                      }}
                      aria-label={m.album_instant_mix()}
                      title={m.album_instant_mix()}
                    >
                      <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                        <path d="M10.59 9.17 5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z" />
                      </svg>
                    </button>
                  </div>
                  <p class="mt-3 truncate text-sm font-semibold">{album.name}</p>
                  <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
                    {album.artist}{album.year ? ` · ${album.year}` : ""}
                  </p>
                </a>
              {/each}
            </div>
          {/snippet}
        </VList>
        {/if}
      {/if}
    </div>
    {#if letterJumpAvailable}
      <AlphabetRail onselect={jumpToLetter} />
    {/if}
  </div>
</div>

{#if staleHint}
  <NewContentPill onrefresh={refresh} />
{/if}
<AlbumContextMenu menu={am} />

<style>
  /* The chip row scrolls horizontally; a visible scrollbar would be noise. */
  .chip-row::-webkit-scrollbar {
    display: none;
  }
</style>
