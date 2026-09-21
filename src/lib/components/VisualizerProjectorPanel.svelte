<script lang="ts">
  import { onMount } from "svelte";
  import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { availableMonitors, type Monitor } from "@tauri-apps/api/window";
  import { m } from "$lib/paraglide/messages";
  import {
    PROJECTOR_SETTINGS_EVENT,
    PROJECTOR_WINDOW_LABEL,
    loadProjectorSettings,
    monitorKey,
    normalizeProjectorSettings,
    saveProjectorSettings,
    selectedMonitor,
    type VisualizerOverlayKind,
    type VisualizerProjectorSettings,
  } from "$lib/visualizer/projector";

  let { onclose }: { onclose: () => void } = $props();

  const overlayKinds: VisualizerOverlayKind[] = ["cover", "track", "progress", "lyrics"];
  const timeouts = [0, 5_000, 10_000, 20_000, 60_000];
  let settings = $state<VisualizerProjectorSettings>(loadProjectorSettings());
  let monitors = $state<Monitor[]>([]);
  let projectorOpen = $state(false);
  let opening = $state(false);
  let error = $state<string | null>(null);

  const overlayLabel = (kind: VisualizerOverlayKind) => {
    switch (kind) {
      case "cover": return m.visualizer_overlay_cover();
      case "track": return m.visualizer_overlay_track();
      case "progress": return m.visualizer_overlay_progress();
      case "lyrics": return m.visualizer_overlay_lyrics();
    }
  };

  function persist(next: VisualizerProjectorSettings) {
    settings = normalizeProjectorSettings(next);
    saveProjectorSettings(settings);
    void emit(PROJECTOR_SETTINGS_EVENT, settings);
  }

  function setFlag(
    key: "fullscreen" | "presetLocked" | "burnInMovement" | "reducedMotion" | "songChangeOnly",
    value: boolean,
  ) {
    persist({ ...settings, [key]: value });
    if (key === "fullscreen" && projectorOpen) void applyPlacement();
  }

  function setOverlay(
    kind: VisualizerOverlayKind,
    patch: Partial<VisualizerProjectorSettings["overlays"][VisualizerOverlayKind]>,
  ) {
    persist({
      ...settings,
      overlays: {
        ...settings.overlays,
        [kind]: { ...settings.overlays[kind], ...patch },
      },
    });
  }

  async function applyPlacement() {
    try {
      const target = selectedMonitor(monitors, settings.monitorKey);
      const window = await WebviewWindow.getByLabel(PROJECTOR_WINDOW_LABEL);
      if (!target || !window) return;
      await window.setFullscreen(false);
      await window.setPosition(target.workArea.position);
      await window.setSize(target.workArea.size);
      await window.setFullscreen(settings.fullscreen);
      await window.setFocus();
    } catch (cause) {
      error = String(cause);
    }
  }

  function setMonitor(key: string) {
    persist({ ...settings, monitorKey: key });
    if (projectorOpen) void applyPlacement();
  }

  async function openProjector() {
    error = null;
    try {
      const existing = await WebviewWindow.getByLabel(PROJECTOR_WINDOW_LABEL);
      if (existing) {
        projectorOpen = true;
        await applyPlacement();
        return;
      }
      const target = selectedMonitor(monitors, settings.monitorKey);
      if (!target) {
        error = m.visualizer_monitors_unavailable();
        return;
      }
      opening = true;
      const position = target.workArea.position.toLogical(target.scaleFactor);
      const size = target.workArea.size.toLogical(target.scaleFactor);
      const projector = new WebviewWindow(PROJECTOR_WINDOW_LABEL, {
        url: "/projector",
        title: m.visualizer_projector(),
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        decorations: false,
        resizable: true,
        fullscreen: settings.fullscreen,
        backgroundColor: "#000000",
        focus: true,
      });
      void projector.once("tauri://created", () => {
        opening = false;
        projectorOpen = true;
      });
      void projector.once("tauri://error", (event) => {
        opening = false;
        projectorOpen = false;
        error = String(event.payload);
      });
      void projector.once("tauri://destroyed", () => {
        projectorOpen = false;
      });
    } catch (cause) {
      opening = false;
      error = String(cause);
    }
  }

  async function closeProjector() {
    try {
      const projector = await WebviewWindow.getByLabel(PROJECTOR_WINDOW_LABEL);
      await projector?.close();
      projectorOpen = false;
    } catch (cause) {
      error = String(cause);
    }
  }

  onMount(() => {
    let cancelled = false;
    let unlistenSettings: UnlistenFn | undefined;
    void availableMonitors()
      .then((available) => {
        if (cancelled) return;
        monitors = available;
        if (!settings.monitorKey && available[0]) {
          persist({ ...settings, monitorKey: monitorKey(available[0]) });
        }
      })
      .catch((cause) => (error = String(cause)));
    void WebviewWindow.getByLabel(PROJECTOR_WINDOW_LABEL)
      .then((window) => {
        if (!cancelled) projectorOpen = window !== null;
      });
    void listen<VisualizerProjectorSettings>(PROJECTOR_SETTINGS_EVENT, (event) => {
      if (!cancelled) settings = normalizeProjectorSettings(event.payload);
    }).then((unlisten) => {
      if (cancelled) unlisten();
      else unlistenSettings = unlisten;
    });
    return () => {
      cancelled = true;
      unlistenSettings?.();
    };
  });
</script>

