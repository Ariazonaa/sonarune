<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { api, formatDuration } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import AlbumContextMenu from "$lib/components/AlbumContextMenu.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { createAlbumMenu } from "$lib/state/albumMenu.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import { contextMenuKey } from "$lib/menu";
  import type { FavoritesData, Page, TrackDto } from "$lib/types";

  type Section = keyof FavoritesData;

  const am = createAlbumMenu();

  /** Items per "load more"; the first page of every section comes with `getFavorites`. */
  const PAGE_SIZE = 50;

  let favorites = $state<FavoritesData | null>(swrGet<FavoritesData>("favorites") ?? null);
  let error = $state<string | null>(null);
  let loadingMore = $state<Record<Section, boolean>>({ artists: false, albums: false, tracks: false });
  let moreError = $state<Record<Section, string | null>>({ artists: null, albums: null, tracks: null });
  /** Discards a full reload that a newer one overtook. */
  let loadSeq = 0;
  /** Bumped whenever the lists are replaced wholesale; section pages still in flight belong to the old lists. */
  let generation = 0;

  function load() {
    const seq = ++loadSeq;
    apiLibrary
      .getFavorites()
      .then((f) => {
        if (seq !== loadSeq) return;
        generation++;
        loadingMore = { artists: false, albums: false, tracks: false };
        moreError = { artists: null, albums: null, tracks: null };
        swrSet("favorites", f);
        favorites = f;
        error = null;
      })
      .catch((e) => {
        if (seq === loadSeq && !favorites) error = String(e);
      });
  }

  onMount(load);

  // Favoriting happens elsewhere; refetch when the library signals a change.
  $effect(() => {
    if (library.revision > 0) untrack(load);
  });

  /** Append `next` to `current`. Favorites can change between pages; a repeated id would crash the keyed list. */
  function mergePage<T extends { id: string }>(current: Page<T>, next: Page<T>): Page<T> {
    const known = new Set(current.items.map((item) => item.id));
    const fresh = next.items.filter((item) => !known.has(item.id));
    return {
      items: [...current.items, ...fresh],
      // A page that adds nothing means the end, whatever the total claims.
      total: fresh.length === 0 ? current.items.length : next.total,
    };
  }

  async function loadMore(section: Section) {
    const current = favorites;
    if (!current || loadingMore[section]) return;
    const seq = generation;
    const start = current[section].items.length;
    loadingMore[section] = true;
    moreError[section] = null;
    try {
      if (section === "artists") {
        const next = await apiLibrary.getFavoriteArtists(start, PAGE_SIZE);
        if (seq === generation && favorites) favorites.artists = mergePage(favorites.artists, next);
      } else if (section === "albums") {
        const next = await apiLibrary.getFavoriteAlbums(start, PAGE_SIZE);
        if (seq === generation && favorites) favorites.albums = mergePage(favorites.albums, next);
      } else {
        const next = await apiLibrary.getFavoriteTracks(start, PAGE_SIZE);
        if (seq === generation && favorites) favorites.tracks = mergePage(favorites.tracks, next);
      }
    } catch (e) {
      if (seq === generation) moreError[section] = String(e);
    } finally {
      if (seq === generation) loadingMore[section] = false;
    }
  }

  async function playTrack(track: TrackDto) {
    if (!track.albumId) {
      player.run(api.enqueueTracks([track.id], false));
      return;
    }
    try {
      const album = await api.getAlbum(track.albumId);
      const index = album.tracks.findIndex((t) => t.id === track.id);
      await api.playAlbum(track.albumId, Math.max(0, index));
    } catch (e) {
      player.error = String(e);
    }
  }

  const isEmpty = $derived(
    favorites !== null &&
      favorites.tracks.items.length === 0 &&
      favorites.albums.items.length === 0 &&
      favorites.artists.items.length === 0,
  );
</script>

