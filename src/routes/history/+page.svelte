<script lang="ts">
  import { onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { api, formatDuration } from "#lib/api.js";
  import { apiLibrary } from "#lib/api/library.js";
  import { m } from "#lib/paraglide/messages.js";
  import { getLocale } from "#lib/paraglide/runtime.js";
  import Cover from "#lib/components/Cover.svelte";
  import EmptyState from "#lib/components/EmptyState.svelte";
  import { confirm } from "#lib/state/confirm.svelte.js";
  import { player } from "#lib/state/player.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import { dayLabel, groupTimeline, msUntilTomorrow, type TimelineRow } from "#lib/historyTimeline.js";
  import { playInAlbum } from "#lib/playInAlbum.js";
  import type { HistoryEntry, HistoryTrack, HistoryYears } from "#lib/types.js";

  const PAGE_SIZE = 100;

  let entries = $state<HistoryEntry[]>([]);
  let years = $state<HistoryYears | null>(null);
  let recording = $state(true);
  let loading = $state(false);
  /** The last page came back short: there is nothing older. */
  let exhausted = $state(false);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  /** What "Today" and "Yesterday" are counted from. The page stays open for
   *  hours (in the tray, too): it moves on at midnight and with every reload. */
  let now = $state(new Date());
  let generation = 0;

  async function load(reset: boolean) {
    if (loading && !reset) return;
    const seq = reset ? ++generation : generation;
    loading = true;
    error = null;
    if (reset) now = new Date();
    try {
      const before = reset ? null : (entries.at(-1)?.id ?? null);
      const page = await apiLibrary.historyPage(before, PAGE_SIZE);
      if (seq !== generation) return;
      entries = reset ? page : [...entries, ...page];
      exhausted = page.length < PAGE_SIZE;
      if (reset) years = await apiLibrary.historyYears();
    } catch (e) {
      if (seq === generation) error = String(e);
    } finally {
      if (seq === generation) {
        loading = false;
        loaded = true;
      }
    }
  }

  onMount(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    void load(true);
    api
      .getExtrasSettings()
      .then((extras) => {
        if (!cancelled) recording = extras.recordHistory;
      })
      .catch(() => {});
    // A listen that just ended lands at the top; reloading the first page
    // also drops what was paged in below, which is fine for a page you are
    // reading from the top.
    void listen("history:changed", () => {
      if (!cancelled && entries.length <= PAGE_SIZE) void load(true);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    // A hidden or minimised window's WebView is suspended, and the listens
    // that ended meanwhile may never have reached it: catch up once it shows
    // again (the player store resyncs the same way).
    const onVisible = () => {
      if (document.visibilityState !== "visible") return;
      if (entries.length <= PAGE_SIZE) void load(true);
      else now = new Date();
    };
    document.addEventListener("visibilitychange", onVisible);
    let midnight: ReturnType<typeof setTimeout> | undefined;
    const untilTomorrow = () => {
      midnight = setTimeout(() => {
        now = new Date();
        untilTomorrow();
      }, msUntilTomorrow(new Date()));
    };
    untilTomorrow();
    return () => {
      cancelled = true;
      unlisten?.();
      document.removeEventListener("visibilitychange", onVisible);
      clearTimeout(midnight);
    };
  });

  const days = $derived(groupTimeline(entries));
  const locale = $derived(getLocale());
  const time = $derived(new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" }));

  function playTrack(track: HistoryTrack) {
    player.run(playInAlbum(track.itemId, track.albumId));
  }

  async function forget(row: TimelineRow) {
    try {
      for (const entry of row.entries) await apiLibrary.historyDelete(entry.id);
      const gone = new Set(row.entries.map((entry) => entry.id));
      entries = entries.filter((entry) => !gone.has(entry.id));
      years = await apiLibrary.historyYears();
    } catch (e) {
      player.error = String(e);
    }
  }

  async function clearAll() {
    const ok = await confirm.ask({
      title: m.history_clear_title(),
      body: m.history_clear_body(),
      confirmLabel: m.history_clear_action(),
      danger: true,
    });
    if (!ok) return;
    try {
      await apiLibrary.historyClear();
      toast.show(m.history_cleared());
      void load(true);
    } catch (e) {
      player.error = String(e);
    }
  }

  const latestYear = $derived(years?.years[0] ?? null);
</script>

<div class="p-6">
  <div class="mb-1 flex flex-wrap items-center gap-3">
    <h1 class="mr-auto text-2xl font-bold tracking-tight">{m.history_title()}</h1>
    {#if latestYear !== null}
      <a
        href="/history/year/{latestYear}"
        class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover"
      >
        {m.history_review_link()}
      </a>
    {/if}
    {#if entries.length > 0}
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={clearAll}
      >
        {m.history_clear_action()}
      </button>
    {/if}
  </div>
  <p class="mb-6 max-w-2xl text-xs text-ink-muted">{m.history_note()}</p>

  {#if !recording}
    <p class="mb-6 max-w-2xl rounded-md border border-edge bg-card px-3 py-2 text-sm text-ink-muted">
      {m.history_recording_off()}
      <a href="/settings" class="font-semibold text-ink underline">{m.nav_settings()}</a>
    </p>
  {/if}

  {#if error}
    <div class="mb-4 flex items-center gap-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      <span>{m.error_generic({ message: error })}</span>
      <button class="ml-auto font-bold underline" onclick={() => load(entries.length === 0)}>
        {m.discover_retry()}
      </button>
    </div>
  {/if}

  {#if loaded && entries.length === 0 && !error}
    <EmptyState
      icon="stats"
      title={m.history_empty()}
      hint={recording ? m.history_empty_hint() : undefined}
      action={{ label: m.empty_cta_albums(), href: "/" }}
    />
  {/if}

  {#each days as day (day.key)}
    <section class="mb-6 max-w-3xl">
      <h2 class="mb-2 text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {dayLabel(day.date, now, { today: m.history_today(), yesterday: m.history_yesterday() }, locale)}
      </h2>
      <table class="w-full border-collapse text-sm">
        <tbody>
          {#each day.rows as row (row.entry.id)}
            <tr
              class="group cursor-pointer transition-colors hover:bg-ink/10"
              onclick={() => playTrack(row.entry.track)}
            >
              <td class="w-20 whitespace-nowrap rounded-l-md px-2 py-1.5 text-xs text-ink-muted tabular-nums">
                {time.format(new Date(row.entry.playedAt))}
              </td>
              <td class="w-11 py-1">
                <div class="w-9">
                  <Cover
                    itemId={row.entry.track.imageItemId}
                    tag={row.entry.track.imageTag}
                    size={96}
                    alt=""
                    blurhash={row.entry.track.imageBlurHash}
                  />
                </div>
              </td>
              <td class="px-3 py-1.5">
                <p class="truncate font-medium">{row.entry.track.name}</p>
                <p class="truncate text-xs text-ink-muted">
                  {row.entry.track.album
                    ? `${row.entry.track.artist} · ${row.entry.track.album}`
                    : row.entry.track.artist}
                </p>
              </td>
              <td class="w-14 px-2 py-1.5 text-right text-xs text-ink-muted tabular-nums">
                {#if row.entries.length > 1}
                  <span title={m.history_repeats({ count: row.entries.length })}>{row.entries.length}×</span>
                {/if}
              </td>
              <td class="w-16 px-2 py-1.5 text-right text-ink-muted tabular-nums">
                {formatDuration(row.entry.track.durationMs)}
              </td>
              <td class="w-9 rounded-r-md py-1.5 pr-2 text-right">
                <button
                  class="rounded-full p-1 text-ink-muted opacity-0 transition-opacity hover:text-ink focus-visible:opacity-100 group-hover:opacity-100"
                  title={m.history_forget()}
                  aria-label={m.history_forget()}
                  onclick={(event) => {
                    event.stopPropagation();
                    void forget(row);
                  }}
                >
                  <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                    <path
                      d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"
                    />
                  </svg>
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </section>
  {/each}

  {#if entries.length > 0 && !exhausted}
    <div class="mt-2 flex max-w-3xl justify-center">
      <button
        class="rounded-full border border-edge px-5 py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
        onclick={() => load(false)}
        disabled={loading}
      >
        {loading ? m.discover_loading() : m.discover_load_more()}
      </button>
    </div>
  {/if}
</div>
