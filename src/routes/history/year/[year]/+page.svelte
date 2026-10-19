<script lang="ts">
  import { untrack } from "svelte";
  import { page } from "$app/state";
  import { api, formatDuration } from "#lib/api.js";
  import { apiLibrary } from "#lib/api/library.js";
  import { m } from "#lib/paraglide/messages.js";
  import { getLocale } from "#lib/paraglide/runtime.js";
  import BarList, { type BarItem } from "#lib/components/BarList.svelte";
  import Cover from "#lib/components/Cover.svelte";
  import EmptyState from "#lib/components/EmptyState.svelte";
  import { formatListened } from "#lib/historyTimeline.js";
  import { player } from "#lib/state/player.svelte.js";
  import type { HistoryTrack, HistoryYears, YearReview } from "#lib/types.js";

  const year = $derived(Number.parseInt(page.params.year ?? "", 10));

  let review = $state<YearReview | null>(null);
  let years = $state<HistoryYears | null>(null);
  let error = $state<string | null>(null);
  let generation = 0;

  async function load(target: number) {
    const seq = ++generation;
    error = null;
    try {
      const [nextReview, nextYears] = await Promise.all([
        apiLibrary.historyYear(target),
        apiLibrary.historyYears(),
      ]);
      if (seq !== generation) return;
      review = nextReview;
      years = nextYears;
    } catch (e) {
      if (seq === generation) error = String(e);
    }
  }

  $effect(() => {
    const target = year;
    if (Number.isFinite(target)) untrack(() => load(target));
  });

  const locale = $derived(getLocale());
  const number = $derived(new Intl.NumberFormat(locale));
  const date = $derived(new Intl.DateTimeFormat(locale, { day: "numeric", month: "long", year: "numeric" }));

  const listened = $derived(formatListened(review?.listenedMs ?? 0));

  /** Recording began during this year: everything before it is unknown. */
  const partialSince = $derived.by(() => {
    const since = years?.recordedSince;
    if (since == null) return null;
    return new Date(since).getFullYear() === year ? new Date(since) : null;
  });

  const monthBars = $derived<BarItem[]>(
    (review?.months ?? []).map((plays, month) => ({
      label: new Intl.DateTimeFormat(locale, { month: "long" }).format(new Date(year, month, 1)),
      value: plays,
      valueLabel: m.stats_plays({ count: plays }),
    })),
  );

  const artistBars = $derived<BarItem[]>(
    (review?.topArtists ?? []).map((artist) => ({
      label: artist.name,
      value: artist.plays,
      valueLabel: m.stats_plays({ count: artist.plays }),
      href: artist.id ? `/artist/${artist.id}` : undefined,
    })),
  );

  async function playTrack(track: HistoryTrack) {
    if (!track.albumId) {
      player.run(api.enqueueTracks([track.itemId], false));
      return;
    }
    try {
      const album = await api.getAlbum(track.albumId);
      const index = album.tracks.findIndex((t) => t.id === track.itemId);
      if (index < 0) player.run(api.enqueueTracks([track.itemId], false));
      else await api.playAlbum(track.albumId, index);
    } catch (e) {
      player.error = String(e);
    }
  }
</script>

{#snippet tile(label: string, value: string)}
  <div class="rounded-lg bg-card p-4">
    <p class="text-xs font-semibold uppercase tracking-wider text-ink-muted">{label}</p>
    <p class="mt-1 text-2xl font-bold tracking-tight tabular-nums">{value}</p>
  </div>
{/snippet}

<div class="p-6">
  <a href="/history" class="mb-4 inline-flex items-center gap-1 text-sm font-semibold text-ink-muted hover:text-ink">
    <span aria-hidden="true">←</span> {m.history_title()}
  </a>
  <div class="mb-1 flex flex-wrap items-center gap-3">
    <h1 class="mr-auto text-2xl font-bold tracking-tight">{m.history_review_title({ year })}</h1>
    {#if years && years.years.length > 1}
      <nav class="flex flex-wrap gap-1.5" aria-label={m.history_review_years()}>
        {#each years.years as other (other)}
          <a
            href="/history/year/{other}"
            aria-current={other === year ? "page" : undefined}
            class="rounded-full px-3 py-1 text-sm font-semibold tabular-nums transition-colors {other === year
              ? 'bg-accent text-(--color-on-accent)'
              : 'border border-edge text-ink-muted hover:border-ink-muted hover:text-ink'}"
          >
            {other}
          </a>
        {/each}
      </nav>
    {/if}
  </div>
  <p class="mb-6 max-w-2xl text-xs text-ink-muted">
    {#if partialSince}
      {m.history_review_partial({ date: date.format(partialSince) })}
    {:else}
      {m.history_review_note()}
    {/if}
  </p>

  {#if error}
    <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if review && review.plays === 0}
    <EmptyState icon="stats" title={m.history_review_empty({ year })} action={{ label: m.history_title(), href: "/history" }} />
  {:else if review}
    <div class="mb-8 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {@render tile(m.history_review_plays(), number.format(review.plays))}
      {@render tile(
        m.history_review_time(),
        listened.hours > 0
          ? m.history_review_hours({ hours: number.format(listened.hours), minutes: listened.minutes })
          : m.history_review_minutes({ minutes: listened.minutes }),
      )}
      {@render tile(m.stats_tile_tracks(), number.format(review.tracks))}
      {@render tile(m.stats_tile_artists(), number.format(review.artists))}
    </div>

    <div class="mb-8 grid gap-4 lg:grid-cols-2">
      {#if artistBars.length > 0}
        <section class="rounded-lg bg-card p-5">
          <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
            {m.stats_top_artists()}
          </h2>
          <BarList items={artistBars} />
        </section>
      {/if}
      <section class="rounded-lg bg-card p-5">
        <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
          {m.history_review_months()}
        </h2>
        <BarList items={monthBars} />
      </section>
    </div>

    {#if review.topAlbums.length > 0}
      <section class="mb-8">
        <h2 class="mb-3 text-lg font-bold tracking-tight">{m.stats_most_played_albums()}</h2>
        <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
          {#each review.topAlbums as album (album.albumId ?? `${album.name}\u0001${album.artist}`)}
            <svelte:element
              this={album.albumId ? "a" : "div"}
              href={album.albumId ? `/album/${album.albumId}` : undefined}
              class="group w-44 shrink-0 rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2"
            >
              <div class="overflow-hidden rounded-md shadow-lg">
                <Cover
                  itemId={album.imageItemId}
                  tag={album.imageTag}
                  size={360}
                  alt={album.name}
                  blurhash={album.imageBlurHash}
                />
              </div>
              <p class="mt-3 truncate text-sm font-semibold">{album.name}</p>
              <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
                {album.artist} · {m.stats_plays({ count: album.plays })}
              </p>
            </svelte:element>
          {/each}
        </div>
      </section>
    {/if}

    {#if review.topTracks.length > 0}
      <section class="mb-8">
        <h2 class="mb-3 text-lg font-bold tracking-tight">{m.stats_most_played_tracks()}</h2>
        <table class="w-full max-w-3xl border-collapse text-sm">
          <tbody>
            {#each review.topTracks as { track, plays }, i (track.itemId)}
              <tr class="group cursor-pointer transition-colors hover:bg-ink/10" onclick={() => playTrack(track)}>
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
                <td class="w-14 px-2 py-1.5 text-right text-xs text-ink-muted tabular-nums">{plays}×</td>
                <td class="w-16 rounded-r-md px-2 py-1.5 text-right text-ink-muted tabular-nums">
                  {formatDuration(track.durationMs)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {/if}
  {/if}
</div>
