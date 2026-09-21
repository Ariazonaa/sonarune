<script lang="ts">
  import { onMount } from "svelte";
  import { VList } from "virtua/svelte";
  import { afterNavigate, goto } from "$app/navigation";
  import { page } from "$app/state";
  import { api, formatDuration } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import type { AlbumDto, ArtistDto, SearchKind, SearchPage, TrackDto } from "$lib/types";

  const HISTORY_KEY = "jellysic:searchHistory";
  const HISTORY_MAX = 8;
  const PAGE_SIZE = 100;
  const LOAD_AHEAD_PX = 1_000;

  type SearchItem =
    | { kind: "artists"; value: ArtistDto }
    | { kind: "albums"; value: AlbumDto }
    | { kind: "tracks"; value: TrackDto };

  let term = $state("");
  let kind = $state<SearchKind>("tracks");
  let items = $state<SearchItem[]>([]);
  let total = $state(0);
  let error = $state<string | null>(null);
  let searching = $state(false);
  let loadingMore = $state(false);
  let history = $state<string[]>([]);
  let selectedIndex = $state(-1);
  let list = $state<VList<SearchItem> | null>(null);
  let input = $state<HTMLInputElement | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let requestId = 0;
  let mounted = false;

  const hasMore = $derived(items.length < total);
  const showHistory = $derived(!term.trim() && history.length > 0);
  const currentTrackId = $derived(player.state.current?.itemId);

  onMount(() => {
    mounted = true;
    try {
      const raw = localStorage.getItem(HISTORY_KEY);
      const parsed: unknown = raw ? JSON.parse(raw) : null;
      if (Array.isArray(parsed)) {
        history = parsed
          .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
          .slice(0, HISTORY_MAX);
      }
    } catch {
      /* corrupt entry — ignore */
    }
    applyUrl();
    return () => {
      mounted = false;
      clearTimeout(timer);
      requestId++;
    };
  });

  afterNavigate(() => {
    if (mounted) applyUrl();
  });

  function validKind(value: string | null): SearchKind {
    return value === "artists" || value === "albums" || value === "tracks" ? value : "tracks";
  }

  function applyUrl() {
    const nextTerm = page.url.searchParams.get("q") ?? "";
    const nextKind = validKind(page.url.searchParams.get("type"));
    // The URL holds the trimmed query (see startSearch), so our own URL sync
    // must not overwrite the input — that ate a trailing space mid-typing and
    // fetched the same results a second time.
    if (nextTerm.trim() === term.trim() && nextKind === kind) return;
    term = nextTerm;
    kind = nextKind;
    startSearch(false, null);
  }

  function syncUrl(query: string, nextKind: SearchKind, replaceState: boolean) {
    const url = new URL(page.url);
    if (query) url.searchParams.set("q", query);
    else url.searchParams.delete("q");
    url.searchParams.set("type", nextKind);
    if (url.href === page.url.href) return;
    goto(url, { replaceState, noScroll: true, keepFocus: true });
  }

  function rememberSearch(query: string) {
    const next = [query, ...history.filter((entry) => entry !== query)].slice(0, HISTORY_MAX);
    history = next;
    try {
      localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
    } catch {
      /* private mode / quota — history stays in-memory only */
    }
  }

  function clearHistory() {
    history = [];
    try {
      localStorage.removeItem(HISTORY_KEY);
    } catch {
      /* ignore */
    }
  }

  function unpack(result: SearchPage, resultKind: SearchKind): SearchItem[] {
    if (resultKind === "artists") {
      return result.artists.map((value) => ({ kind: "artists" as const, value }));
    }
    if (resultKind === "albums") {
      return result.albums.map((value) => ({ kind: "albums" as const, value }));
    }
    return result.tracks.map((value) => ({ kind: "tracks" as const, value }));
  }

  function onInput() {
    selectedIndex = -1;
    clearTimeout(timer);
    timer = setTimeout(() => startSearch(false, "replace"), 300);
  }

  function pickRecent(query: string) {
    term = query;
    clearTimeout(timer);
    startSearch(true, "push");
  }

  function selectKind(next: SearchKind) {
    if (next === kind) return;
    kind = next;
    selectedIndex = -1;
    clearTimeout(timer);
    startSearch(false, "push");
  }

  async function startSearch(record: boolean, navigation: "replace" | "push" | null) {
    const query = term.trim();
    const resultKind = kind;
    const id = ++requestId;
    items = [];
    total = 0;
    selectedIndex = -1;
    error = null;
    loadingMore = false;
    if (navigation) syncUrl(query, resultKind, navigation === "replace");
    if (!query) {
      searching = false;
      return;
    }
    if (record) rememberSearch(query);
    searching = true;
    try {
      const result = await api.searchPage(query, resultKind, 0, PAGE_SIZE);
      if (id !== requestId) return;
      items = unpack(result, resultKind);
      total = Math.max(0, result.total);
      queueMicrotask(maybeLoadMore);
    } catch (cause) {
      if (id === requestId) error = String(cause);
    } finally {
      if (id === requestId) searching = false;
    }
  }

  async function loadMore() {
    const query = term.trim();
    if (!query || searching || loadingMore || !hasMore) return false;
    const id = requestId;
    const resultKind = kind;
    const start = items.length;
    loadingMore = true;
    try {
      const result = await api.searchPage(query, resultKind, start, PAGE_SIZE);
      if (id !== requestId || resultKind !== kind || query !== term.trim()) return false;
      items = [...items, ...unpack(result, resultKind)];
      total = Math.max(0, result.total);
      return true;
    } catch (cause) {
      if (id === requestId) error = String(cause);
      return false;
    } finally {
      if (id === requestId) loadingMore = false;
    }
  }

  function maybeLoadMore() {
    if (!list || searching || loadingMore || !hasMore) return;
    const remaining = list.getScrollSize() - list.getScrollOffset() - list.getViewportSize();
    if (remaining < LOAD_AHEAD_PX) {
      loadMore().then((loaded) => {
        if (loaded) maybeLoadMore();
      });
    }
  }

  function moveSelection(delta: number) {
    if (items.length === 0) return;
    selectedIndex = Math.max(0, Math.min(items.length - 1, selectedIndex + delta));
    list?.scrollToIndex(selectedIndex, { align: "nearest" });
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      moveSelection(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      moveSelection(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      clearTimeout(timer);
      const selected = items[selectedIndex];
      if (selected) activate(selected);
      else startSearch(true, "push");
    } else if (event.key === "Escape") {
      event.preventDefault();
      clearTimeout(timer);
      term = "";
      startSearch(false, "push");
      input?.focus();
    }
  }

  async function playTrack(track: TrackDto) {
    if (!track.albumId) return;
    try {
      const detail = await api.getAlbum(track.albumId);
      const index = detail.tracks.findIndex((candidate) => candidate.id === track.id);
      await api.playAlbum(track.albumId, Math.max(0, index));
    } catch (cause) {
      player.error = String(cause);
    }
  }

  function activate(item: SearchItem) {
    if (item.kind === "artists") goto(`/artist/${item.value.id}`);
    else if (item.kind === "albums") goto(`/album/${item.value.id}`);
    else playTrack(item.value);
  }

  function itemKey(item: SearchItem): string {
    return `${item.kind}:${item.value.id}`;
  }

  function highlight(text: string, query: string): { text: string; hit: boolean }[] {
    const needle = query.trim().toLowerCase();
    if (!needle) return [{ text, hit: false }];
    const lower = text.toLowerCase();
    const parts: { text: string; hit: boolean }[] = [];
    let index = 0;
    while (index < text.length) {
      const hit = lower.indexOf(needle, index);
      if (hit === -1) {
        parts.push({ text: text.slice(index), hit: false });
        break;
      }
      if (hit > index) parts.push({ text: text.slice(index, hit), hit: false });
      parts.push({ text: text.slice(hit, hit + needle.length), hit: true });
      index = hit + needle.length;
    }
    return parts;
  }
</script>

{#snippet hl(text: string)}
  {#each highlight(text, term) as part, index (index)}
    {#if part.hit}<mark class="rounded-sm bg-accent/25 text-ink">{part.text}</mark>{:else}{part.text}{/if}
  {/each}
{/snippet}

<div class="flex h-full min-h-0 flex-col p-6 pb-0">
  <div class="shrink-0">
    <input
      bind:this={input}
      type="search"
      bind:value={term}
      oninput={onInput}
      onkeydown={onKeydown}
      placeholder={m.search_placeholder()}
      aria-label={m.nav_search()}
      aria-activedescendant={selectedIndex >= 0 ? `search-result-${selectedIndex}` : undefined}
      class="w-full max-w-2xl rounded-full bg-panel-2 px-5 py-3 text-sm font-medium outline-none placeholder:text-ink-muted focus:ring-2 focus:ring-ink/30"
      {@attach (node) => node.focus()}
    />
    <p class="mt-2 text-xs text-ink-muted">{m.search_keyboard_hint()}</p>

    <div class="mt-5 flex items-center gap-2" role="tablist" aria-label={m.nav_search()}>
      {#each ["tracks", "albums", "artists"] as tab (tab)}
        <button
          role="tab"
          aria-selected={kind === tab}
          class="rounded-full px-4 py-2 text-sm font-bold transition-colors
            {kind === tab ? 'bg-accent text-(--color-on-accent)' : 'bg-panel-2 text-ink-muted hover:text-ink'}"
          onclick={() => selectKind(tab as SearchKind)}
        >
          {tab === "tracks" ? m.search_tracks() : tab === "albums" ? m.search_albums() : m.search_artists()}
        </button>
      {/each}
      {#if term.trim() && !searching}
        <span class="ml-auto text-xs font-semibold text-ink-muted">{(total === 1 ? m.search_results_count_one : m.search_results_count_many)({ count: total })}</span>
      {/if}
    </div>
  </div>

  {#if error}
    <button
      class="mt-4 shrink-0 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-left text-sm text-red-300"
      onclick={() => (error = null)}
    >
      {m.error_generic({ message: error })}
    </button>
  {/if}

  {#if showHistory}
    <div class="mt-6 max-w-2xl shrink-0">
      <div class="mb-2 flex items-center justify-between">
        <h2 class="text-xs font-bold uppercase tracking-wide text-ink-muted">{m.search_recent()}</h2>
        <button class="text-xs font-semibold text-ink-muted hover:text-ink" onclick={clearHistory}>
          {m.search_clear_history()}
        </button>
      </div>
      <div class="flex flex-wrap gap-2">
        {#each history as query (query)}
          <button class="rounded-full bg-panel-2 px-3 py-1.5 text-sm font-medium hover:bg-card" onclick={() => pickRecent(query)}>
            {query}
          </button>
        {/each}
      </div>
    </div>
  {:else if searching}
    <div class="mt-6 h-1.5 w-full max-w-2xl overflow-hidden rounded-full bg-ink/10">
      <div class="h-full w-1/3 animate-pulse rounded-full bg-accent"></div>
    </div>
  {:else if term.trim() && items.length === 0}
    <EmptyState
      icon="search"
      title={m.search_no_results({ term: term.trim() })}
      hint={m.search_no_results_hint()}
    />
  {/if}

  {#if items.length > 0}
    <div class="mt-4 min-h-0 flex-1">
      <VList bind:this={list} data={items} getKey={itemKey} style="height: 100%;" onscroll={maybeLoadMore}>
        {#snippet children(item, index)}
          <button
            id={`search-result-${index}`}
            data-list-row
            class="flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left transition-colors
              {selectedIndex === index ? 'bg-ink/15 ring-1 ring-ink/20 ring-inset' : 'hover:bg-ink/10'}
              {item.kind === 'tracks' && item.value.id === currentTrackId ? 'text-accent' : ''}"
            onclick={() => activate(item)}
            onmouseenter={() => (selectedIndex = index)}
          >
            <div class="h-14 w-14 shrink-0 overflow-hidden {item.kind === 'artists' ? 'rounded-full' : 'rounded-md'}">
              {#if item.kind === "artists"}
                <Cover itemId={item.value.id} tag={item.value.imageTag} size={112} alt="" blurhash={item.value.imageBlurHash} round />
              {:else if item.kind === "albums"}
                <Cover itemId={item.value.id} tag={item.value.imageTag} size={112} alt="" blurhash={item.value.imageBlurHash} />
              {:else}
                <Cover itemId={item.value.imageItemId} tag={item.value.imageTag} size={112} alt="" blurhash={item.value.imageBlurHash} />
              {/if}
            </div>
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-semibold">{@render hl(item.value.name)}</p>
              {#if item.kind === "artists"}
                <p class="truncate text-xs text-ink-muted">{m.artist_eyebrow()}</p>
              {:else if item.kind === "albums"}
                <p class="truncate text-xs text-ink-muted">{item.value.artist}</p>
              {:else}
                <p class="truncate text-xs text-ink-muted">{item.value.artist} · {item.value.album}</p>
              {/if}
            </div>
            {#if item.kind === "tracks"}
              <span class="text-xs text-ink-muted tabular-nums">{formatDuration(item.value.durationMs)}</span>
            {/if}
          </button>
        {/snippet}
      </VList>
    </div>
  {/if}

  {#if loadingMore}
    <p class="shrink-0 py-2 text-center text-xs font-semibold text-ink-muted">{m.search_loading_more()}</p>
  {/if}
</div>
