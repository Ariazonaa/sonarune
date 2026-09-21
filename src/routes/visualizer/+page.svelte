<script lang="ts">
  import { onMount } from "svelte";
  import { VList } from "virtua/svelte";
  import { m } from "$lib/paraglide/messages";
  import { player } from "$lib/state/player.svelte";
  import VisualizerProjectorPanel from "$lib/components/VisualizerProjectorPanel.svelte";
  import {
    VisualizerController,
    loadCycleSetting,
    loadBlendSetting,
    loadSensitivitySetting,
    loadQualitySetting,
    loadRemotePresetsSetting,
    saveRemotePresetsSetting,
    type CycleSetting,
    type VisualizerQuality,
  } from "$lib/visualizer/controller";
  import type { EnergyLevel } from "$lib/visualizer/engine";
  import type {
    PresetCatalogProgress,
    PresetComplexity,
    PresetEntry,
  } from "$lib/visualizer/presets";
  import { PRESET_PACK_COUNT } from "$lib/visualizer/presets";

  const CYCLE_OPTIONS: CycleSetting[] = ["auto", 10, 15, 30, 60, 0];
  /** Preset transition times in seconds (0 = hard cut). */
  const BLEND_OPTIONS = [0, 1, 2.7, 5];
  const QUALITY_OPTIONS: VisualizerQuality[] = ["low", "medium", "high"];
  /** Overlay hides after this much mouse idle time. */
  const OVERLAY_IDLE_MS = 2_500;

  let container = $state<HTMLElement | null>(null);
  let canvas = $state<HTMLCanvasElement | null>(null);
  let controller: VisualizerController | null = null;

  let presetName = $state("");
  let locked = $state(false);
  let cycle = $state<CycleSetting>(loadCycleSetting());
  let blend = $state(loadBlendSetting());
  let sensitivity = $state(loadSensitivitySetting());
  let quality = $state<VisualizerQuality>(loadQualitySetting());
  let overlayVisible = $state(true);
  let error = $state<string | null>(null);
  /** Briefly surfaces the preset name whenever it changes. */
  let nameVisible = $state(false);

  // Preset browser (B2) — reactive mirrors of the controller's persisted sets.
  let browserOpen = $state(false);
  let presetSearch = $state("");
  let authorSearch = $state("");
  let packFilter = $state("all");
  let intensityFilter = $state<"all" | "calm" | "medium" | "intense">("all");
  let complexityFilter = $state<"all" | PresetComplexity>("all");
  let presetList = $state<PresetEntry[]>([]);
  let catalogProgress = $state<PresetCatalogProgress>({
    loadedPacks: 0,
    totalPacks: PRESET_PACK_COUNT,
    failedPacks: [],
  });
  let favorites = $state<Set<string>>(new Set());
  let blocked = $state<Set<string>>(new Set());
  let quarantined = $state<Set<string>>(new Set());
  let favoritesOnly = $state(false);
  /** Opt-in for the online Weekly presets; while off they are not even listed. */
  let remotePresets = $state(loadRemotePresetsSetting());
  let backupInput = $state<HTMLInputElement | null>(null);
  let backupStatus = $state("");
  let projectorPanelOpen = $state(false);

  const availablePresets = $derived(
    remotePresets ? presetList : presetList.filter((entry) => !entry.sourceUrl),
  );
  const packOptions = $derived([...new Set(availablePresets.map((entry) => entry.pack))].sort());

  const filteredPresets = $derived.by(() => {
    const q = presetSearch.trim().toLowerCase();
    const author = authorSearch.trim().toLowerCase();
    return availablePresets.filter((entry) => {
      if (q && !entry.name.toLowerCase().includes(q)) return false;
      if (author && !entry.author.toLowerCase().includes(author)) return false;
      if (packFilter !== "all" && entry.pack !== packFilter) return false;
      if (intensityFilter !== "all" && entry.intensity !== intensityFilter) return false;
      if (complexityFilter !== "all" && entry.complexity !== complexityFilter) return false;
      if (favoritesOnly && !favorites.has(entry.name)) return false;
      return true;
    });
  });

  const qualityLabel = (q: VisualizerQuality) =>
    q === "low" ? m.quality_low() : q === "high" ? m.quality_high() : m.quality_medium();

  let overlayTimer: ReturnType<typeof setTimeout> | undefined;
  let nameTimer: ReturnType<typeof setTimeout> | undefined;

  const currentTrackId = $derived(player.state.current?.itemId);

  function flashName(name: string) {
    presetName = name;
    nameVisible = true;
    clearTimeout(nameTimer);
    nameTimer = setTimeout(() => (nameVisible = false), 4_000);
  }

  function setCycle(value: CycleSetting) {
    cycle = value;
    controller?.setCycle(value);
  }

  function setBlend(value: number) {
    blend = value;
    controller?.setBlend(value);
  }

  function setSensitivity(value: number) {
    sensitivity = value;
    controller?.setSensitivity(value);
  }

  function setQuality(value: VisualizerQuality) {
    quality = value;
    controller?.setQuality(value);
  }

  function setFavoritesOnly(on: boolean) {
    favoritesOnly = on;
    controller?.setFavoritesOnly(on);
  }

  function setRemotePresets(on: boolean) {
    // Without a controller (creation failed) the choice is still stored.
    remotePresets = controller
      ? controller.setRemotePresetsEnabled(on)
      : saveRemotePresetsSetting(on);
    if (packFilter !== "all" && !packOptions.includes(packFilter)) packFilter = "all";
  }

  function toggleFavorite(name: string) {
    controller?.toggleFavorite(name);
    favorites = new Set(controller?.favoriteNames() ?? []);
  }

  function toggleBlocked(name: string) {
    controller?.toggleBlocked(name);
    blocked = new Set(controller?.blockedNames() ?? []);
  }

  function selectPreset(name: string) {
    controller?.selectPreset(name);
    quarantined = new Set(controller?.quarantinedNames() ?? []);
  }

  function exportPreferences() {
    if (!controller) return;
    const blob = new Blob([controller.exportPreferences()], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "jellysic-visualizer-preferences.json";
    a.click();
    URL.revokeObjectURL(url);
    backupStatus = m.visualizer_backup_exported();
  }

  async function importPreferences(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file || !controller) return;
    try {
      controller.importPreferences(await file.text());
      favorites = new Set(controller.favoriteNames());
      blocked = new Set(controller.blockedNames());
      backupStatus = m.visualizer_backup_imported();
    } catch (cause) {
      error = m.visualizer_backup_error({
        message: cause instanceof Error ? cause.message : String(cause),
      });
    }
  }

  function clearQuarantine() {
    controller?.clearQuarantine();
    quarantined = new Set();
  }

  function screenshot() {
    const url = controller?.capture();
    if (!url) return;
    const a = document.createElement("a");
    a.href = url;
    a.download = "jellysic-visualizer.png";
    a.click();
  }

  function toggleLock() {
    locked = !locked;
    controller?.setLocked(locked);
  }

  function pokeOverlay() {
    overlayVisible = true;
    clearTimeout(overlayTimer);
    overlayTimer = setTimeout(() => (overlayVisible = false), OVERLAY_IDLE_MS);
  }

  function toggleFullscreen() {
    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else {
      container?.requestFullscreen();
    }
  }

  // New track (unless locked) -> new preset, like Feishin.
  let lastTrackId: string | undefined;
  $effect(() => {
    const id = currentTrackId;
    if (id && lastTrackId && id !== lastTrackId) {
      controller?.onTrackChange(player.state.current?.name);
    }
    lastTrackId = id;
  });

  onMount(() => {
    let observer: ResizeObserver | undefined;
    let cancelled = false;

    (async () => {
      try {
        if (!canvas) return;
        controller = await VisualizerController.create(canvas, {
          onError: (message) => {
            error = message;
            quarantined = new Set(controller?.quarantinedNames() ?? []);
          },
          onPreset: flashName,
          onCatalog: (entries, progress) => {
            presetList = entries;
            catalogProgress = progress;
          },
          isCancelled: () => cancelled,
        });
        // The factory checks `isCancelled` too, but teardown can land in the
        // window between its last check and this assignment — the controller
        // owns a WebGL context and an AudioWorklet, so a leaked one keeps
        // rendering after the route is gone.
        if (!controller) return; // creation was cancelled
        if (cancelled) {
          controller.destroy();
          controller = null;
          return;
        }
        controller.setLocked(locked);
        // Mirror the controller's persisted state into reactive UI state.
        presetList = controller.presetEntries();
        favorites = new Set(controller.favoriteNames());
        blocked = new Set(controller.blockedNames());
        quarantined = new Set(controller.quarantinedNames());
        favoritesOnly = controller.favoritesOnly;
        remotePresets = controller.remotePresetsEnabled;
        sensitivity = controller.sensitivity;
        quality = controller.quality;
        observer = new ResizeObserver(() => {
          if (canvas && controller) controller.setSize(canvas.clientWidth, canvas.clientHeight);
        });
        if (container) observer.observe(container);
      } catch (e) {
        error = String(e);
      }
    })();

    pokeOverlay();

    return () => {
      cancelled = true;
      clearTimeout(overlayTimer);
      clearTimeout(nameTimer);
      observer?.disconnect();
      controller?.destroy();
      controller = null;
    };
  });

  // Preset metadata comes from the packs as raw enum values; render them
  // through the message catalog rather than shipping English into the UI.
  function intensityLabel(level: EnergyLevel): string {
    return level === "calm"
      ? m.visualizer_intensity_calm()
      : level === "intense"
        ? m.visualizer_intensity_intense()
        : m.visualizer_intensity_medium();
  }

  function complexityLabel(level: PresetComplexity): string {
    return level === "light"
      ? m.visualizer_complexity_light()
      : level === "heavy"
        ? m.visualizer_complexity_heavy()
        : m.visualizer_complexity_medium();
  }