{#snippet sectionHeading(section: Section, title: string, count: string)}
  <div class="mb-3 flex items-baseline gap-3">
    <h2 id="favorites-{section}" class="text-lg font-bold tracking-tight">{title}</h2>
    <span class="text-sm font-medium text-ink-muted">{count}</span>
  </div>
{/snippet}

{#snippet sectionMore(section: Section, page: Page<{ id: string }>)}
  {@const message = moreError[section]}
  {#if message}
    <p class="mt-4 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message })}
    </p>
  {/if}
  {#if page.items.length < page.total}
    <div class="mt-4 flex justify-center">
      <button
        class="rounded-full border border-edge px-5 py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
        onclick={() => loadMore(section)}
        disabled={loadingMore[section]}
        aria-describedby="favorites-{section}"
      >
        {loadingMore[section] ? m.discover_loading() : m.discover_load_more()}
      </button>
    </div>
  {/if}
{/snippet}

<div class="p-6">
  <h1 class="mb-5 text-2xl font-bold tracking-tight">{m.nav_favorites()}</h1>

  {#if error}
    <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if isEmpty}
    <EmptyState
      icon="heart"
      title={m.favorites_empty()}
      action={{ label: m.empty_cta_albums(), href: "/" }}
    />
  {:else if favorites}
    {#if favorites.artists.items.length > 0}
      {@const total = favorites.artists.total}
      <section class="mb-8">
        {@render sectionHeading(
          "artists",
          m.favorites_artists(),
          (total === 1 ? m.favorites_artists_count_one : m.favorites_artists_count_many)({ count: total }),
        )}
        <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(9.5rem,1fr))] gap-4">
          {#each favorites.artists.items as artist (artist.id)}
            <a
              href="/artist/{artist.id}"
              class="group rounded-lg bg-card p-3 text-center transition-colors duration-200 hover:bg-panel-2"
            >
              <div class="overflow-hidden rounded-full shadow-lg">
                <Cover itemId={artist.id} tag={artist.imageTag} size={360} alt={artist.name} blurhash={artist.imageBlurHash} round />
              </div>
              <p class="mt-3 truncate text-sm font-semibold">{artist.name}</p>
              <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">{m.artist_eyebrow()}</p>
            </a>
          {/each}
        </div>
        {@render sectionMore("artists", favorites.artists)}
      </section>
    {/if}

    {#if favorites.albums.items.length > 0}
      {@const total = favorites.albums.total}
      <section class="mb-8">
        {@render sectionHeading(
          "albums",
          m.favorites_albums(),
          (total === 1 ? m.favorites_albums_count_one : m.favorites_albums_count_many)({ count: total }),
        )}
        <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
          {#each favorites.albums.items as album (album.id)}
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
              <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">{album.artist}</p>
            </a>
          {/each}
        </div>
        {@render sectionMore("albums", favorites.albums)}
      </section>
    {/if}

    {#if favorites.tracks.items.length > 0}
      {@const total = favorites.tracks.total}
      <section class="mb-8 max-w-3xl">
        {@render sectionHeading(
          "tracks",
          m.favorites_tracks(),
          (total === 1 ? m.favorites_tracks_count_one : m.favorites_tracks_count_many)({ count: total }),
        )}
        <table class="w-full border-collapse text-sm">
          <tbody>
            {#each favorites.tracks.items as track (track.id)}
              <tr
                class="group cursor-pointer transition-colors hover:bg-ink/10"
                onclick={() => playTrack(track)}
              >
                <td class="w-11 rounded-l-md py-1">
                  <div class="ml-2 w-9">
                    <Cover itemId={track.imageItemId} tag={track.imageTag} size={96} alt="" blurhash={track.imageBlurHash} />
                  </div>
                </td>
                <td class="px-3 py-1.5">
                  <p class="truncate font-medium">{track.name}</p>
                  <p class="truncate text-xs text-ink-muted">{track.artist}</p>
                </td>
                <td class="w-16 rounded-r-md px-2 py-1.5 text-right text-ink-muted tabular-nums">
                  {formatDuration(track.durationMs)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
        {@render sectionMore("tracks", favorites.tracks)}
      </section>
    {/if}
  {/if}
</div>

<AlbumContextMenu menu={am} />
