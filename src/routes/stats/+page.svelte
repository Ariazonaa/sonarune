<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatDuration } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import { getLocale } from "$lib/paraglide/runtime";
  import Cover from "$lib/components/Cover.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import BarList, { type BarItem } from "$lib/components/BarList.svelte";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import type { AlbumDto, StatsData, TrackDto } from "$lib/types";

  let stats = $state<StatsData | null>(swrGet<StatsData>("stats") ?? null);
  let error = $state<string | null>(null);

  function load() {
    apiLibrary
      .getStats()
      .then((s) => {
        swrSet("stats", s);
        stats = s;
        error = null;
      })
      .catch((e) => {
        if (!stats) error = String(e);
      });
  }

  onMount(load);

  // Play counts and last-played dates change as you listen; refresh on change.
  $effect(() => {
    if (library.revision > 0) load();
  });

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

  /** Plain thousands separators, in the language the app is in. */
  const number = $derived(new Intl.NumberFormat(getLocale()));

  const artistBars = $derived<BarItem[]>(
    (stats?.topArtists ?? []).map((artist) => ({
      label: artist.name,
      value: artist.plays,
      valueLabel: m.stats_plays({ count: artist.plays }),
      href: artist.id ? `/artist/${artist.id}` : undefined,
    })),
  );

  const decadeBars = $derived<BarItem[]>(
    (stats?.decades ?? []).map((decade) => ({
      label: decade.label,
      value: decade.albums,
      valueLabel: number.format(decade.albums),
      href: `/discover/decade/${decade.startYear}`,
    })),
  );

  /** Share of the library that has been played at least once. */
  const playedShare = $derived.by(() => {
    const totals = stats?.totals;
    if (!totals || totals.tracks <= 0) return null;
    return Math.round((totals.playedTracks / totals.tracks) * 100);
  });

  const isEmpty = $derived(
    stats !== null &&
      stats.recentlyPlayed.length === 0 &&
      stats.mostPlayedTracks.length === 0 &&
      stats.mostPlayedAlbums.length === 0,
  );
</script>

{#snippet trackList(tracks: TrackDto[])}
  <table class="w-full max-w-3xl border-collapse text-sm">
    <tbody>
      {#each tracks as track, i (track.id)}
        <tr
          class="group cursor-pointer transition-colors hover:bg-ink/10"
          onclick={() => playTrack(track)}
        >
          <td class="w-8 rounded-l-md px-2 py-1.5 text-right text-ink-muted tabular-nums">{i + 1}</td>
          <td class="w-11 py-1">
            <div class="w-9">
              <Cover itemId={track.imageItemId} tag={track.imageTag} size={96} alt="" blurhash={track.imageBlurHash} />
            </div>
          </td>
          <td class="px-3 py-1.5">
            <p class="truncate font-medium">{track.name}</p>
            <p class="truncate text-xs text-ink-muted">{track.artist}</p>
          </td>
          <td class="w-14 px-2 py-1.5 text-right text-xs text-ink-muted tabular-nums">
            {#if track.playCount > 0}{track.playCount}×{/if}
          </td>
          <td class="w-16 rounded-r-md px-2 py-1.5 text-right text-ink-muted tabular-nums">
            {formatDuration(track.durationMs)}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/snippet}

{#snippet tile(label: string, value: string, hint: string | null)}
  <div class="rounded-lg bg-card p-4">
    <p class="text-xs font-semibold uppercase tracking-wider text-ink-muted">{label}</p>
    <p class="mt-1 text-2xl font-bold tracking-tight tabular-nums">{value}</p>
    {#if hint}
      <p class="mt-0.5 text-xs text-ink-muted">{hint}</p>
    {/if}
  </div>
{/snippet}

{#snippet albumRow(albums: AlbumDto[])}
  <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
    {#each albums as album (album.id)}
      <a
        href="/album/{album.id}"
        class="group w-44 shrink-0 rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2"
      >
        <div class="overflow-hidden rounded-md shadow-lg">
          <Cover itemId={album.id} tag={album.imageTag} size={360} alt={album.name} blurhash={album.imageBlurHash} />
        </div>
        <p class="mt-3 truncate text-sm font-semibold">{album.name}</p>
        <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">{album.artist}</p>
      </a>
    {/each}
  </div>
{/snippet}

<div class="p-6">
  <h1 class="mb-1 text-2xl font-bold tracking-tight">{m.stats_title()}</h1>
  <p class="mb-6 text-xs text-ink-muted">{m.stats_note()}</p>

  {#if error}
    <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if isEmpty}
    <EmptyState
      icon="stats"
      title={m.stats_empty()}
      action={{ label: m.empty_cta_albums(), href: "/" }}
    />
  {:else if stats}
    <!-- Four numbers, no chart: a single value has nothing to compare itself
         to, and a bar of one is decoration. -->
    <div class="mb-8 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {@render tile(m.stats_tile_tracks(), number.format(stats.totals.tracks), null)}
      {@render tile(m.stats_tile_albums(), number.format(stats.totals.albums), null)}
      {@render tile(m.stats_tile_artists(), number.format(stats.totals.artists), null)}
      {@render tile(
        m.stats_tile_played(),
        number.format(stats.totals.playedTracks),
        playedShare === null ? null : m.stats_tile_played_share({ percent: playedShare }),
      )}
    </div>

    <div class="mb-8 grid gap-4 lg:grid-cols-2">
      {#if artistBars.length > 0}
        <section class="rounded-lg bg-card p-5">
          <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted">
            {m.stats_top_artists()}
          </h2>
          <p class="mt-1 mb-4 text-xs text-ink-muted">{m.stats_top_artists_hint()}</p>
          <BarList items={artistBars} />
        </section>
      {/if}
      {#if decadeBars.length > 0}
        <section class="rounded-lg bg-card p-5">
          <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted">
            {m.stats_decades()}
          </h2>
          <p class="mt-1 mb-4 text-xs text-ink-muted">{m.stats_decades_hint()}</p>
          <BarList items={decadeBars} />
        </section>
      {/if}
    </div>

    {#if stats.mostPlayedAlbums.length > 0}
      <section class="mb-8">
        <h2 class="mb-3 text-lg font-bold tracking-tight">{m.stats_most_played_albums()}</h2>
        {@render albumRow(stats.mostPlayedAlbums)}
      </section>
    {/if}
    {#if stats.mostPlayedTracks.length > 0}
      <section class="mb-8">
        <h2 class="mb-3 text-lg font-bold tracking-tight">{m.stats_most_played_tracks()}</h2>
        {@render trackList(stats.mostPlayedTracks)}
      </section>
    {/if}
    {#if stats.recentlyPlayed.length > 0}
      <section class="mb-8">
        <h2 class="mb-3 text-lg font-bold tracking-tight">{m.stats_recently_played()}</h2>
        {@render trackList(stats.recentlyPlayed)}
      </section>
    {/if}
  {/if}
</div>
