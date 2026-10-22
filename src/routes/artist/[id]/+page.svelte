<script lang="ts">
  import { untrack } from "svelte";
  import { page } from "$app/state";
  import { api, formatDuration } from "#lib/api.js";
  import { errorCause, isNotFound } from "#lib/appErrors.js";
  import { m } from "#lib/paraglide/messages.js";
  import Cover from "#lib/components/Cover.svelte";
  import CoverBackdrop from "#lib/components/CoverBackdrop.svelte";
  import AlbumContextMenu from "#lib/components/AlbumContextMenu.svelte";
  import { library } from "#lib/state/library.svelte.js";
  import { player } from "#lib/state/player.svelte.js";
  import { createAlbumMenu } from "#lib/state/albumMenu.svelte.js";
  import { contextMenuKey } from "#lib/menu.js";
  import { playInAlbum } from "#lib/playInAlbum.js";
  import { swrGet, swrSet, swrDelete } from "#lib/swr.js";
  import { apiLibrary } from "#lib/api/library.js";
  import type { AlbumDto, ArtistDetail, ArtistDto, TrackDto } from "#lib/types.js";

  const am = createAlbumMenu();

  let detail = $state<ArtistDetail | null>(null);
  let similar = $state<ArtistDto[]>([]);
  let error = $state<string | null>(null);
  let bioExpanded = $state(false);

  const artistId = $derived(page.params.id!);

  // Play a top song in its album context (Spotify behavior).
  function playTopSong(track: TrackDto) {
    player.run(playInAlbum(track.id, track.albumId));
  }

  $effect(() => {
    const id = artistId;
    const key = `artist:${id}`;
    detail = swrGet<ArtistDetail>(key) ?? null;
    similar = swrGet<ArtistDto[]>(`artist:similar:${id}`) ?? [];
    error = null;
    bioExpanded = false;
    api
      .getArtist(id)
      .then((d) => {
        swrSet(key, d);
        if (id === page.params.id) detail = d;
      })
      .catch((e) => {
        if (id !== page.params.id) return;
        const msg = errorCause(e);
        if (isNotFound(e)) {
          swrDelete(key);
          detail = null;
          error = msg;
        } else if (!detail) {
          error = msg;
        }
      });
    // Similar artists are decoration — a failure must not block the page.
    api
      .getSimilarArtists(id, 12)
      .then((artists) => {
        swrSet(`artist:similar:${id}`, artists);
        if (id === page.params.id) similar = artists;
      })
      .catch(() => {});
  });

  // Nothing to show after a failed load (server down, artist not cached): try
  // again when the library is poked — the offline banner's Retry and the
  // recovery after an outage do that (net.svelte.ts).
  $effect(() => {
    void library.revision;
    untrack(() => {
      if (!error || detail) return;
      const id = artistId;
      api
        .getArtist(id)
        .then((d) => {
          swrSet(`artist:${id}`, d);
          if (id !== page.params.id) return;
          detail = d;
          error = null;
        })
        .catch(() => {});
    });
  });

  const albumCount = $derived(detail?.albums.length ?? 0);

  // Heart-shaped SVG paths (filled / outline), as on the album page.
  const HEART_ON =
    "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z";
  const HEART_OFF =
    "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z";

  // Optimistic like the album heart: flip locally, revert if the server refuses.
  function toggleFavorite() {
    const d = detail;
    if (!d) return;
    const next = !d.artist.isFavorite;
    d.artist.isFavorite = next;
    apiLibrary.setFavorite(d.artist.id, next).catch(() => {
      d.artist.isFavorite = !next;
    });
  }
</script>

