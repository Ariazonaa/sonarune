<script lang="ts">
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { api, formatDuration } from "$lib/api";
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import CoverBackdrop from "$lib/components/CoverBackdrop.svelte";
  import ArtistLinks from "$lib/components/ArtistLinks.svelte";
  import AddToPlaylistMenu from "$lib/components/AddToPlaylistMenu.svelte";
  import { menuPoint } from "$lib/menu";
  import MetadataEditor from "$lib/components/MetadataEditor.svelte";
  import { player } from "$lib/state/player.svelte";
  import { session } from "$lib/state/session.svelte";
  import { confirm } from "$lib/state/confirm.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { library } from "$lib/state/library.svelte";
  import { swrGet, swrSet, swrDelete } from "$lib/swr";
  import type { AlbumDetail, TrackDto } from "$lib/types";

  let detail = $state<AlbumDetail | null>(null);
  let error = $state<string | null>(null);
  let addTo = $state<{ x: number; y: number; trackIds: string[] } | null>(null);
  let edit = $state<{ itemId: string; name: string } | null>(null);

  const albumId = $derived(page.params.id!);
  const currentTrackId = $derived(player.state.current?.itemId);

  // Re-fetch after a metadata edit so the album/track rows reflect the change.
  function reloadAlbum() {
    const id = albumId;
    api
      .getAlbum(id)
      .then((d) => {
        swrSet(`album:${id}`, d);
        if (id === page.params.id) detail = d;
      })
      .catch(() => {});
  }

  $effect(() => {
    const id = albumId;
    const key = `album:${id}`;
    // Show the cached album immediately, then revalidate in the background.
    detail = swrGet<AlbumDetail>(key) ?? null;
    error = null;
    api
      .getAlbum(id)
      .then((d) => {
        swrSet(key, d);
        if (id === page.params.id) detail = d;
      })
      .catch((e) => {
        if (id !== page.params.id) return;
        const msg = String(e);
        // Removed on the server: drop the cached ghost so its Play button
        // doesn't linger. Otherwise (offline/transient) keep the stale copy.
        if (msg.includes("returned 404")) {
          swrDelete(key);
          detail = null;
          error = msg;
        } else if (!detail) {
          error = msg;
        }
      });
  });

  const trackCount = $derived(detail?.tracks.length ?? 0);

  type TrackRow =
    | { kind: "disc"; disc: number }
    | { kind: "track"; track: TrackDto; index: number };

  // Group tracks under "Disc N" headers when the album spans multiple discs.
  // Tracks keep their global index so play/enqueue stay correct.
  const trackRows = $derived.by((): TrackRow[] => {
    const tracks = detail?.tracks ?? [];
    const multiDisc = new Set(tracks.map((t) => t.discNumber ?? 1)).size > 1;
    const rows: TrackRow[] = [];
    let lastDisc: number | null = null;
    tracks.forEach((track, index) => {
      const disc = track.discNumber ?? 1;
      if (multiDisc && disc !== lastDisc) {
        rows.push({ kind: "disc", disc });
        lastDisc = disc;
      }
      rows.push({ kind: "track", track, index });
    });
    return rows;
  });

  // Heart-shaped SVG paths (filled / outline).
  const HEART_ON =
    "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z";
  const HEART_OFF =
    "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z";

  // Optimistic favorite toggles: flip locally, revert if the server refuses.
  function toggleAlbumFavorite() {
    const d = detail;
    if (!d) return;
    const next = !d.album.isFavorite;
    d.album.isFavorite = next;
    apiLibrary.setFavorite(d.album.id, next).catch(() => {
      d.album.isFavorite = !next;
    });
  }

  function toggleTrackFavorite(track: TrackDto) {
    const next = !track.isFavorite;
    track.isFavorite = next;
    apiLibrary.setFavorite(track.id, next).catch(() => {
      track.isFavorite = !next;
    });
  }

  const TRASH =
    "M6 7h12l-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 7zm3-3h6l1 2H8l1-2z";

  // Delete the whole album (its folder/files) from the server, then leave.
  async function deleteAlbum() {
    const d = detail;
    if (!d) return;
    const ok = await confirm.ask({
      title: m.delete_confirm_title(),
      body: m.delete_album_confirm({ name: d.album.name }),
      confirmLabel: m.delete_action(),
      danger: true,
    });
    if (!ok) return;
    try {
      await apiLibrary.deleteItems([d.album.id]);
      swrDelete(`album:${d.album.id}`);
      toast.show(m.delete_done());
      library.poke(); // other views drop the album on their next revalidate
      goto("/");
    } catch (e) {
      player.error = String(e);
    }
  }

  // Delete a single track's file from the server; drop it from the view.
  async function deleteTrack(track: TrackDto) {
    const ok = await confirm.ask({
      title: m.delete_confirm_title(),
      body: m.delete_track_confirm({ name: track.name }),
      confirmLabel: m.delete_action(),
      danger: true,
    });
    if (!ok) return;
    try {
      await apiLibrary.deleteItems([track.id]);
      if (detail) detail.tracks = detail.tracks.filter((t) => t.id !== track.id);
      toast.show(m.delete_done());
    } catch (e) {
      player.error = String(e);
    }
  }
