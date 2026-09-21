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

  const rows = $derived(
    home
      ? [
          { title: m.home_recently_played(), albums: home.recentlyPlayed },
          { title: m.home_recently_added(), albums: home.recentlyAdded },
          { title: m.home_most_played(), albums: home.mostPlayed },
          { title: m.home_forgotten_favorites(), albums: home.forgottenFavorites },
        ].filter((row) => row.albums.length > 0)
      : [],
  );
</script>

{#snippet albumCard(album: AlbumDto)}
  <a
    href="/album/{album.id}"
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

  {#each rows as row (row.title)}
    <section class="mb-8">
      <h2 class="mb-3 text-lg font-bold tracking-tight">{row.title}</h2>
      <div data-card-row class="flex gap-4 overflow-x-auto pb-2">
        {#each row.albums as album (album.id)}
          {@render albumCard(album)}
        {/each}
      </div>
    </section>
  {/each}
</div>

<AlbumContextMenu menu={am} />