</script>

<div
  bind:this={container}
  class="relative h-full min-h-0 bg-black"
  onmousemove={pokeOverlay}
  role="presentation"
  data-testid="visualizer"
>
  <canvas bind:this={canvas} class="h-full w-full"></canvas>

  {#if error}
    <p class="absolute inset-x-0 top-4 mx-auto w-fit rounded-md border border-red-900 bg-red-950/80 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}

  <!-- Preset name flash: visible for a few seconds on every switch, even
       when the control overlay is hidden. -->
  <p
    class="pointer-events-none absolute left-5 top-4 max-w-[70%] truncate rounded-full bg-black/60 px-3 py-1 text-xs text-ink/90 transition-opacity duration-500
      {nameVisible ? 'opacity-100' : 'opacity-0'}"
    title={presetName}
  >
    {presetName}
  </p>

  <div
    class="absolute inset-x-0 bottom-0 flex items-center justify-between gap-4 bg-gradient-to-t from-black/80 to-transparent px-5 pb-4 pt-10 transition-opacity duration-300
      {overlayVisible ? 'opacity-100' : 'pointer-events-none opacity-0'}"
  >
    <div class="flex min-w-0 items-center gap-3">
      <p class="truncate text-xs text-ink/70" title={presetName}>{presetName}</p>
      <div class="flex shrink-0 items-center gap-1" role="group" aria-label={m.visualizer_cycle()}>
        <span class="text-[10px] uppercase tracking-wider text-ink/50">{m.visualizer_cycle()}</span>
        {#each CYCLE_OPTIONS as option (option)}
          <button
            class="rounded-full px-2 py-0.5 text-[11px] font-semibold transition-colors
              {cycle === option ? 'bg-ink/20 text-ink' : 'text-ink/60 hover:text-ink'}"
            onclick={() => setCycle(option)}
          >
            {option === "auto"
              ? m.visualizer_cycle_auto()
              : option === 0
                ? m.visualizer_cycle_off()
                : `${option}s`}
          </button>
        {/each}
      </div>
      <div class="flex shrink-0 items-center gap-1" role="group" aria-label={m.visualizer_blend()}>
        <span class="text-[10px] uppercase tracking-wider text-ink/50">{m.visualizer_blend()}</span>
        {#each BLEND_OPTIONS as option (option)}
          <button
            class="rounded-full px-2 py-0.5 text-[11px] font-semibold transition-colors
              {blend === option ? 'bg-ink/20 text-ink' : 'text-ink/60 hover:text-ink'}"
            onclick={() => setBlend(option)}
          >
            {option === 0 ? m.visualizer_blend_cut() : `${option}s`}
          </button>
        {/each}
      </div>
    </div>
    <div class="flex shrink-0 items-center gap-1">
      <button
        class="rounded-full p-2 transition-colors {projectorPanelOpen ? 'text-accent' : 'text-ink/70 hover:text-ink'}"
        onclick={() => {
          projectorPanelOpen = !projectorPanelOpen;
          if (projectorPanelOpen) browserOpen = false;
        }}
        aria-label={m.visualizer_projector()}
        title={m.visualizer_projector()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor" aria-hidden="true"><path d="M3 4h18a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-7v2h3v2H7v-2h3v-2H3a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zm0 2v11h18V6H3z"/></svg>
      </button>
      <button
        class="rounded-full p-2 transition-colors {browserOpen ? 'text-accent' : 'text-ink/70 hover:text-ink'}"
        onclick={() => {
          browserOpen = !browserOpen;
          if (browserOpen) projectorPanelOpen = false;
        }}
        aria-label={m.visualizer_presets()}
        title={m.visualizer_presets()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M4 6h16v2H4V6zm0 5h16v2H4v-2zm0 5h10v2H4v-2z"/></svg>
      </button>
      <button
        class="rounded-full p-2 text-ink/70 transition-colors hover:text-ink"
        onclick={screenshot}
        aria-label={m.visualizer_screenshot()}
        title={m.visualizer_screenshot()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M9 3l-1.83 2H4a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-3.17L15 3H9zm3 5a5 5 0 1 1 0 10 5 5 0 0 1 0-10zm0 2a3 3 0 1 0 0 6 3 3 0 0 0 0-6z"/></svg>
      </button>
      <button
        class="rounded-full p-2 text-ink/70 transition-colors hover:text-ink"
        onclick={() => controller?.nextPreset()}
        aria-label={m.visualizer_next_preset()}
        title={m.visualizer_next_preset()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z"/></svg>
      </button>
      <button
        class="rounded-full p-2 transition-colors {locked ? 'text-accent' : 'text-ink/70 hover:text-ink'}"
        onclick={toggleLock}
        aria-label={m.visualizer_lock()}
        title={m.visualizer_lock()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M12 2a5 5 0 0 0-5 5v3H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8a2 2 0 0 0-2-2h-1V7a5 5 0 0 0-5-5zm-3 8V7a3 3 0 1 1 6 0v3H9z"/></svg>
      </button>
      <button
        class="rounded-full p-2 text-ink/70 transition-colors hover:text-ink"
        onclick={toggleFullscreen}
        aria-label={m.visualizer_fullscreen()}
        title={m.visualizer_fullscreen()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/></svg>
      </button>
    </div>
  </div>

  {#if projectorPanelOpen}
    <VisualizerProjectorPanel onclose={() => (projectorPanelOpen = false)} />
  {/if}

  {#if browserOpen}
    <aside class="absolute inset-y-0 right-0 z-10 flex w-80 max-w-[85vw] flex-col bg-black/85 backdrop-blur-sm">
      <div class="flex items-center justify-between px-4 py-3">
        <h2 class="text-sm font-bold text-ink">{m.visualizer_presets()}</h2>
        <button
          class="flex h-7 w-7 items-center justify-center rounded-full text-ink/60 transition-colors hover:bg-ink/10 hover:text-ink"
          onclick={() => (browserOpen = false)}
          aria-label={m.dismiss()}
          title={m.dismiss()}
        >
          <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true"><path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z"/></svg>
        </button>
      </div>

      <div class="space-y-3 border-b border-edge px-4 pb-3">
        <div>
          <p class="mb-1 text-[10px] uppercase tracking-wider text-ink/50">{m.visualizer_quality()}</p>
          <div class="flex gap-1">
            {#each QUALITY_OPTIONS as q (q)}
              <button
                class="rounded-full px-2.5 py-0.5 text-[11px] font-semibold transition-colors
                  {quality === q ? 'bg-ink/20 text-ink' : 'text-ink/60 hover:text-ink'}"
                onclick={() => setQuality(q)}
              >
                {qualityLabel(q)}
              </button>
            {/each}
          </div>
        </div>
        <div>
          <p class="mb-1 text-[10px] uppercase tracking-wider text-ink/50">
            {m.visualizer_sensitivity()} · {sensitivity.toFixed(1)}×
          </p>
          <input
            type="range"
            min="0.5"
            max="3"
            step="0.1"
            value={sensitivity}
            oninput={(e) => setSensitivity(Number((e.currentTarget as HTMLInputElement).value))}
            class="w-full accent-accent"
          />
        </div>
        <label class="flex items-center gap-2 text-xs text-ink/80">
          <input
            type="checkbox"
            checked={favoritesOnly}
            onchange={(e) => setFavoritesOnly((e.currentTarget as HTMLInputElement).checked)}
            class="accent-accent"
          />
          {m.visualizer_favorites_only()}
        </label>
        <div>
          <label class="flex items-center gap-2 text-xs text-ink/80">
            <input
              type="checkbox"
              checked={remotePresets}
              onchange={(e) => setRemotePresets((e.currentTarget as HTMLInputElement).checked)}
              class="accent-accent"
              aria-describedby="visualizer-remote-presets-warning"
            />
            {m.visualizer_remote_presets()}
          </label>
          <p id="visualizer-remote-presets-warning" class="mt-1 pl-5 text-[10px] leading-snug text-ink/60">
            {m.visualizer_remote_presets_warning()}
          </p>
        </div>
        <div class="flex items-center justify-between text-[10px] text-ink/50">
          <span>{m.visualizer_catalog_count({ filtered: filteredPresets.length, total: availablePresets.length })}</span>
          <span>
            {catalogProgress.loadedPacks}/{catalogProgress.totalPacks} {m.visualizer_packs()}
          </span>
        </div>
        <input
          type="search"
          placeholder={m.visualizer_search_presets()}
          bind:value={presetSearch}
          class="w-full rounded-full bg-ink/10 px-3 py-1.5 text-xs text-ink outline-none placeholder:text-ink/40 focus:bg-ink/15"
        />
        <input
          type="search"
          placeholder={m.visualizer_search_author()}
          bind:value={authorSearch}
          class="w-full rounded-full bg-ink/10 px-3 py-1.5 text-xs text-ink outline-none placeholder:text-ink/40 focus:bg-ink/15"
        />
        <div class="grid grid-cols-2 gap-2">
          <select
            bind:value={packFilter}
            aria-label={m.visualizer_pack()}
            class="min-w-0 rounded-md bg-ink/10 px-2 py-1.5 text-[11px] text-ink outline-none"
          >
            <option value="all">{m.visualizer_all_packs()}</option>
            {#each packOptions as pack (pack)}
              <option value={pack}>{pack}</option>
            {/each}
          </select>
          <select
            bind:value={intensityFilter}
            aria-label={m.visualizer_energy()}
            class="min-w-0 rounded-md bg-ink/10 px-2 py-1.5 text-[11px] text-ink outline-none"
          >
            <option value="all">{m.visualizer_all_energy()}</option>
            <option value="calm">{m.visualizer_energy_calm()}</option>
            <option value="medium">{m.visualizer_energy_medium()}</option>
            <option value="intense">{m.visualizer_energy_intense()}</option>
          </select>
          <select
            bind:value={complexityFilter}
            aria-label={m.visualizer_complexity()}
            class="min-w-0 rounded-md bg-ink/10 px-2 py-1.5 text-[11px] text-ink outline-none"
          >
            <option value="all">{m.visualizer_all_complexity()}</option>
            <option value="light">{m.visualizer_complexity_light()}</option>
            <option value="medium">{m.visualizer_complexity_medium()}</option>
            <option value="heavy">{m.visualizer_complexity_heavy()}</option>
          </select>
          <div class="flex gap-1">
            <button
              class="flex-1 rounded-md bg-ink/10 px-2 py-1 text-[10px] text-ink/70 hover:bg-ink/15 hover:text-ink"
              onclick={exportPreferences}
              title={m.visualizer_backup_export()}
            >{m.visualizer_backup_export()}</button>
            <button
              class="flex-1 rounded-md bg-ink/10 px-2 py-1 text-[10px] text-ink/70 hover:bg-ink/15 hover:text-ink"
              onclick={() => backupInput?.click()}
              title={m.visualizer_backup_import()}
            >{m.visualizer_backup_import()}</button>
          </div>
        </div>
        <input
          bind:this={backupInput}
          class="hidden"
          type="file"
          accept="application/json,.json"
          onchange={importPreferences}
        />
        {#if quarantined.size > 0}
          <button
            class="text-left text-[10px] text-amber-300/80 hover:text-amber-200"
            onclick={clearQuarantine}
          >
            {(quarantined.size === 1
              ? m.visualizer_quarantine_clear_one
              : m.visualizer_quarantine_clear_many)({ count: quarantined.size })}
          </button>
        {/if}
        {#if catalogProgress.failedPacks.length > 0}
          <p class="text-[10px] text-amber-300/80">
            {m.visualizer_pack_failures({ packs: catalogProgress.failedPacks.join(", ") })}
          </p>
        {/if}
        {#if backupStatus}<p class="text-[10px] text-emerald-300/80">{backupStatus}</p>{/if}
      </div>

      <div class="min-h-0 flex-1 p-2">
        <VList
          data={filteredPresets}
          style="height: 100%;"
          getKey={(entry) => entry.name}
        >
          {#snippet children(entry)}
            <div
              class="group flex h-[50px] items-center gap-1 rounded-md pr-1
                {entry.name === presetName ? 'bg-ink/15' : 'hover:bg-ink/10'}
                {blocked.has(entry.name) ? 'opacity-40' : ''}"
            >
              <button
                class="min-w-0 flex-1 py-1 pl-2 text-left"
                onclick={() => selectPreset(entry.name)}
                title={entry.name}
              >
                <span class="block truncate text-xs text-ink">{entry.name}</span>
                <span class="block truncate text-[9px] text-ink/45">
                  {entry.pack} · {entry.author} · {intensityLabel(entry.intensity)} ·
                  {complexityLabel(entry.complexity)}
                </span>
              </button>
              <button
                class="rounded p-1 {favorites.has(entry.name) ? 'text-accent' : 'text-ink/40 hover:text-ink'}"
                onclick={() => toggleFavorite(entry.name)}
                aria-label={m.favorite_toggle()}
                title={m.favorite_toggle()}
              >
                <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true"><path d="M12 17.27 18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z"/></svg>
              </button>
              <button
                class="rounded p-1 {blocked.has(entry.name) ? 'text-red-400' : 'text-ink/40 hover:text-ink'}"
                onclick={() => toggleBlocked(entry.name)}
                aria-label={m.visualizer_hide()}
                title={m.visualizer_hide()}
              >
                <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true"><path d="M12 6.5c3.79 0 7.17 2.13 8.82 5.5-.6 1.24-1.47 2.31-2.52 3.15l1.43 1.43C21.36 15.14 22.5 13.66 23 12c-1.73-4.39-6-7.5-11-7.5-1.4 0-2.74.25-3.98.7l1.63 1.62c.76-.2 1.54-.32 2.35-.32zM2.71 3.16 1.3 4.57l2.72 2.72C2.63 8.4 1.5 10.06 1 12c1.73 4.39 6 7.5 11 7.5 1.94 0 3.78-.47 5.4-1.3l3.03 3.03 1.41-1.41L2.71 3.16z"/></svg>
              </button>
            </div>
          {/snippet}
        </VList>
      </div>
    </aside>
  {/if}
</div>
