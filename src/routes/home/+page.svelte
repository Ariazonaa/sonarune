<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import AlbumContextMenu from "$lib/components/AlbumContextMenu.svelte";
  import CardSkeleton from "$lib/components/CardSkeleton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { createAlbumMenu } from "$lib/state/albumMenu.svelte";
  import { homeRows, type HomeRowId } from "$lib/state/homeRows.svelte";
  import { startDrag } from "$lib/dragToPlaylist";
  import { contextMenuKey } from "$lib/menu";
  import { swrGet, swrSet } from "$lib/swr";
  import type { AlbumDto, HomeData } from "$lib/types";

  const am = createAlbumMenu();

  // Show the cached rows instantly on revisit, then refresh in the background.
  let home = $state<HomeData | null>(swrGet<HomeData>("home") ?? null);
  let error = $state<string | null>(null);

  // Guards against an older response overwriting a newer one: `load()` runs on
  // mount and again on every library revision, so two fetches can be in
  // flight and finish out of order.
  let requestId = 0;

  function load() {
    const id = ++requestId;
    apiLibrary
      .getHome()
      .then((d) => {
        if (id !== requestId) return;
        swrSet("home", d);
        home = d;
        error = null;
      })
      .catch((e) => {
        if (id !== requestId) return;
        if (!home) error = String(e);
      });
  }

  onMount(load);

  // Refetch when the server library changes (new music adds a "recently
  // added" entry).
  $effect(() => {
    if (library.revision > 0) load();
  });

  // The headings, by the field each row reads. The order and which of them
  // show at all is the user's (homeRows); a row the server has nothing for
  // still drops out, because an empty row is noise, not a setting.
  const ROW_TITLE: Record<HomeRowId, () => string> = {
    recentlyPlayed: m.home_recently_played,
    recentlyAdded: m.home_recently_added,
    mostPlayed: m.home_most_played,
    forgottenFavorites: m.home_forgotten_favorites,
  };

  const rows = $derived.by(() => {
    const data = home;
    if (!data) return [];
    return homeRows.visible
      .map((id) => ({ id, title: ROW_TITLE[id](), albums: data[id] }))
      .filter((row) => row.albums.length > 0);
  });
</script>

{#snippet albumCard(album: AlbumDto)}
  <a
    href="/album/{album.id}"
    draggable="true"
    ondragstart={(e) => startDrag(e, { albumId: album.id, label: album.name })}
    oncontextmenu={(e) => am.open(e, album)}
    {@attach contextMenuKey}
    class="group w-44 shrink-0 rounded-lg bg-card p-3 transition-colors duration-200 hover:bg-panel-2"
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
    <p class="mt-0.5 truncate text-xs font-medium text-ink-muted">
      {album.artist}{album.year ? ` · ${album.year}` : ""}
    </p>
  </a>
{/snippet}

<div class="p-6">
  <h1 class="mb-5 text-2xl font-bold tracking-tight">{m.nav_home()}</h1>

  {#if error}
    <p class="mb-4 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if home && rows.length === 0}
    <EmptyState
      icon="home"
      title={m.home_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
    />
  {:else if !home}
    {#each [0, 1] as section (section)}
      <section class="mb-8">
        <div class="skeleton mb-3 h-5 w-40 rounded"></div>
        <div class="flex gap-4 overflow-hidden pb-2">
          {#each Array(6) as _, i (i)}
            <div class="w-44 shrink-0"><CardSkeleton /></div>
          {/each}
        </div>
      </section>
    {/each}
  {/if}

  {#each rows as row, position (row.id)}
    <section class="group/row mb-8">
      <div class="mb-3 flex items-center gap-1">
        <h2 class="text-lg font-bold tracking-tight">{row.title}</h2>
        <!-- Arranging the page is a rare act, so the controls stay out of the
             way until the row is under the pointer or holds the focus. -->
        <span
          class="flex items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/row:opacity-100"
        >
          <button
            class="rounded p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-30 disabled:hover:bg-transparent"
            disabled={position === 0}
            onclick={() => homeRows.moveAmongVisible(row.id, -1)}
            aria-label={m.home_row_move_up()}
            title={m.home_row_move_up()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M12 8l6 6H6l6-6z" />
            </svg>
          </button>
          <button
            class="rounded p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-30 disabled:hover:bg-transparent"
            disabled={position === rows.length - 1}
            onclick={() => homeRows.moveAmongVisible(row.id, 1)}
            aria-label={m.home_row_move_down()}
            title={m.home_row_move_down()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M12 16l-6-6h12l-6 6z" />
            </svg>
          </button>
          <button
            class="rounded p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
            onclick={() => homeRows.toggle(row.id)}
            aria-label={m.home_row_hide()}
            title={m.home_row_hide_hint()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path
                d="M12 7c-4.4 0-8 4.1-8 5s3.6 5 8 5 8-4.1 8-5-3.6-5-8-5zm0 8a3 3 0 1 1 0-6 3 3 0 0 1 0 6zM3.3 2.3 1.9 3.7l18.4 18.4 1.4-1.4L3.3 2.3z"
              />
            </svg>
          </button>
        </span>
      </div>
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each row.albums as album (album.id)}
          {@render albumCard(album)}
        {/each}
      </div>
    </section>
  {/each}
</div>

<AlbumContextMenu menu={am} />