{#snippet albumGrid(albums: AlbumDto[])}
  <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
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
        <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">{album.year ?? ""}</p>
      </a>
    {/each}
  </div>
{/snippet}

<div>
  {#if error}
    <p class="m-6 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if detail}
    <header
      class="relative isolate flex items-end gap-6 overflow-hidden bg-gradient-to-b from-panel-2 to-panel p-6 pt-10"
    >
      <!-- Artists without a photo borrow their first album's cover. -->
      <CoverBackdrop
        itemId={detail.artist.imageTag ? detail.artist.id : (detail.albums[0]?.id ?? null)}
        tag={detail.artist.imageTag ?? detail.albums[0]?.imageTag ?? null}
      />
      <div class="w-40 shrink-0 shadow-2xl">
        <Cover
          itemId={detail.artist.id}
          tag={detail.artist.imageTag}
          size={480}
          alt={detail.artist.name}
          blurhash={detail.artist.imageBlurHash}
          round
        />
      </div>
      <div class="min-w-0">
        <p class="text-xs font-bold uppercase tracking-wider">{m.artist_eyebrow()}</p>
        <h1 class="mt-2 truncate text-5xl font-black tracking-tight">{detail.artist.name}</h1>
        <div class="mt-4 flex items-center gap-4">
          <button
            class="flex items-center gap-2 rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover"
            onclick={() => player.run(api.playInstantMix(artistId))}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M12 2C6.48 2 2 6.48 2 12v8a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-8c0-5.52-4.48-10-10-10zm0 3a7 7 0 0 1 6.32 4H5.68A7 7 0 0 1 12 5zm0 13a3 3 0 1 1 0-6 3 3 0 0 1 0 6z" />
            </svg>
            {m.artist_radio()}
          </button>
          <button
            class="rounded-full p-1.5 transition-colors {detail.artist.isFavorite
              ? 'text-accent hover:text-accent-hover'
              : 'text-ink-muted hover:text-ink'}"
            onclick={toggleFavorite}
            aria-pressed={detail.artist.isFavorite}
            aria-label={detail.artist.isFavorite ? m.favorite_remove() : m.favorite_add()}
            title={detail.artist.isFavorite ? m.favorite_remove() : m.favorite_add()}
          >
            <svg viewBox="0 0 24 24" class="h-6 w-6" fill="currentColor" aria-hidden="true">
              <path d={detail.artist.isFavorite ? HEART_ON : HEART_OFF} />
            </svg>
          </button>
          <p class="text-sm font-medium text-ink-muted">
            {(albumCount === 1 ? m.artist_albums_one : m.artist_albums_many)({ count: albumCount })}
          </p>
        </div>
      </div>
    </header>

    {#if detail.artist.overview}
      <section class="px-6 pt-5">
        <p
          class="max-w-3xl whitespace-pre-line text-sm leading-relaxed text-ink-muted {bioExpanded
            ? ''
            : 'line-clamp-3'}"
        >
          {detail.artist.overview}
        </p>
        <button
          class="mt-1 text-xs font-semibold text-accent hover:underline"
          onclick={() => (bioExpanded = !bioExpanded)}
        >
          {bioExpanded ? m.artist_show_less() : m.artist_show_more()}
        </button>
      </section>
    {/if}

    {#if detail.topSongs.length > 0}
      <section class="px-6 pt-6">
        <h2 class="mb-3 text-xl font-bold tracking-tight">{m.artist_top_songs()}</h2>
        <table class="w-full max-w-3xl border-collapse text-sm">
          <tbody>
            {#each detail.topSongs as track, i (track.id)}
              <tr
                class="group cursor-pointer transition-colors hover:bg-ink/10"
                onclick={() => playTopSong(track)}
              >
                <td class="w-8 rounded-l-md px-2 py-1.5 text-right text-ink-muted tabular-nums">
                  {i + 1}
                </td>
                <td class="w-11 py-1">
                  <div class="w-9">
                    <Cover itemId={track.imageItemId} tag={track.imageTag} size={96} alt="" blurhash={track.imageBlurHash} />
                  </div>
                </td>
                <td class="px-3 py-1.5"><p class="truncate font-medium">{track.name}</p></td>
                <td class="w-16 rounded-r-md px-2 py-1.5 text-right text-ink-muted tabular-nums">
                  {formatDuration(track.durationMs)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {/if}

    {#if detail.albums.length > 0}
      <section class="p-6">
        <h2 class="mb-4 text-xl font-bold tracking-tight">{m.nav_albums()}</h2>
        {@render albumGrid(detail.albums)}
      </section>
    {/if}

    {#if detail.appearsOn.length > 0}
      <section class="px-6 pb-6">
        <h2 class="mb-4 text-xl font-bold tracking-tight">{m.artist_appears_on()}</h2>
        {@render albumGrid(detail.appearsOn)}
      </section>
    {/if}

    {#if similar.length > 0}
      <section class="p-6 pt-0">
        <h2 class="mb-4 text-xl font-bold tracking-tight">{m.artist_similar()}</h2>
        <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
          {#each similar as artist (artist.id)}
            <a
              href="/artist/{artist.id}"
              class="w-42 shrink-0 rounded-lg bg-card p-3 text-center transition-colors duration-200 hover:bg-panel-2"
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
      </section>
    {/if}
  {:else}
    <div class="p-6">
      <div class="flex items-end gap-6">
        <div class="skeleton h-40 w-40 shrink-0 rounded-full"></div>
        <div class="flex-1 space-y-3 pb-2">
          <div class="skeleton h-3 w-16 rounded"></div>
          <div class="skeleton h-10 w-1/2 rounded-lg"></div>
        </div>
      </div>
      <div class="mt-8 grid grid-cols-[repeat(auto-fill,minmax(10.5rem,1fr))] gap-4">
        {#each Array(6) as _, i (i)}
          <div class="skeleton aspect-square w-full rounded-xl"></div>
        {/each}
      </div>
    </div>
  {/if}
</div>

<AlbumContextMenu menu={am} />
