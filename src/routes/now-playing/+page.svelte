<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { m } from "$lib/paraglide/messages";
  import Cover from "$lib/components/Cover.svelte";
  import CoverBackdrop from "$lib/components/CoverBackdrop.svelte";
  import ArtistLinks from "$lib/components/ArtistLinks.svelte";
  import SpectrumBar from "$lib/components/SpectrumBar.svelte";
  import LyricsView from "$lib/components/LyricsView.svelte";
  import PlaybackProgress from "$lib/components/PlaybackProgress.svelte";
  import TransportControls from "$lib/components/TransportControls.svelte";
  import { api, coverUrl } from "$lib/api";
  import { ambientColors, type AmbientColors } from "$lib/ambient";
  import { isModalOpen } from "$lib/modal";
  import { nextEntries } from "$lib/queueOrder";
  import { lyrics } from "$lib/state/lyrics.svelte";
  import { player } from "$lib/state/player.svelte";
  import { VisualizerController } from "$lib/visualizer/controller";

  const MODES = ["ambient", "cover", "visualizer"] as const;
  type BackdropMode = (typeof MODES)[number];
  const MODE_KEY = "jellysic.nowplaying.backdrop";
  const MODE_LABELS: Record<BackdropMode, () => string> = {
    ambient: m.nowplaying_backdrop_ambient,
    cover: m.nowplaying_backdrop_cover,
    visualizer: m.nowplaying_backdrop_visualizer,
  };

  const storedMode = localStorage.getItem(MODE_KEY);
  let mode = $state<BackdropMode>(
    MODES.find((candidate) => candidate === storedMode) ?? "ambient",
  );
  const modeTitle = $derived(m.nowplaying_backdrop_mode({ mode: MODE_LABELS[mode]() }));
  let canvas = $state<HTMLCanvasElement | null>(null);
  let controller: VisualizerController | null = null;

  const current = $derived(player.state.current);
  const currentTrackId = $derived(current?.itemId);

  // The next few tracks in play order (the shuffle order while shuffle is
  // on), with their stored queue index for jumping.
  const upNext = $derived(nextEntries(player.queue, 3));

  const HEART_ON =
    "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z";
  const HEART_OFF =
    "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z";
  const coverSrc = $derived(
    current ? coverUrl(current.imageItemId, current.imageTag, 640) : null,
  );
  const hasLyrics = $derived(
    !!current &&
      lyrics.forTrackId === current.itemId &&
      (lyrics.lyrics?.lines.length ?? 0) > 0,
  );

  function close() {
    goto("/");
  }

  function toggleMode() {
    mode = MODES[(MODES.indexOf(mode) + 1) % MODES.length];
    localStorage.setItem(MODE_KEY, mode);
  }

  // Ambient backdrop colors follow the cover; null = plain panel fallback.
  let ambient = $state<AmbientColors | null>(null);
  $effect(() => {
    const src = coverSrc;
    if (!src) {
      ambient = null;
      return;
    }
    let stale = false;
    ambientColors(src).then((colors) => {
      if (!stale) ambient = colors;
    });
    return () => {
      stale = true;
    };
  });

  // Visualizer backdrop only lives while its mode is active.
  $effect(() => {
    if (mode !== "visualizer" || !canvas) return;
    const el = canvas;
    let cancelled = false;
    let observer: ResizeObserver | undefined;

    (async () => {
      try {
        const created = await VisualizerController.create(el, {
          onError: (message) => console.warn("now-playing backdrop:", message),
          onPreset: () => {},
          isCancelled: () => cancelled,
        });
        if (!created) return;
        if (cancelled) {
          created.destroy();
          return;
        }
        controller = created;
        // The backdrop always follows the music on its own.
        controller.setCycle("auto", false);
        observer = new ResizeObserver(() => {
          if (controller) controller.setSize(el.clientWidth, el.clientHeight);
        });
        observer.observe(el);
      } catch (e) {
        console.warn("now-playing backdrop failed:", e);
      }
    })();

    return () => {
      cancelled = true;
      observer?.disconnect();
      controller?.destroy();
      controller = null;
    };
  });

  // New track -> new preset for the backdrop too.
  let lastTrackId: string | undefined;
  $effect(() => {
    const id = currentTrackId;
    if (id && lastTrackId && id !== lastTrackId) {
      controller?.onTrackChange();
    }
    lastTrackId = id;
  });

  onMount(() => {
    const onKey = (event: KeyboardEvent) => {
      // An overlay (palette, shortcut help, a confirmation) owns Escape. It
      // either handled it already (defaultPrevented) or — opened after this
      // page registered its listener — only gets it after this handler.
      if (event.key === "Escape" && !event.defaultPrevented && !isModalOpen()) close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
</script>

<!-- isolate: the cover backdrop sits at -z-10 inside this view, not behind it. -->
<div class="relative isolate h-full min-h-0 overflow-hidden bg-base">
  {#if mode === "ambient"}
    <div class="absolute inset-0 overflow-hidden">
      <div class="ambient-blob blob-a" style:background-color={ambient?.a ?? "transparent"}></div>
      <div class="ambient-blob blob-b" style:background-color={ambient?.b ?? "transparent"}></div>
    </div>
  {:else if mode === "cover"}
    <CoverBackdrop
      itemId={current?.imageItemId ?? null}
      tag={current?.imageTag ?? null}
      strength={1}
      scrim={false}
    />
  {:else}
    <canvas bind:this={canvas} class="absolute inset-0 h-full w-full bg-black"></canvas>
  {/if}
  <!-- Dim layer keeps the track info readable over busy backdrops. -->
  <div class="absolute inset-0 bg-black/45"></div>

  <button
    class="absolute right-14 top-4 z-10 rounded-full bg-black/50 p-2 text-ink/70 transition-colors hover:text-ink"
    onclick={toggleMode}
    aria-label={modeTitle}
    title={modeTitle}
  >
    <!-- The icon shows the current backdrop; a click moves to the next. -->
    <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
      {#if mode === "ambient"}
        <path d="M12 2s-7 8.16-7 13a7 7 0 0 0 14 0c0-4.84-7-13-7-13zm0 18a5 5 0 0 1-5-5c0-2.9 3.32-7.7 5-9.83 1.68 2.13 5 6.93 5 9.83a5 5 0 0 1-5 5z" />
      {:else if mode === "cover"}
        <path d="M21 19V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2zM8.5 13.5l2.5 3 3.5-4.5 4.5 6H5l3.5-4.5z" />
      {:else}
        <path d="M3 13h2v4H3v-4zm4-6h2v10H7V7zm4 3h2v7h-2v-7zm4-5h2v12h-2V5zm4 8h2v4h-2v-4z" />
      {/if}
    </svg>
  </button>

  <button
    class="absolute right-4 top-4 z-10 rounded-full bg-black/50 p-2 text-ink/70 transition-colors hover:text-ink"
    onclick={close}
    aria-label={m.nowplaying_close()}
    title={m.nowplaying_close()}
  >
    <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
      <path d="M7.41 8.59 12 13.17l4.59-4.58L18 10l-6 6-6-6 1.41-1.41z" />
    </svg>
  </button>

  {#if current}
    <div
      class="relative z-[5] flex h-full min-h-0 flex-col items-center justify-center gap-6 p-8
        {hasLyrics ? 'lg:flex-row lg:gap-14' : ''}"
    >
      <div
        class="flex min-h-0 flex-col items-center gap-5
          {hasLyrics
          ? 'w-full max-w-sm shrink-0 lg:h-full lg:max-w-md lg:justify-center'
          : 'h-full w-full max-w-xl justify-center'}"
      >
        <!-- The art takes the height the controls leave (up to a cap), so it
             grows with the window without pushing them out of the view: a size
             container, with the square as large as both its sides allow. Below
             lg with lyrics the column has no fixed height, so the box gets one. -->
        <div
          class="flex w-full justify-center [container-type:size]
            {hasLyrics
            ? 'h-[min(20rem,34vh)] lg:h-auto lg:max-h-[26rem] lg:min-h-0 lg:flex-1'
            : 'max-h-[32rem] min-h-0 flex-1'}"
        >
          <div class="w-[min(100cqw,100cqh)] shadow-2xl">
            <Cover
              itemId={current.imageItemId}
              tag={current.imageTag}
              size={640}
              alt={current.name}
              blurhash={current.imageBlurHash}
            />
          </div>
        </div>
        <div class="w-full max-w-2xl shrink-0 text-center">
          <h1
            class="truncate font-black tracking-tight text-ink
              {hasLyrics ? 'text-2xl' : 'text-3xl'}"
          >
            {current.name}
          </h1>
          <p
            class="mt-2 truncate font-medium text-ink/70
              {hasLyrics ? 'text-md' : 'text-lg'}"
          >
            <ArtistLinks
              artists={current.artists}
              fallback={current.artist}
              class="text-ink/70"
            />{current.album ? ` · ${current.album}` : ""}
          </p>
          <button
            class="mt-3 rounded-full p-1.5 transition-colors {player.currentIsFavorite
              ? 'text-accent'
              : 'text-ink/70 hover:text-ink'}"
            onclick={() => player.toggleCurrentFavorite()}
            aria-pressed={player.currentIsFavorite}
            aria-label={player.currentIsFavorite ? m.favorite_remove() : m.favorite_add()}
            title={player.currentIsFavorite ? m.favorite_remove() : m.favorite_add()}
          >
            <svg viewBox="0 0 24 24" class="h-6 w-6" fill="currentColor" aria-hidden="true">
              <path d={player.currentIsFavorite ? HEART_ON : HEART_OFF} />
            </svg>
          </button>

          <PlaybackProgress size="lg" class="mt-3" />
          <div class="mt-2 flex justify-center">
            <TransportControls size="lg" />
          </div>

          <SpectrumBar class="mt-4 h-14 w-full opacity-90" />

          {#if !hasLyrics && upNext.length > 0}
            <!-- Low windows give its height to the art instead. -->
            <div class="mt-6 w-full text-left short:hidden">
              <p class="mb-2 text-[10px] font-bold uppercase tracking-wider text-ink/50">
                {m.queue_up_next()}
              </p>
              <ul class="space-y-1">
                {#each upNext as item (item.index)}
                  <li>
                    <button
                      class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left transition-colors hover:bg-ink/10"
                      onclick={() => player.run(api.queueJump(item.index))}
                    >
                      <div class="w-8 shrink-0">
                        <Cover itemId={item.track.imageItemId} tag={item.track.imageTag} size={96} alt="" blurhash={item.track.imageBlurHash} />
                      </div>
                      <div class="min-w-0">
                        <p class="truncate text-xs font-medium text-ink">{item.track.name}</p>
                        <p class="truncate text-[11px] text-ink/60">{item.track.artist}</p>
                      </div>
                    </button>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
        </div>
      </div>
      {#if hasLyrics}
        <div class="min-h-0 w-full max-w-2xl flex-1 lg:h-full lg:max-w-xl">
          <LyricsView variant="fullscreen" />
        </div>
      {/if}
    </div>
  {:else}
    <div class="relative z-[5] flex h-full items-center justify-center">
      <p class="text-sm text-ink/60">{m.player_nothing_playing()}</p>
    </div>
  {/if}
</div>

<style>
  /* Two huge blurred discs drifting slowly; color transition on track change. */
  .ambient-blob {
    position: absolute;
    width: 85vmax;
    height: 85vmax;
    border-radius: 50%;
    filter: blur(90px);
    transition: background-color 1500ms ease;
    will-change: transform;
  }
  .blob-a {
    top: -30%;
    left: -20%;
    animation: drift-a 26s ease-in-out infinite alternate;
  }
  .blob-b {
    right: -25%;
    bottom: -35%;
    animation: drift-b 32s ease-in-out infinite alternate;
  }
  @keyframes drift-a {
    from {
      transform: translate(0, 0) scale(1);
    }
    to {
      transform: translate(10%, 8%) scale(1.15);
    }
  }
  @keyframes drift-b {
    from {
      transform: translate(0, 0) scale(1.1);
    }
    to {
      transform: translate(-9%, -7%) scale(0.95);
    }
  }
</style>
