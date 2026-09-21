<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { api, formatDuration } from "$lib/api";
  import {
    apiDiscovery,
    type SmartFilter,
    type SmartPlayed,
  } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import { session } from "$lib/state/session.svelte";
  import { library } from "$lib/state/library.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { confirm } from "$lib/state/confirm.svelte";
  import { swrGet, swrSet } from "$lib/swr";
  import {
    cloneSmartFilter,
    emptySmartFilter,
    loadSmartViews,
    newSmartView,
    persistSmartViews,
    smartYearProblem,
    type SavedSmartView,
    type SmartYearProblem,
  } from "$lib/smartViews";
  import type { GenreDto, Page, TrackDto } from "$lib/types";

  const PAGE_SIZE = 100;
  const QUERY_DELAY_MS = 250;

  let savedViews = $state<SavedSmartView[]>([]);
  let activeId = $state<string | null>(null);
  let name = $state("");
  let filter = $state<SmartFilter>(emptySmartFilter());
  let genres = $state<GenreDto[] | null>(swrGet<GenreDto[]>("genres") ?? null);
  let genresError = $state<string | null>(null);

  let tracks = $state<TrackDto[]>([]);
  let total = $state<number | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let queryGeneration = 0;
  let activeQuery = emptySmartFilter();
  let materializing = $state(false);

  // What is actually sent: integers within the backend's bounds.
  const cleanFilter = $derived(cloneSmartFilter(filter));
  const filterSignature = $derived(JSON.stringify(cleanFilter));
  // Years that can't be sent as entered (out of range, start after end, a
  // span the backend refuses) block querying and show why.
  const yearProblem = $derived(smartYearProblem(filter));
  const yearsInvalid = $derived(yearProblem !== null);
  const YEAR_PROBLEM_MESSAGES: Record<SmartYearProblem, () => string> = {
    range: m.smart_year_out_of_range,
    order: m.smart_year_invalid,
    span: m.smart_year_span,
  };
  const hasMore = $derived(total === null || tracks.length < total);
  const hasResults = $derived(tracks.length > 0);

  function loadGenres() {
    api
      .getGenres()
      .then((result) => {
        genres = result;
        genresError = null;
        swrSet("genres", result);
      })
      .catch((cause) => {
        if (!genres) genresError = String(cause);
      });
  }

  onMount(() => {
    savedViews = loadSmartViews(session.info);
    loadGenres();
  });

  function cacheKey(signature: string) {
    return `smart:results:${signature}`;
  }

  async function startQuery(signature: string) {
    const query = cloneSmartFilter(filter);
    const seq = ++queryGeneration;
    activeQuery = query;
    if (yearsInvalid) {
      tracks = [];
      total = 0;
      error = null;
      loading = false;
      return;
    }
    const cached = swrGet<Page<TrackDto>>(cacheKey(signature));
    tracks = cached?.items ?? [];
    total = cached?.total ?? null;
    loading = true;
    error = null;
    try {
      const result = await apiDiscovery.querySmartView(query, 0, PAGE_SIZE);
      if (seq !== queryGeneration || signature !== filterSignature) return;
      tracks = result.items;
      total = result.total;
      swrSet(cacheKey(signature), result);
    } catch (cause) {
      if (seq === queryGeneration && tracks.length === 0) error = String(cause);
    } finally {
      if (seq === queryGeneration) loading = false;
    }
  }

  async function loadMore() {
    if (loading || !hasMore || yearsInvalid) return;
    const seq = queryGeneration;
    const signature = JSON.stringify(activeQuery);
    loading = true;
    try {
      const result = await apiDiscovery.querySmartView(activeQuery, tracks.length, PAGE_SIZE);
      if (seq !== queryGeneration || signature !== JSON.stringify(activeQuery)) return;
      const known = new Set(tracks.map((track) => track.id));
      tracks = [...tracks, ...result.items.filter((track) => !known.has(track.id))];
      total = result.total;
      error = null;
    } catch (cause) {
      if (seq === queryGeneration) error = String(cause);
    } finally {
      if (seq === queryGeneration) loading = false;
    }
  }

  $effect(() => {
    const signature = filterSignature;
    // Validity changes re-run the query too: a typed 0 cleans to the same
    // signature as 1, and the old results must not stay up under the error.
    void yearsInvalid;
    const timer = setTimeout(() => untrack(() => startQuery(signature)), QUERY_DELAY_MS);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    if (library.revision > 0) {
      const signature = untrack(() => filterSignature);
      untrack(() => startQuery(signature));
    }
  });

  function setNumber(field: "yearFrom" | "yearTo" | "minPlayCount", event: Event) {
    const value = (event.currentTarget as HTMLInputElement).valueAsNumber;
    filter = { ...filter, [field]: Number.isFinite(value) ? value : null };
  }

  function toggleGenre(id: string) {
    filter = {
      ...filter,
      genreIds: filter.genreIds.includes(id)
        ? filter.genreIds.filter((genreId) => genreId !== id)
        : [...filter.genreIds, id],
    };
  }

  function selectView(view: SavedSmartView) {
    activeId = view.id;
    name = view.name;
    filter = cloneSmartFilter(view.filter);
  }

  function newView() {
    activeId = null;
    name = "";
    filter = emptySmartFilter();
  }

  function saveView() {
    const trimmed = name.trim();
    if (!trimmed) return;
    const now = new Date().toISOString();
    if (activeId) {
      savedViews = savedViews.map((view) =>
        view.id === activeId
          ? { ...view, name: trimmed, filter: cloneSmartFilter(filter), updatedAt: now }
          : view,
      );
    } else {
      const created = newSmartView(trimmed, filter);
      savedViews = [...savedViews, created];
      activeId = created.id;
    }
    name = trimmed;
    persistSmartViews(session.info, savedViews);
    toast.show(m.smart_saved({ name: trimmed }));
  }

  async function deleteView(view: SavedSmartView) {
    const accepted = await confirm.ask({
      title: m.smart_delete_title(),
      body: m.smart_delete_body({ name: view.name }),
      confirmLabel: m.smart_delete_action(),
      danger: true,
    });
    if (!accepted) return;
    savedViews = savedViews.filter((candidate) => candidate.id !== view.id);
    persistSmartViews(session.info, savedViews);
    if (activeId === view.id) newView();
  }

  async function materialize() {
    const trimmed = name.trim();
    if (!trimmed || materializing || yearsInvalid) return;
    materializing = true;
    try {
      const playlistId = await apiDiscovery.materializeSmartView(trimmed, cloneSmartFilter(filter));
      toast.show(m.smart_materialized({ name: trimmed }));
      await goto(`/playlist/${playlistId}`);
    } catch (cause) {
      error = String(cause);
    } finally {
      materializing = false;
    }
  }

  async function playTrack(track: TrackDto) {
    if (!track.albumId) {
      player.run(apiDiscovery.playTracks([track.id], false));
      return;
    }
    try {
      const album = await api.getAlbum(track.albumId);
      const index = album.tracks.findIndex((candidate) => candidate.id === track.id);
      await api.playAlbum(track.albumId, Math.max(0, index));
    } catch (cause) {
      player.error = String(cause);
    }
  }
