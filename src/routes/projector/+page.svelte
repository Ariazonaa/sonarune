<script lang="ts">
  import { onMount } from "svelte";
  import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { m } from "$lib/paraglide/messages";
  import { formatDuration } from "$lib/api";
  import Cover from "$lib/components/Cover.svelte";
  import { lyrics } from "$lib/state/lyrics.svelte";
  import { player } from "$lib/state/player.svelte";
  import type { LyricLineDto } from "$lib/types";
  import {
    REMOTE_PRESETS_KEY,
    VisualizerController,
    loadRemotePresetsSetting,
  } from "$lib/visualizer/controller";
  import {
    PROJECTOR_SETTINGS_EVENT,
    PROJECTOR_SETTINGS_KEY,
    burnInTransform,
    isOverlayVisible,
    loadProjectorSettings,
    normalizeProjectorSettings,
    saveProjectorSettings,
    type VisualizerOverlayKind,
    type VisualizerProjectorSettings,
  } from "$lib/visualizer/projector";

  let container = $state<HTMLElement | null>(null);
  let canvas = $state<HTMLCanvasElement | null>(null);
  let controller: VisualizerController | null = null;
  let settings = $state<VisualizerProjectorSettings>(loadProjectorSettings());
  let error = $state<string | null>(null);
  let presetName = $state("");
  let now = $state(performance.now());
  let positionMs = $state(0);
  let controlsVisible = $state(true);
  let controlTimer: ReturnType<typeof setTimeout> | undefined;
  let activatedAt = $state<Record<VisualizerOverlayKind, number>>({
    cover: performance.now(),
    track: performance.now(),
    progress: performance.now(),
    lyrics: performance.now(),
  });

  const s = $derived(player.state);
  const currentTrackId = $derived(s.current?.itemId ?? "");
  const driftStep = $derived(Math.floor(now / 30_000));
  const progressPercent = $derived(s.durationMs > 0
    ? Math.min(100, Math.max(0, (positionMs / s.durationMs) * 100))
    : 0);

  // The store picks up nudges made in the main window via `storage` events.
  const effectiveLyricsOffset = $derived((lyrics.lyrics?.offsetMs ?? 0) + lyrics.manualOffsetMs);

  const activeLyricIndex = $derived.by(() => {
    const data = lyrics.lyrics;
    if (!data?.synced || lyrics.forTrackId !== currentTrackId) return -1;
    let index = -1;
    for (let i = 0; i < data.lines.length; i++) {
      const start = data.lines[i].startMs;
      if (start != null && start - effectiveLyricsOffset <= positionMs) index = i;
    }
    return index;
  });
  const activeLyric = $derived(activeLyricIndex >= 0
    ? lyrics.lyrics?.lines[activeLyricIndex] ?? null
    : null);
  const nextLyric = $derived(activeLyricIndex >= 0
    ? lyrics.lyrics?.lines[activeLyricIndex + 1] ?? null
    : null);

  type Segment = { text: string; startMs: number | null };
  function lyricSegments(line: LyricLineDto): Segment[] {
    const cues = line.cues
      .filter((cue) => cue.position != null)
      .sort((a, b) => a.position! - b.position!);
    if (cues.length === 0) return [{ text: line.text, startMs: null }];
    const result: Segment[] = [];
    if (cues[0].position! > 0) {
      result.push({ text: line.text.slice(0, cues[0].position!), startMs: null });
    }
    for (let i = 0; i < cues.length; i++) {
      const end = i + 1 < cues.length ? cues[i + 1].position! : undefined;
      result.push({ text: line.text.slice(cues[i].position!, end), startMs: cues[i].startMs });
    }
    return result;
  }

  function layerVisible(kind: VisualizerOverlayKind): boolean {
    return isOverlayVisible(settings.overlays[kind], now, activatedAt[kind]);
  }

  function layerTransform(kind: VisualizerOverlayKind): string {
    return burnInTransform(
      kind,
      driftStep,
      currentTrackId,
      settings.burnInMovement,
      settings.reducedMotion,
    );
  }

  function activateOverlays() {
    const at = performance.now();
    activatedAt = { cover: at, track: at, progress: at, lyrics: at };
  }

  function poke() {
    controlsVisible = true;
    clearTimeout(controlTimer);
    controlTimer = setTimeout(() => (controlsVisible = false), 2_500);
    if (!settings.songChangeOnly) activateOverlays();
  }

  function persist(next: VisualizerProjectorSettings) {
    settings = normalizeProjectorSettings(next);
    saveProjectorSettings(settings);
    void emit(PROJECTOR_SETTINGS_EVENT, settings);
  }

  function toggleLock() {
    persist({ ...settings, presetLocked: !settings.presetLocked });
    controller?.setLocked(settings.presetLocked);
  }

  async function toggleFullscreen() {
    try {
      const window = getCurrentWindow();
      const fullscreen = !(await window.isFullscreen());
      await window.setFullscreen(fullscreen);
      persist({ ...settings, fullscreen });
    } catch (cause) {
      error = String(cause);
    }
  }

  async function closeProjector() {
    await getCurrentWindow().close();
  }

  let lastTrackId = "";
  $effect(() => {
    const id = currentTrackId;
    if (id !== lastTrackId) {
      activateOverlays();
      if (id && lastTrackId) controller?.onTrackChange(s.current?.name);
      lastTrackId = id;
    }
  });

  $effect(() => {
    controller?.setLocked(settings.presetLocked);
  });

  onMount(() => {
    let cancelled = false;
    let observer: ResizeObserver | undefined;
    let unlistenSettings: UnlistenFn | undefined;
    const onStorage = (event: StorageEvent) => {
      if (event.key === PROJECTOR_SETTINGS_KEY) settings = loadProjectorSettings();
      // The online-preset opt-in is toggled in the main window's visualizer.
      if (event.key === null || event.key === REMOTE_PRESETS_KEY) {
        controller?.setRemotePresetsEnabled(loadRemotePresetsSetting(), false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "F11") {
        event.preventDefault();
        void toggleFullscreen();
      }
    };

    window.addEventListener("storage", onStorage);
    window.addEventListener("keydown", onKey);
    const tick = setInterval(() => {
      now = performance.now();
      positionMs = player.estimatedPositionMs();
    }, 200);
    positionMs = player.estimatedPositionMs();

    void listen<VisualizerProjectorSettings>(PROJECTOR_SETTINGS_EVENT, (event) => {
      if (!cancelled) settings = normalizeProjectorSettings(event.payload);
    }).then((unlisten) => {
      if (cancelled) unlisten();
      else unlistenSettings = unlisten;
    });

    (async () => {
      try {
        if (!canvas) return;
        controller = await VisualizerController.create(canvas, {
          onError: (message) => (error = message),
          onPreset: (name) => (presetName = name),
          isCancelled: () => cancelled,
        });
        // See the visualizer route: teardown can land after the factory's
        // last cancellation check, and the controller owns GPU/audio
        // resources that would otherwise never be released.
        if (!controller) return;
        if (cancelled) {
          controller.destroy();
          controller = null;
          return;
        }
        controller.setLocked(settings.presetLocked);
        observer = new ResizeObserver(() => {
          if (canvas && controller) controller.setSize(canvas.clientWidth, canvas.clientHeight);
        });
        if (container) observer.observe(container);
      } catch (cause) {
        error = String(cause);
      }
    })();

    poke();
    return () => {
      cancelled = true;
      clearInterval(tick);
      clearTimeout(controlTimer);
      observer?.disconnect();
      unlistenSettings?.();
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("keydown", onKey);
      void controller?.destroy();
      controller = null;
    };
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  bind:this={container}
  class="relative h-screen w-screen overflow-hidden bg-black text-ink"
  onmousemove={poke}
  onpointerdown={poke}
>
  <canvas bind:this={canvas} class="absolute inset-0 h-full w-full"></canvas>

  {#if s.current && layerVisible("cover")}
    <div
      class="pointer-events-none absolute left-[6vw] top-[8vh] w-[min(22vw,320px)] drop-shadow-2xl"
      style:opacity={settings.overlays.cover.opacity}
      style:transform={layerTransform("cover")}
      style:transition={settings.reducedMotion ? "none" : "transform 20s ease-in-out, opacity 500ms"}
    >
      <Cover
        itemId={s.current.imageItemId}
        tag={s.current.imageTag}
        blurhash={s.current.imageBlurHash}
        size={640}
        alt=""
        class="shadow-2xl"
      />
    </div>
  {/if}

  {#if s.current && layerVisible("track")}
    <div
      class="pointer-events-none absolute bottom-[13vh] left-[7vw] max-w-[55vw] rounded-overlay bg-black/35 px-5 py-4 shadow-2xl backdrop-blur-sm"
      style:opacity={settings.overlays.track.opacity}
      style:transform={layerTransform("track")}
      style:transition={settings.reducedMotion ? "none" : "transform 20s ease-in-out, opacity 500ms"}
    >
      <p class="truncate text-[clamp(1.5rem,3vw,3.5rem)] font-black leading-tight tracking-tight">{s.current.name}</p>
      <p class="mt-1 truncate text-[clamp(1rem,1.7vw,2rem)] font-semibold text-ink/70">{s.current.artist}</p>
    </div>
  {/if}

  {#if activeLyric && layerVisible("lyrics")}
    <div
      class="pointer-events-none absolute left-1/2 top-1/2 w-[min(76vw,1100px)] -translate-x-1/2 -translate-y-1/2 text-center drop-shadow-[0_3px_10px_rgba(0,0,0,0.95)]"
      style:opacity={settings.overlays.lyrics.opacity}
      style:transform={`${layerTransform("lyrics")} translate(-50%, -50%)`}
      style:transition={settings.reducedMotion ? "none" : "transform 20s ease-in-out, opacity 350ms"}
    >
      <p class="whitespace-pre-wrap text-[clamp(1.7rem,4vw,4.5rem)] font-black leading-tight">
        {#each lyricSegments(activeLyric) as segment}
          <span class={segment.startMs == null || segment.startMs - effectiveLyricsOffset <= positionMs ? "text-ink" : "text-ink/45"}>{segment.text}</span>
        {/each}
      </p>
      {#if nextLyric}
        <p class="mt-5 whitespace-pre-wrap text-[clamp(1rem,2vw,2rem)] font-bold text-ink/45">{nextLyric.text}</p>
      {/if}
    </div>
  {/if}

  {#if s.current && layerVisible("progress")}
    <div
      class="pointer-events-none absolute inset-x-[7vw] bottom-[5vh]"
      style:opacity={settings.overlays.progress.opacity}
      style:transform={layerTransform("progress")}
      style:transition={settings.reducedMotion ? "none" : "transform 20s ease-in-out, opacity 500ms"}
    >
      <div class="h-1.5 overflow-hidden rounded-full bg-ink/20 shadow-lg">
        <div class="h-full rounded-full bg-ink" style:width={`${progressPercent}%`}></div>
      </div>
      <div class="mt-2 flex justify-between text-sm font-semibold tabular-nums text-ink/70">
        <span>{formatDuration(positionMs)}</span>
        <span>{formatDuration(s.durationMs)}</span>
      </div>
    </div>
  {/if}

  {#if error}
    <p class="absolute inset-x-0 top-4 mx-auto w-fit max-w-[80vw] rounded-md border border-red-900 bg-red-950/85 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}

  <div
    class="absolute right-4 top-4 flex items-center gap-1 rounded-full bg-black/65 p-1.5 shadow-xl backdrop-blur-sm transition-opacity duration-300 {controlsVisible ? 'opacity-100' : 'pointer-events-none opacity-0'}"
    title={presetName}
  >
    <button class="rounded-full p-2 text-ink/70 hover:bg-ink/10 hover:text-ink" onclick={() => controller?.nextPreset()} aria-label={m.visualizer_next_preset()} title={m.visualizer_next_preset()}>
      <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true"><path d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z"/></svg>
    </button>
    <button class="rounded-full p-2 {settings.presetLocked ? 'bg-ink/15 text-accent' : 'text-ink/70 hover:bg-ink/10 hover:text-ink'}" onclick={toggleLock} aria-label={m.visualizer_projector_preset_lock()} title={m.visualizer_projector_preset_lock()}>
      <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true"><path d="M12 2a5 5 0 0 0-5 5v3H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8a2 2 0 0 0-2-2h-1V7a5 5 0 0 0-5-5zm-3 8V7a3 3 0 1 1 6 0v3H9z"/></svg>
    </button>
    <button class="rounded-full p-2 text-ink/70 hover:bg-ink/10 hover:text-ink" onclick={toggleFullscreen} aria-label={m.visualizer_fullscreen()} title={m.visualizer_fullscreen()}>
      <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true"><path d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
    </button>
    <button class="rounded-full p-2 text-ink/70 hover:bg-ink/10 hover:text-ink" onclick={closeProjector} aria-label={m.dismiss()} title={m.dismiss()}>
      <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true"><path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z"/></svg>
    </button>
  </div>
</div>