<aside class="absolute inset-y-0 right-0 z-20 flex w-[360px] max-w-[92vw] flex-col bg-black/90 text-ink shadow-2xl backdrop-blur-md">
  <div class="flex items-center justify-between border-b border-edge px-4 py-3">
    <h2 class="text-sm font-bold">{m.visualizer_projector_settings()}</h2>
    <button
      class="flex h-7 w-7 items-center justify-center rounded-full text-ink/60 hover:bg-ink/10 hover:text-ink"
      onclick={onclose}
      aria-label={m.dismiss()}
      title={m.dismiss()}
    >
      <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true"><path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z"/></svg>
    </button>
  </div>

  <div class="min-h-0 flex-1 space-y-5 overflow-y-auto p-4">
    <section class="space-y-3">
      <label class="block text-xs text-ink/70">
        <span class="mb-1 block font-semibold text-ink">{m.visualizer_projector_monitor()}</span>
        <select
          class="w-full rounded-md bg-ink/10 px-3 py-2 text-sm outline-none"
          value={settings.monitorKey ?? ""}
          onchange={(event) => setMonitor(event.currentTarget.value)}
          disabled={monitors.length === 0}
        >
          {#each monitors as monitor, index (monitorKey(monitor))}
            <option value={monitorKey(monitor)}>
              {monitor.name ?? m.visualizer_monitor_number({ number: index + 1 })}
              · {monitor.size.width}×{monitor.size.height}
            </option>
          {/each}
        </select>
      </label>
      {#if monitors.length === 0}
        <p class="text-xs text-ink/50">{m.visualizer_monitors_unavailable()}</p>
      {/if}
      <label class="flex items-center gap-2 text-xs text-ink/80">
        <input type="checkbox" checked={settings.fullscreen} onchange={(e) => setFlag("fullscreen", e.currentTarget.checked)} class="accent-accent" />
        {m.visualizer_fullscreen()}
      </label>
      <label class="flex items-center gap-2 text-xs text-ink/80">
        <input type="checkbox" checked={settings.presetLocked} onchange={(e) => setFlag("presetLocked", e.currentTarget.checked)} class="accent-accent" />
        {m.visualizer_projector_preset_lock()}
      </label>
      <div class="flex gap-2">
        {#if projectorOpen}
          <button class="flex-1 rounded-md bg-ink/10 px-3 py-2 text-xs font-semibold hover:bg-ink/15" onclick={closeProjector}>
            {m.visualizer_projector_close()}
          </button>
        {:else}
          <button class="flex-1 rounded-md bg-accent px-3 py-2 text-xs font-semibold text-(--color-on-accent) disabled:opacity-50" onclick={openProjector} disabled={opening || monitors.length === 0}>
            {m.visualizer_projector_open()}
          </button>
        {/if}
      </div>
    </section>

    <section class="space-y-3 border-t border-edge pt-4">
      <h3 class="text-xs font-bold uppercase tracking-wider text-ink/60">{m.visualizer_overlays()}</h3>
      {#each overlayKinds as kind (kind)}
        {@const overlay = settings.overlays[kind]}
        <div class="space-y-2 rounded-lg bg-ink/5 p-3">
          <label class="flex items-center gap-2 text-xs font-semibold">
            <input type="checkbox" checked={overlay.enabled} onchange={(e) => setOverlay(kind, { enabled: e.currentTarget.checked })} class="accent-accent" />
            {overlayLabel(kind)}
          </label>
          <label class="block text-[11px] text-ink/60">
            <span class="mb-1 flex justify-between"><span>{m.visualizer_overlay_opacity()}</span><span>{Math.round(overlay.opacity * 100)}%</span></span>
            <input class="w-full accent-accent" type="range" min="0.1" max="1" step="0.05" value={overlay.opacity} oninput={(e) => setOverlay(kind, { opacity: Number(e.currentTarget.value) })} />
          </label>
          <label class="flex items-center justify-between gap-3 text-[11px] text-ink/60">
            <span>{m.visualizer_overlay_timeout()}</span>
            <select class="rounded bg-ink/10 px-2 py-1 text-ink" value={overlay.timeoutMs} onchange={(e) => setOverlay(kind, { timeoutMs: Number(e.currentTarget.value) })}>
              {#each timeouts as timeout (timeout)}
                <option value={timeout}>{timeout === 0 ? m.visualizer_overlay_always() : m.visualizer_overlay_seconds({ seconds: timeout / 1000 })}</option>
              {/each}
            </select>
          </label>
        </div>
      {/each}
    </section>

    <section class="space-y-3 border-t border-edge pt-4">
      <label class="flex items-center gap-2 text-xs text-ink/80">
        <input type="checkbox" checked={settings.burnInMovement} onchange={(e) => setFlag("burnInMovement", e.currentTarget.checked)} class="accent-accent" />
        {m.visualizer_burn_in_movement()}
      </label>
      <label class="flex items-center gap-2 text-xs text-ink/80">
        <input type="checkbox" checked={settings.reducedMotion} onchange={(e) => setFlag("reducedMotion", e.currentTarget.checked)} class="accent-accent" />
        {m.visualizer_reduced_motion()}
      </label>
      <label class="flex items-center gap-2 text-xs text-ink/80">
        <input type="checkbox" checked={settings.songChangeOnly} onchange={(e) => setFlag("songChangeOnly", e.currentTarget.checked)} class="accent-accent" />
        {m.visualizer_song_change_only()}
      </label>
    </section>

    {#if error}
      <p class="rounded-md border border-red-900 bg-red-950/70 px-3 py-2 text-xs text-red-300">{m.error_generic({ message: error })}</p>
    {/if}
  </div>
</aside>