</script>

<div class="p-6">
  <div class="mb-5">
    <h1 class="text-2xl font-bold tracking-tight">{m.nav_smart_views()}</h1>
    <p class="mt-1 max-w-3xl text-sm text-ink-muted">{m.smart_subtitle()}</p>
  </div>

  <div class="grid items-start gap-5 xl:grid-cols-[17rem_minmax(0,1fr)]">
    <aside class="rounded-xl border border-edge bg-card p-3">
      <div class="mb-3 flex items-center gap-2 px-1">
        <h2 class="mr-auto text-sm font-bold">{m.smart_saved_title()}</h2>
        <button
          class="flex h-8 w-8 items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
          onclick={newView}
          aria-label={m.smart_new()}
          title={m.smart_new()}
        >
          <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
            <path d="M11 5h2v14h-2V5zm-6 6h14v2H5v-2z" />
          </svg>
        </button>
      </div>
      {#if savedViews.length === 0}
        <p class="rounded-lg border border-dashed border-edge px-3 py-4 text-xs leading-relaxed text-ink-muted">
          {m.smart_saved_empty()}
        </p>
      {:else}
        <div class="space-y-1">
          {#each savedViews as view (view.id)}
            <div
              class="group flex items-center gap-1 rounded-lg border px-2 py-1.5 transition-colors
                {activeId === view.id ? 'border-accent bg-accent/10' : 'border-transparent hover:bg-panel-2'}"
            >
              <button
                class="flex min-w-0 flex-1 items-center gap-2 text-left"
                onclick={() => selectView(view)}
                aria-pressed={activeId === view.id}
              >
                <span class="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-accent/15 text-accent">
                  <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                    <path d="m12 2 1.45 4.55L18 8l-4.55 1.45L12 14l-1.45-4.55L6 8l4.55-1.45L12 2zm6 11 .93 3.07L22 17l-3.07.93L18 21l-.93-3.07L14 17l3.07-.93L18 13zM5 12l1.14 3.86L10 17l-3.86 1.14L5 22l-1.14-3.86L0 17l3.86-1.14L5 12z" />
                  </svg>
                </span>
                <span class="min-w-0">
                  <span class="block truncate text-sm font-semibold">{view.name}</span>
                  <span class="block truncate text-[0.65rem] font-bold uppercase tracking-wide text-accent">
                    {m.smart_view_badge()}
                  </span>
                </span>
              </button>
              <button
                class="invisible rounded p-1 text-ink-muted hover:text-red-400 group-hover:visible group-focus-within:visible"
                onclick={() => deleteView(view)}
                aria-label={m.smart_delete()}
                title={m.smart_delete()}
              >
                <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                  <path d="M6 7h12l-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 7zm3-3h6l1 2H8l1-2z" />
                </svg>
              </button>
            </div>
          {/each}
        </div>
      {/if}
    </aside>

    <div class="min-w-0">
      <section class="rounded-xl border border-edge bg-card p-4">
        <div class="flex flex-wrap items-end gap-3">
          <label class="min-w-56 flex-1 text-xs font-semibold text-ink-muted">
            {m.smart_name_label()}
            <input
              class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none transition-colors placeholder:text-ink-muted focus:border-accent"
              bind:value={name}
              placeholder={m.smart_name_placeholder()}
            />
          </label>
          <button
            class="rounded-full border border-edge px-4 py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
            onclick={newView}
          >
            {m.smart_new()}
          </button>
          <button
            class="rounded-full bg-accent px-4 py-2 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-40"
            onclick={saveView}
            disabled={!name.trim()}
          >
            {m.smart_save()}
          </button>
        </div>

        <h2 class="mt-6 mb-3 text-sm font-bold">{m.smart_filters_title()}</h2>
        <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
          <label class="text-xs font-semibold text-ink-muted">
            {m.smart_year_from()}
            <input
              type="number"
              min="1"
              max="9999"
              value={filter.yearFrom ?? ""}
              oninput={(event) => setNumber("yearFrom", event)}
              aria-invalid={yearsInvalid}
              class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
          </label>
          <label class="text-xs font-semibold text-ink-muted">
            {m.smart_year_to()}
            <input
              type="number"
              min="1"
              max="9999"
              value={filter.yearTo ?? ""}
              oninput={(event) => setNumber("yearTo", event)}
              aria-invalid={yearsInvalid}
              class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
          </label>
          <label class="text-xs font-semibold text-ink-muted">
            {m.smart_played()}
            <select
              value={filter.played}
              onchange={(event) =>
                (filter = {
                  ...filter,
                  played: (event.currentTarget as HTMLSelectElement).value as SmartPlayed,
                })}
              class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            >
              <option value="all">{m.smart_played_all()}</option>
              <option value="played">{m.smart_played_yes()}</option>
              <option value="unplayed">{m.smart_played_no()}</option>
            </select>
          </label>
          <label class="text-xs font-semibold text-ink-muted">
            {m.smart_min_play_count()}
            <input
              type="number"
              min="0"
              value={filter.minPlayCount ?? ""}
              oninput={(event) => setNumber("minPlayCount", event)}
              class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
          </label>
          <label class="text-xs font-semibold text-ink-muted md:col-span-1">
            {m.smart_added_since()}
            <input
              type="date"
              value={filter.addedSince ?? ""}
              oninput={(event) =>
                (filter = {
                  ...filter,
                  addedSince: (event.currentTarget as HTMLInputElement).value || null,
                })}
              class="mt-1.5 w-full rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm text-ink outline-none focus:border-accent"
            />
          </label>
          <label class="flex items-center gap-2 self-end rounded-md border border-edge bg-panel-2 px-3 py-2 text-sm font-semibold">
            <input
              type="checkbox"
              checked={filter.favoriteOnly}
              onchange={(event) =>
                (filter = {
                  ...filter,
                  favoriteOnly: (event.currentTarget as HTMLInputElement).checked,
                })}
              class="h-4 w-4 accent-(--color-accent)"
            />
            {m.smart_favorites_only()}
          </label>
        </div>
        {#if yearProblem}
          <p class="mt-2 text-xs font-semibold text-red-400">{YEAR_PROBLEM_MESSAGES[yearProblem]()}</p>
        {/if}

        <fieldset class="mt-5">
          <legend class="mb-2 text-xs font-semibold text-ink-muted">{m.smart_genres()}</legend>
          {#if genresError}
            <p class="mb-2 text-xs text-red-400">{m.error_generic({ message: genresError })}</p>
          {/if}
          {#if genres?.length}
            <div class="flex max-h-32 flex-wrap gap-1.5 overflow-y-auto pr-1">
              {#each genres as genre (genre.id)}
                <button
                  type="button"
                  class="rounded-full border px-3 py-1 text-xs font-semibold transition-colors
                    {filter.genreIds.includes(genre.id)
                      ? 'border-accent bg-accent text-(--color-on-accent)'
                      : 'border-edge text-ink-muted hover:border-ink-muted hover:text-ink'}"
                  onclick={() => toggleGenre(genre.id)}
                  aria-pressed={filter.genreIds.includes(genre.id)}
                >
                  {genre.name}
                </button>
              {/each}
            </div>
          {:else if genres}
            <p class="text-xs text-ink-muted">{m.smart_genres_empty()}</p>
          {/if}
        </fieldset>
      </section>

      <section class="mt-5">
        <div class="mb-3 flex flex-wrap items-center gap-2">
          <h2 class="mr-auto text-lg font-bold tracking-tight">{m.smart_results()}</h2>
          {#if total !== null}
            <span class="mr-2 text-xs font-medium text-ink-muted">
              {(total === 1 ? m.smart_tracks_one : m.smart_tracks_many)({ count: total })}
            </span>
          {/if}
          <button
            class="rounded-full border border-edge px-3 py-1.5 text-xs font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
            onclick={() => player.run(apiDiscovery.playSmartView(cloneSmartFilter(filter), false))}
            disabled={!hasResults || yearsInvalid}
          >
            {m.discover_play()}
          </button>
          <button
            class="rounded-full border border-edge px-3 py-1.5 text-xs font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
            onclick={() => player.run(apiDiscovery.playSmartView(cloneSmartFilter(filter), true))}
            disabled={!hasResults || yearsInvalid}
          >
            {m.discover_shuffle()}
          </button>
          <button
            class="rounded-full bg-accent px-3 py-1.5 text-xs font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-40"
            onclick={materialize}
            disabled={!name.trim() || !hasResults || materializing || yearsInvalid}
          >
            {materializing ? m.smart_materializing() : m.smart_materialize()}
          </button>
        </div>

        {#if error}
          <div class="mb-3 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
            <span>{m.error_generic({ message: error })}</span>
            <button class="ml-auto font-bold underline" onclick={() => startQuery(filterSignature)}>
              {m.discover_retry()}
            </button>
          </div>
        {/if}

        {#if tracks.length > 0}
          <div class="overflow-hidden rounded-lg border border-edge">
            <table class="w-full border-collapse text-sm">
              <tbody>
                {#each tracks as track (track.id)}
                  <tr
                    data-list-row
                    class="group cursor-pointer outline-none transition-colors hover:bg-ink/10 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent"
                    tabindex="0"
                    role="button"
                    aria-label={track.name}
                    onclick={() => playTrack(track)}
                    onkeydown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        playTrack(track);
                      }
                    }}
                  >
                    <td class="w-12 rounded-l-md py-1.5 pl-2">
                      <div class="w-9">
                        <Cover
                          itemId={track.imageItemId}
                          tag={track.imageTag}
                          size={96}
                          alt=""
                          blurhash={track.imageBlurHash}
                        />
                      </div>
                    </td>
                    <td class="min-w-0 px-3 py-2">
                      <p class="truncate font-medium text-ink">{track.name}</p>
                      <p class="truncate text-xs text-ink-muted">{track.artist}</p>
                    </td>
                    <td class="hidden max-w-64 truncate px-3 py-2 text-ink-muted lg:table-cell">
                      {track.album}
                    </td>
                    <td class="hidden w-28 px-3 py-2 text-right text-xs text-ink-muted xl:table-cell">
                      {(track.playCount === 1 ? m.smart_play_count_one : m.smart_play_count_many)({
                        count: track.playCount,
                      })}
                    </td>
                    <td class="w-16 rounded-r-md px-3 py-2 text-right text-ink-muted tabular-nums">
                      {formatDuration(track.durationMs)}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {:else if loading}
          <div class="space-y-1 rounded-lg border border-edge p-2" aria-label={m.discover_loading()}>
            {#each Array(8) as _, index (index)}
              <div class="skeleton h-12 rounded-md"></div>
            {/each}
          </div>
        {:else if !error && !yearsInvalid}
          <div class="rounded-lg border border-dashed border-edge">
            <EmptyState
              icon="filter"
              title={m.smart_empty()}
              action={{ label: m.empty_cta_reset_filters(), onclick: () => (filter = emptySmartFilter()) }}
              compact
            />
          </div>
        {/if}

        {#if hasMore && tracks.length > 0}
          <div class="mt-5 flex justify-center">
            <button
              class="rounded-full border border-edge px-5 py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
              onclick={loadMore}
              disabled={loading}
            >
              {loading ? m.discover_loading() : m.discover_load_more()}
            </button>
          </div>
        {/if}
      </section>
    </div>
  </div>
</div>