</script>

<div>
  {#if error}
    <p class="m-6 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {:else if detail}
    <header
      class="relative isolate flex items-end gap-6 overflow-hidden bg-gradient-to-b from-panel-2 to-panel p-6 pt-10"
    >
      <CoverBackdrop itemId={detail.album.id} tag={detail.album.imageTag} />
      <div class="w-48 shrink-0 shadow-2xl">
        <Cover
          itemId={detail.album.id}
          tag={detail.album.imageTag}
          size={480}
          alt={detail.album.name}
          blurhash={detail.album.imageBlurHash}
        />
      </div>
      <div class="min-w-0">
        <p class="text-xs font-bold uppercase tracking-wider">{m.album_eyebrow()}</p>
        <h1 class="mt-2 truncate text-5xl font-black tracking-tight">{detail.album.name}</h1>
        <p class="mt-4 text-sm font-medium text-ink-muted">
          <ArtistLinks
            artists={detail.album.artists}
            fallback={detail.album.artist}
            class="font-bold text-ink"
          />{detail.album.year ? ` · ${detail.album.year}` : ""}
          · {(trackCount === 1 ? m.album_tracks_one : m.album_tracks_many)({ count: trackCount })}
        </p>
      </div>
    </header>

    <div class="flex items-center gap-4 px-6 py-4">
      <button
        class="flex h-13 w-13 items-center justify-center rounded-full bg-accent text-(--color-on-accent) shadow-lg transition-all hover:scale-105 hover:bg-accent-hover"
        onclick={() => player.run(api.playAlbum(albumId, 0))}
        aria-label={m.album_play()}
        title={m.album_play()}
      >
        <svg viewBox="0 0 24 24" class="h-6 w-6" fill="currentColor" aria-hidden="true">
          <path d="M8 5v14l11-7L8 5z" />
        </svg>
      </button>
      <button
        class="rounded-full p-1.5 transition-colors {detail.album.isFavorite
          ? 'text-accent hover:text-accent-hover'
          : 'text-ink-muted hover:text-ink'}"
        onclick={toggleAlbumFavorite}
        aria-pressed={detail.album.isFavorite}
        aria-label={detail.album.isFavorite ? m.favorite_remove() : m.favorite_add()}
        title={detail.album.isFavorite ? m.favorite_remove() : m.favorite_add()}
      >
        <svg viewBox="0 0 24 24" class="h-7 w-7" fill="currentColor" aria-hidden="true">
          <path d={detail.album.isFavorite ? HEART_ON : HEART_OFF} />
        </svg>
      </button>
      <button
        class="rounded-full border border-ink-muted/50 px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink hover:text-ink"
        onclick={() => player.run(api.enqueueAlbum(albumId, true))}
      >
        {m.album_play_next()}
      </button>
      <button
        class="rounded-full border border-ink-muted/50 px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink hover:text-ink"
        onclick={() => player.run(api.enqueueAlbum(albumId, false))}
      >
        {m.album_add_to_queue()}
      </button>
      <button
        class="rounded-full border border-ink-muted/50 px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink hover:text-ink"
        aria-haspopup="menu"
        onclick={(e) => detail && (addTo = { ...menuPoint(e), trackIds: detail.tracks.map((t) => t.id) })}
      >
        {m.add_to_playlist()}
      </button>
      {#if session.info?.canEdit || session.info?.canDelete}
        <div class="ml-auto flex items-center gap-2">
          {#if session.info?.canEdit}
            <button
              class="flex items-center gap-1.5 rounded-full border border-ink-muted/50 px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink hover:text-ink"
              onclick={() => detail && (edit = { itemId: detail.album.id, name: detail.album.name })}
              title={m.edit_title()}
            >
              <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                <path d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z" />
              </svg>
              {m.edit_title()}
            </button>
          {/if}
          {#if session.info?.canDelete}
            <button
              class="flex items-center gap-1.5 rounded-full border border-red-500/40 px-4 py-1.5 text-sm font-bold text-red-400 transition-colors hover:border-red-500 hover:bg-red-500/10"
              onclick={deleteAlbum}
              title={m.delete_album()}
            >
              <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                <path d={TRASH} />
              </svg>
              {m.delete_album()}
            </button>
          {/if}
        </div>
      {/if}
    </div>
    <div class="px-6 pb-6">
      <table class="w-full border-collapse text-sm">
        <tbody>
          {#each trackRows as row (row.kind === "disc" ? `disc-${row.disc}` : row.track.id)}
            {#if row.kind === "disc"}
            <tr>
              <td colspan="4" class="px-2 pt-5 pb-1 text-xs font-bold uppercase tracking-wide text-ink-muted">
                {m.album_disc({ number: row.disc })}
              </td>
            </tr>
            {:else}
            {@const track = row.track}
            {@const i = row.index}
            <tr
              class="group cursor-pointer outline-none transition-colors hover:bg-ink/10 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent
                {track.id === currentTrackId ? 'text-accent' : ''}"
              tabindex="0"
              role="button"
              aria-label={track.name}
              onclick={() => player.run(api.playAlbum(albumId, i))}
              onkeydown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  player.run(api.playAlbum(albumId, i));
                }
              }}
            >
              <td class="w-10 rounded-l-md px-2 py-2 text-right text-ink-muted tabular-nums">
                {track.indexNumber ?? i + 1}
              </td>
              <td class="px-3 py-2">
                <p class="font-medium {track.id === currentTrackId ? '' : 'text-ink'}">{track.name}</p>
                <p class="text-xs text-ink-muted">
                  <ArtistLinks artists={track.artists} fallback={track.artist} />
                </p>
              </td>
              <td class="w-40 px-1 py-2">
                <span class="flex items-center justify-end gap-1">
                  <button
                    class="rounded p-1 {track.isFavorite
                      ? 'text-accent hover:text-accent-hover'
                      : 'invisible text-ink-muted group-hover:visible hover:text-ink'}"
                    onclick={(e) => {
                      e.stopPropagation();
                      toggleTrackFavorite(track);
                    }}
                    aria-pressed={track.isFavorite}
                    aria-label={track.isFavorite ? m.favorite_remove() : m.favorite_add()}
                    title={track.isFavorite ? m.favorite_remove() : m.favorite_add()}
                  >
                    <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
                      <path d={track.isFavorite ? HEART_ON : HEART_OFF} />
                    </svg>
                  </button>
                  <button
                    class="invisible rounded p-1 text-ink-muted group-hover:visible hover:text-ink"
                    onclick={(e) => {
                      e.stopPropagation();
                      player.run(api.enqueueTracks([track.id], true));
                    }}
                    aria-label={m.album_play_next()}
                    title={m.album_play_next()}
                  >
                    <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
                      <path d="M3 6h12v2H3V6zm0 4h12v2H3v-2zm0 4h8v2H3v-2zm14-4V6l6 4-6 4v-4z"/>
                    </svg>
                  </button>
                  <button
                    class="invisible rounded p-1 text-ink-muted group-hover:visible hover:text-ink"
                    onclick={(e) => {
                      e.stopPropagation();
                      player.run(api.enqueueTracks([track.id], false));
                    }}
                    aria-label={m.album_add_to_queue()}
                    title={m.album_add_to_queue()}
                  >
                    <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
                      <path d="M3 6h12v2H3V6zm0 4h12v2H3v-2zm0 4h8v2H3v-2zm15 0v-3h2v3h3v2h-3v3h-2v-3h-3v-2h3z"/>
                    </svg>
                  </button>
                  <button
                    class="invisible rounded p-1 text-ink-muted group-hover:visible hover:text-ink"
                    onclick={(e) => {
                      e.stopPropagation();
                      addTo = { ...menuPoint(e), trackIds: [track.id] };
                    }}
                    aria-haspopup="menu"
                    aria-label={m.add_to_playlist()}
                    title={m.add_to_playlist()}
                  >
                    <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
                      <path d="M14 10H3v2h11v-2zm0-4H3v2h11V6zm4 8v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zM3 16h7v-2H3v2z"/>
                    </svg>
                  </button>
                  {#if session.info?.canEdit}
                    <button
                      class="invisible rounded p-1 text-ink-muted group-hover:visible hover:text-ink"
                      onclick={(e) => {
                        e.stopPropagation();
                        edit = { itemId: track.id, name: track.name };
                      }}
                      aria-label={m.ctx_edit()}
                      title={m.ctx_edit()}
                    >
                      <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
                        <path d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04a1 1 0 0 0 0-1.41l-2.34-2.34a1 1 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z" />
                      </svg>
                    </button>
                  {/if}
                  {#if session.info?.canDelete}
                    <button
                      class="invisible rounded p-1 text-ink-muted group-hover:visible hover:text-red-400"
                      onclick={(e) => {
                        e.stopPropagation();
                        deleteTrack(track);
                      }}
                      aria-label={m.delete_from_server()}
                      title={m.delete_from_server()}
                    >
                      <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor">
                        <path d={TRASH} />
                      </svg>
                    </button>
                  {/if}
                </span>
              </td>
              <td class="w-16 rounded-r-md px-2 py-2 text-right text-ink-muted tabular-nums">
                {formatDuration(track.durationMs)}
              </td>
            </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    </div>
  {:else}
    <div class="p-6">
      <div class="flex items-end gap-6">
        <div class="skeleton h-48 w-48 shrink-0 rounded-xl"></div>
        <div class="flex-1 space-y-3 pb-2">
          <div class="skeleton h-3 w-16 rounded"></div>
          <div class="skeleton h-10 w-2/3 rounded-lg"></div>
          <div class="skeleton h-4 w-40 rounded"></div>
        </div>
      </div>
      <div class="mt-8 space-y-2">
        {#each Array(8) as _, i (i)}
          <div class="skeleton h-10 w-full rounded-lg"></div>
        {/each}
      </div>
    </div>
  {/if}
</div>

{#if addTo}
  <AddToPlaylistMenu x={addTo.x} y={addTo.y} trackIds={addTo.trackIds} onclose={() => (addTo = null)} />
{/if}
{#if edit}
  <MetadataEditor itemId={edit.itemId} displayName={edit.name} onclose={() => (edit = null)} onsaved={reloadAlbum} />
{/if}
