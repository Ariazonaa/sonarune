<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import { getLocale, setLocale } from "$lib/paraglide/runtime";
  import { player } from "$lib/state/player.svelte";
  import { session } from "$lib/state/session.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { confirm } from "$lib/state/confirm.svelte";
  import { syncTrayLabels } from "$lib/tray";
  import {
    deleteCustomPreset,
    loadCustomPresets,
    MAX_CUSTOM_PRESETS,
    MAX_PRESET_NAME,
    normalizeName,
    persistCustomPresets,
    sameGains,
    saveCustomPreset,
    type EqPreset,
  } from "$lib/eqPresets";
  import ShortcutSettings from "$lib/components/ShortcutSettings.svelte";
  import LayoutSettings from "$lib/components/LayoutSettings.svelte";
  import TrustedCertSettings from "$lib/components/TrustedCertSettings.svelte";
  import UpdateSettings from "$lib/components/UpdateSettings.svelte";
  import {
    ACCENTS,
    applyTheme,
    lightenHex,
    loadTheme,
    type AccentOption,
    type ThemeSettings,
  } from "$lib/theme";
  import type {
    AudioDeviceSnapshot,
    CacheInfo,
    DesktopSettings,
    DspParams,
    ExtrasSettings,
    PlaybackSettings,
  } from "$lib/types";

  // Language names are shown in their own language on purpose.
  const LOCALES = [
    { code: "en", label: "English" },
    { code: "de", label: "Deutsch" },
  ] as const;

  const BAND_LABELS = ["31", "62", "125", "250", "500", "1K", "2K", "4K", "8K", "16K"];
  const CACHE_LIMITS_MB = [64, 128, 256, 512, 1024, 2048];

  // The id is stable and never shown; the label is translated.
  const PRESETS: { id: string; label: () => string; gains: number[] }[] = [
    { id: "flat", label: m.eq_preset_flat, gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
    { id: "bass_boost", label: m.eq_preset_bass_boost, gains: [6, 5, 4, 2.5, 1, 0, 0, 0, 0, 0] },
    {
      id: "bass_reducer",
      label: m.eq_preset_bass_reducer,
      gains: [-6, -5, -4, -2.5, -1, 0, 0, 0, 0, 0],
    },
    {
      id: "treble_boost",
      label: m.eq_preset_treble_boost,
      gains: [0, 0, 0, 0, 0, 1, 2.5, 4, 5, 6],
    },
    { id: "vocal", label: m.eq_preset_vocal, gains: [-2, -3, -3, 1.5, 4, 4, 3, 1.5, 0, -1.5] },
    { id: "rock", label: m.eq_preset_rock, gains: [5, 4, 3, 1.5, -0.5, -1, 0.5, 2.5, 3.5, 4.5] },
    {
      id: "electronic",
      label: m.eq_preset_electronic,
      gains: [4.5, 4, 1, 0, -2, 2, 1, 1.5, 4, 4.5],
    },
    { id: "jazz", label: m.eq_preset_jazz, gains: [3, 2, 1, 1.5, -1.5, -1.5, 0, 1.5, 2.5, 3] },
    {
      id: "classical",
      label: m.eq_preset_classical,
      gains: [4, 3, 2.5, 1, -0.5, -0.5, 0, 1.5, 2.5, 3.5],
    },
    { id: "pop", label: m.eq_preset_pop, gains: [-1.5, -0.5, 0, 1.5, 3.5, 3.5, 2, 0, -0.5, -1.5] },
  ];

  const ACCENT_LABELS: Record<string, () => string> = {
    green: m.accent_green,
    blue: m.accent_blue,
    purple: m.accent_purple,
    pink: m.accent_pink,
    red: m.accent_red,
    orange: m.accent_orange,
    teal: m.accent_teal,
    white: m.accent_white,
  };

  let settings = $state<DspParams | null>(null);
  let playback = $state<PlaybackSettings | null>(null);
  let audioDevices = $state<AudioDeviceSnapshot>({ devices: [], fallbackDevice: null });
  let theme = $state<ThemeSettings>(loadTheme());
  let extras = $state<ExtrasSettings | null>(null);
  let desktop = $state<DesktopSettings | null>(null);
  let cacheInfo = $state<CacheInfo | null>(null);
  let importedCount = $state<number | null>(null);
  let backupError = $state<string | null>(null);
  let importInput = $state<HTMLInputElement | null>(null);
  let maintenanceBusy = $state(false);
  let maintenanceMessage = $state<string | null>(null);
  let maintenanceError = $state<string | null>(null);
  // Full path of the last diagnostics export, so the user can see/open it.
  let diagnosticsPath = $state<string | null>(null);

  const selectedOutputMissing = $derived(
    playback?.outputDevice != null && !audioDevices.devices.includes(playback.outputDevice),
  );
  const outputFallbackActive = $derived(
    playback?.outputDevice != null && audioDevices.fallbackDevice === playback.outputDevice,
  );

  onMount(() => {
    let disposed = false;
    let refreshingDevices = false;

    async function refreshAudioDevices(reportError = false) {
      if (refreshingDevices) return;
      refreshingDevices = true;
      try {
        const snapshot = await api.listAudioDevices();
        if (!disposed) audioDevices = snapshot;
      } catch (e) {
        if (reportError && !disposed) player.error = String(e);
      } finally {
        refreshingDevices = false;
      }
    }

    async function initialize() {
      try {
        const [nextSettings, nextPlayback, nextExtras, nextDesktop, nextCache] =
          await Promise.all([
            api.getAudioSettings(),
            api.getPlaybackSettings(),
            api.getExtrasSettings(),
            api.getDesktopSettings(),
            api.getCacheInfo(),
          ]);
        if (disposed) return;
        settings = nextSettings;
        playback = nextPlayback;
        extras = nextExtras;
        rememberAutoDj(nextExtras);
        desktop = nextDesktop;
        cacheInfo = nextCache;
        await refreshAudioDevices(true);
      } catch (e) {
        if (!disposed) player.error = String(e);
      }
    }

    void initialize();
    const pollTimer = setInterval(() => void refreshAudioDevices(), 2500);
    return () => {
      disposed = true;
      clearInterval(pollTimer);
    };
  });

  function save() {
    if (settings) player.run(api.setAudioSettings($state.snapshot(settings)));
  }

  function savePlayback() {
    if (playback) player.run(api.setPlaybackSettings($state.snapshot(playback)));
  }

  function changeLocale(locale: "en" | "de") {
    setLocale(locale);
    void syncTrayLabels();
  }

  async function saveDesktop() {
    if (!desktop) return;
    try {
      desktop = await api.setDesktopSettings($state.snapshot(desktop));
      cacheInfo = await api.getCacheInfo();
    } catch (e) {
      player.error = String(e);
    }
  }

  function formatBytes(bytes: number): string {
    const units = ["B", "KB", "MB", "GB"];
    let value = Math.max(0, bytes);
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    const formatted = new Intl.NumberFormat(getLocale(), {
      maximumFractionDigits: unit === 0 ? 0 : 1,
    }).format(value);
    return `${formatted} ${units[unit]}`;
  }

  async function clearCache() {
    maintenanceBusy = true;
    maintenanceMessage = null;
    maintenanceError = null;
    try {
      cacheInfo = await api.clearCoverCache();
      maintenanceMessage = m.settings_cache_cleared();
    } catch (e) {
      maintenanceError = String(e);
    } finally {
      maintenanceBusy = false;
    }
  }

  async function exportDiagnostics() {
    maintenanceBusy = true;
    maintenanceMessage = null;
    maintenanceError = null;
    try {
      const path = await api.exportDiagnostics();
      diagnosticsPath = path;
      maintenanceMessage = m.settings_diagnostics_exported({
        name: path.split(/[\\/]/).pop() ?? path,
      });
    } catch (e) {
      maintenanceError = String(e);
    } finally {
      maintenanceBusy = false;
    }
  }

  function revealDiagnostics() {
    if (diagnosticsPath) {
      void api.revealPath(diagnosticsPath).catch((e) => (maintenanceError = String(e)));
    }
  }

  function applyPreset(gains: number[]) {
    if (!settings) return;
    settings.eqGainsDb = [...gains];
    save();
  }

  // User presets (localStorage, like theme and smart views).
  let customPresets = $state<EqPreset[]>(loadCustomPresets());
  // null: the save form is closed.
  let presetName = $state<string | null>(null);

  // The preset the sliders currently match — built-ins win a tie.
  const activePreset = $derived.by(() => {
    const gains = settings?.eqGainsDb;
    if (!gains) return null;
    const builtIn = PRESETS.find((p) => sameGains(p.gains, gains));
    if (builtIn) return `builtin:${builtIn.id}`;
    const custom = customPresets.find((p) => sameGains(p.gains, gains));
    return custom ? `custom:${custom.name}` : null;
  });
  const presetNameClean = $derived(normalizeName(presetName ?? ""));
  const presetOverwrites = $derived(
    customPresets.find(
      (p) => p.name.toLocaleLowerCase() === presetNameClean.toLocaleLowerCase(),
    ) ?? null,
  );
  const presetsFull = $derived(
    customPresets.length >= MAX_CUSTOM_PRESETS && presetOverwrites === null,
  );

  function commitPreset() {
    if (!settings || presetName === null) return;
    const result = saveCustomPreset(customPresets, presetName, $state.snapshot(settings.eqGainsDb));
    if (!result.ok) return;
    const name = presetNameClean;
    customPresets = result.presets;
    persistCustomPresets(customPresets);
    presetName = null;
    toast.show(m.settings_eq_preset_saved({ name }));
  }

  function presetKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      if (presetNameClean && !presetsFull) commitPreset();
    } else if (event.key === "Escape") {
      event.preventDefault();
      presetName = null;
    }
  }

  // Until now the only way out was forgetting the active server's pinned
  // certificate — with a publicly trusted certificate there was none at all.
  // Rust (`commands::disconnect`) revokes the token and drops credentials and
  // queue; the settings and the pinned certificates stay.
  async function signOut() {
    const ok = await confirm.ask({
      title: m.settings_sign_out_title(),
      body: m.settings_sign_out_confirm(),
      confirmLabel: m.settings_sign_out(),
      danger: true,
    });
    if (!ok) return;
    try {
      await session.disconnect();
    } catch (e) {
      player.error = String(e);
    }
  }

  async function removePreset(preset: EqPreset) {
    const accepted = await confirm.ask({
      title: m.settings_eq_delete_title(),
      body: m.settings_eq_delete_body({ name: preset.name }),
      confirmLabel: m.settings_eq_delete_action(),
      danger: true,
    });
    if (!accepted) return;
    customPresets = deleteCustomPreset(customPresets, preset.name);
    persistCustomPresets(customPresets);
  }

  function setAccent(option: AccentOption) {
    theme.accent = option.accent;
    theme.accentHover = option.accentHover;
    theme.accentMode = "fixed";
    applyTheme($state.snapshot(theme));
  }

  function setCustomAccent(hex: string) {
    theme.accent = hex;
    theme.accentHover = lightenHex(hex);
    theme.accentMode = "fixed";
    applyTheme($state.snapshot(theme));
  }

  // The fixed accent stays stored as the fallback for covers without a clear color.
  function setCoverAccent() {
    theme.accentMode = "cover";
    applyTheme($state.snapshot(theme));
  }

  const fixedAccent = $derived(theme.accentMode === "fixed");
  // True when the accent isn't one of the presets (a custom color is active).
  const customAccent = $derived(fixedAccent && !ACCENTS.some((a) => a.accent === theme.accent));

  // Startup view: where to land after sign-in (frontend-only preference).
  const STARTUP_VIEWS = [
    { path: "/", label: m.nav_albums },
    { path: "/home", label: m.nav_home },
    { path: "/songs", label: m.nav_songs },
    { path: "/artists", label: m.nav_artists },
    { path: "/playlists", label: m.nav_playlists },
    { path: "/favorites", label: m.nav_favorites },
    { path: "/discover", label: m.nav_discover },
    { path: "/smart", label: m.nav_smart_views },
    { path: "/collections", label: m.nav_collections },
  ];
  let startupView = $state(localStorage.getItem("jellysic.startupView") ?? "/");
  function setStartupView(path: string) {
    startupView = path;
    localStorage.setItem("jellysic.startupView", path);
  }

  /** Auto-DJ numbers the backend last accepted — the fallback for an emptied
   *  number input. Rust's defaults until the settings have loaded. */
  let savedAutoDj = { maxTracks: 25, recentTracks: 50 };

  function rememberAutoDj(next: ExtrasSettings) {
    savedAutoDj = { maxTracks: next.autoDjMaxTracks, recentTracks: next.autoDjRecentTracks };
  }

  /** A whole number within the backend's range (it clamps to the same bounds). */
  function wholeInRange(value: number | null, min: number, max: number, fallback: number): number {
    const n = typeof value === "number" && Number.isFinite(value) ? value : fallback;
    return Math.min(max, Math.max(min, Math.round(n)));
  }

  /** Backend semantics for `listenbrainzToken`: a value stores the token, ""
   *  clears it, null keeps the stored one. Only `saveToken` passes one — a
   *  draft typed into the token field must not ride along (untrimmed, never
   *  confirmed) with an Auto-DJ toggle. */
  function persistExtras(current: ExtrasSettings, listenbrainzToken: string | null = null): Promise<void> {
    // `bind:value` on a number input yields null when emptied and keeps
    // decimals; the backend deserializes u32, so either would fail this save
    // and every later one.
    current.autoDjMaxTracks = wholeInRange(current.autoDjMaxTracks, 5, 100, savedAutoDj.maxTracks);
    current.autoDjRecentTracks = wholeInRange(
      current.autoDjRecentTracks,
      1,
      500,
      savedAutoDj.recentTracks,
    );
    const snapshot = { ...$state.snapshot(current), listenbrainzToken };
    return api.setExtrasSettings(snapshot).then(() => rememberAutoDj(snapshot));
  }

  function saveExtras() {
    if (extras) player.run(persistExtras(extras));
  }

  function saveToken() {
    if (!extras) return;
    const current = extras;
    const token = current.listenbrainzToken?.trim() ?? "";
    const saved = persistExtras(current, token);
    // The token never lingers in the DOM.
    current.listenbrainzToken = null;
    player.run(
      saved.then(() => {
        current.listenbrainzConfigured = token !== "";
      }),
    );
  }

  async function exportToFile() {
    importedCount = null;
    backupError = null;
    try {
      const json = await api.exportSettings();
      const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = "jellysic-settings.json";
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      backupError = String(e);
    }
  }

  async function importFromFile(file: File) {
    importedCount = null;
    backupError = null;
    try {
      const keys = await api.importSettings(await file.text());
      importedCount = keys.length;
      // Reflect the imported values — otherwise the next control change
      // would write the stale pre-import objects back.
      settings = await api.getAudioSettings();
      playback = await api.getPlaybackSettings();
      const nextExtras = await api.getExtrasSettings();
      extras = nextExtras;
      rememberAutoDj(nextExtras);
      desktop = await api.getDesktopSettings();
      cacheInfo = await api.getCacheInfo();
    } catch (e) {
      backupError = String(e);
    }
  }
</script>

<div class="max-w-2xl p-6">
  <h1 class="mb-6 text-2xl font-bold tracking-tight">{m.settings_title()}</h1>

  <section class="mb-8 rounded-lg bg-card p-5">
    <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
      {m.settings_language()}
    </h2>
    <div class="flex items-center gap-2">
      {#each LOCALES as locale (locale.code)}
        <button
          class="rounded-full px-4 py-1.5 text-sm font-bold transition-colors
            {getLocale() === locale.code
            ? 'bg-accent text-(--color-on-accent)'
            : 'border border-edge text-ink-muted hover:border-ink-muted hover:text-ink'}"
          onclick={() => changeLocale(locale.code)}
        >
          {locale.label}
        </button>
      {/each}
    </div>
  </section>

  <section class="mb-8 rounded-lg bg-card p-5">
    <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
      {m.settings_appearance()}
    </h2>

    <div>
      <p class="mb-2 text-sm font-medium">{m.settings_accent()}</p>
      <div class="flex items-center gap-3">
        {#each ACCENTS as option (option.id)}
          <button
            class="h-7 w-7 rounded-full ring-offset-2 ring-offset-card transition-transform hover:scale-110
              {fixedAccent && theme.accent === option.accent ? 'ring-2 ring-ink' : 'ring-1 ring-edge'}"
            style="background-color: {option.accent}"
            aria-label={ACCENT_LABELS[option.id]?.() ?? option.id}
            title={ACCENT_LABELS[option.id]?.() ?? option.id}
            onclick={() => setAccent(option)}
          ></button>
        {/each}
        <label
          class="relative h-7 w-7 cursor-pointer rounded-full ring-offset-2 ring-offset-card transition-transform hover:scale-110
            {customAccent ? 'ring-2 ring-ink' : 'ring-1 ring-edge'}"
          style="background: conic-gradient(#ef4444, #f97316, #eab308, #22c55e, #06b6d4, #3b82f6, #a855f7, #ef4444)"
          title={m.settings_accent_custom()}
        >
          <input
            type="color"
            value={theme.accent}
            oninput={(e) => setCustomAccent((e.currentTarget as HTMLInputElement).value)}
            class="absolute inset-0 cursor-pointer opacity-0"
            aria-label={m.settings_accent_custom()}
          />
        </label>
        <button
          class="flex h-7 items-center gap-1.5 rounded-full py-1 pr-3 pl-1 text-xs font-semibold ring-offset-2 ring-offset-card transition-transform hover:scale-105
            {fixedAccent ? 'text-ink-muted ring-1 ring-edge' : 'text-ink ring-2 ring-ink'}"
          aria-pressed={!fixedAccent}
          title={m.settings_accent_cover_hint()}
          onclick={setCoverAccent}
        >
          <!-- Previews the playing cover's colors (runtime vars, no literals). -->
          <span
            class="h-5 w-5 rounded-full"
            style="background: linear-gradient(135deg, var(--cover-a), var(--cover-b))"
          ></span>
          {m.settings_accent_cover()}
        </button>
      </div>
      {#if !fixedAccent}
        <p class="mt-2 max-w-md text-xs text-ink-muted">{m.settings_accent_cover_hint()}</p>
      {/if}
    </div>

    <div class="mt-5">
      <p class="mb-2 text-sm font-medium">{m.settings_startup_view()}</p>
      <select
        class="w-full max-w-xs rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
        value={startupView}
        onchange={(e) => setStartupView((e.currentTarget as HTMLSelectElement).value)}
      >
        {#each STARTUP_VIEWS as view (view.path)}
          <option value={view.path}>{view.label()}</option>
        {/each}
      </select>
    </div>
  </section>

  <LayoutSettings />
  <ShortcutSettings />

  {#if desktop}
    <section class="mb-8 rounded-lg bg-card p-5">
      <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_desktop()}
      </h2>

      <label class="block">
        <span class="mb-1 block text-sm font-medium">{m.settings_close_behavior()}</span>
        <select
          class="w-full max-w-xs rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
          bind:value={desktop.closeBehavior}
          onchange={() => void saveDesktop()}
        >
          <option value="quit">{m.settings_close_quit()}</option>
          <option value="tray">{m.settings_close_tray()}</option>
        </select>
        <p class="mt-1.5 max-w-md text-xs text-ink-muted">{m.settings_close_hint()}</p>
      </label>

      <label class="mt-5 flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{m.settings_start_minimized()}</p>
          <p class="mt-0.5 max-w-md text-xs text-ink-muted">
            {m.settings_start_minimized_hint()}
          </p>
        </div>
        <input
          type="checkbox"
          class="h-4 w-4 accent-(--color-accent)"
          bind:checked={desktop.startMinimized}
          onchange={() => void saveDesktop()}
        />
      </label>
    </section>
  {/if}

  <UpdateSettings bind:desktop onsave={() => void saveDesktop()} />

  {#if settings}
    <section class="mb-8 rounded-lg bg-card p-5">
      <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_playback()}
      </h2>

      <label class="flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{m.settings_normalization()}</p>
          <p class="mt-0.5 max-w-md text-xs text-ink-muted">
            {m.settings_normalization_hint()}
          </p>
        </div>
        <input
          type="checkbox"
          class="h-4 w-4 accent-(--color-accent)"
          bind:checked={settings.normalizationEnabled}
          onchange={save}
        />
      </label>

      <label class="mt-5 block">
        <div class="mb-1 flex items-center justify-between text-sm">
          <span class="font-medium">{m.settings_preamp()}</span>
          <span class="text-xs text-ink-muted tabular-nums">
            {settings.preampDb >= 0 ? "+" : ""}{settings.preampDb.toFixed(1)} dB
          </span>
        </div>
        <input
          type="range"
          class="h-1 w-full accent-(--color-accent)"
          min="-15"
          max="15"
          step="0.5"
          disabled={!settings.normalizationEnabled}
          bind:value={settings.preampDb}
          onchange={save}
        />
      </label>

      {#if playback}
        <label class="mt-6 flex items-center justify-between gap-4">
          <div>
            <p class="text-sm font-medium">{m.settings_crossfade()}</p>
            <p class="mt-0.5 max-w-md text-xs text-ink-muted">{m.settings_crossfade_hint()}</p>
          </div>
          <select
            class="rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
            bind:value={playback.crossfadeMode}
            onchange={savePlayback}
          >
            <option value="off">{m.settings_crossfade_off()}</option>
            <option value="smart">{m.settings_crossfade_smart()}</option>
            <option value="always">{m.settings_crossfade_always()}</option>
          </select>
        </label>
        <label class="mt-3 block">
          <div class="mb-1 flex items-center justify-between text-sm">
            <span class="sr-only">{m.settings_crossfade()}</span>
            <span></span>
            <span class="text-xs text-ink-muted tabular-nums">
              {m.settings_crossfade_seconds({ seconds: playback.crossfadeSeconds.toFixed(0) })}
            </span>
          </div>
          <input
            type="range"
            class="h-1 w-full accent-(--color-accent)"
            min="1"
            max="12"
            step="1"
            disabled={playback.crossfadeMode === "off"}
            bind:value={playback.crossfadeSeconds}
            onchange={savePlayback}
          />
        </label>

        <label class="mt-6 block">
          <span class="mb-1 block text-sm font-medium">{m.settings_output_device()}</span>
          <select
            class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
            value={playback.outputDevice ?? ""}
            onchange={(e) => {
              if (playback) {
                playback.outputDevice = e.currentTarget.value || null;
                savePlayback();
              }
            }}
          >
            <option value="">{m.settings_output_default()}</option>
            {#if selectedOutputMissing && playback.outputDevice}
              <option value={playback.outputDevice} disabled>
                {m.settings_output_unavailable({ device: playback.outputDevice })}
              </option>
            {/if}
            <!-- keyed by index: duplicate device names are possible -->
            {#each audioDevices.devices as device, i (i)}
              <option value={device}>{device}</option>
            {/each}
          </select>
          {#if outputFallbackActive && playback.outputDevice}
            <span class="mt-1.5 block max-w-lg text-xs text-ink-muted">
              {selectedOutputMissing
                ? m.settings_output_fallback_missing({ device: playback.outputDevice })
                : m.settings_output_fallback_returned({ device: playback.outputDevice })}
            </span>
          {/if}
        </label>
      {/if}
    </section>

    <section class="mb-8 rounded-lg bg-card p-5">
      <div class="mb-4 flex items-center justify-between">
        <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted">
          {m.settings_equalizer()}
        </h2>
        <label class="flex items-center gap-2 text-sm">
          {m.settings_eq_enable()}
          <input
            type="checkbox"
            class="h-4 w-4 accent-(--color-accent)"
            bind:checked={settings.eqEnabled}
            onchange={save}
          />
        </label>
      </div>

      <div class="mb-3 flex flex-wrap items-center gap-2">
        <span class="text-xs text-ink-muted">{m.settings_eq_preset()}</span>
        {#each PRESETS as preset (preset.id)}
          {@const active = activePreset === `builtin:${preset.id}`}
          <button
            class="rounded-full border px-3 py-1 text-xs transition-colors disabled:opacity-40
              {active
              ? 'border-accent bg-accent/10 text-ink'
              : 'border-edge text-ink-muted hover:border-ink-muted hover:text-ink'}"
            aria-pressed={active}
            disabled={!settings.eqEnabled}
            onclick={() => applyPreset(preset.gains)}
          >
            {preset.label()}
          </button>
        {/each}
        {#each customPresets as preset (preset.name)}
          {@const active = activePreset === `custom:${preset.name}`}
          <span
            class="inline-flex max-w-full items-center rounded-full border text-xs transition-colors
              {active ? 'border-accent bg-accent/10 text-ink' : 'border-edge text-ink-muted'}"
          >
            <button
              class="min-w-0 truncate py-1 pr-1 pl-3 transition-colors hover:text-ink disabled:opacity-40"
              aria-pressed={active}
              disabled={!settings.eqEnabled}
              title={preset.name}
              onclick={() => applyPreset(preset.gains)}
            >
              {preset.name}
            </button>
            <button
              class="shrink-0 rounded-full py-1 pr-2 pl-1 text-ink-muted transition-colors hover:text-red-400"
              aria-label={m.settings_eq_delete_preset({ name: preset.name })}
              title={m.settings_eq_delete_preset({ name: preset.name })}
              onclick={() => removePreset(preset)}
            >
              <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true">
                <path
                  d="M6.4 5 12 10.6 17.6 5 19 6.4 13.4 12l5.6 5.6-1.4 1.4-5.6-5.6L6.4 19 5 17.6 10.6 12 5 6.4z"
                />
              </svg>
            </button>
          </span>
        {/each}
      </div>

      <div class="mb-4 flex flex-wrap items-center gap-2">
        {#if presetName === null}
          <button
            class="rounded-full border border-dashed border-edge px-3 py-1 text-xs text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
            disabled={!settings.eqEnabled}
            onclick={() => (presetName = "")}
          >
            {m.settings_eq_save_preset()}
          </button>
        {:else}
          <input
            class="w-56 max-w-full rounded-md border border-edge bg-base px-2.5 py-1 text-xs outline-none placeholder:text-ink-muted focus:border-accent"
            bind:value={presetName}
            maxlength={MAX_PRESET_NAME}
            placeholder={m.settings_eq_preset_name()}
            aria-label={m.settings_eq_preset_name()}
            onkeydown={presetKeydown}
            {@attach (node: HTMLInputElement) => node.focus()}
          />
          <button
            class="rounded-full bg-accent px-3 py-1 text-xs font-semibold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-40"
            disabled={!presetNameClean || presetsFull}
            onclick={commitPreset}
          >
            {presetOverwrites ? m.settings_eq_preset_overwrite() : m.settings_save()}
          </button>
          <button
            class="rounded-full border border-edge px-3 py-1 text-xs text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
            onclick={() => (presetName = null)}
          >
            {m.cancel()}
          </button>
          {#if presetsFull}
            <span class="text-xs text-ink-muted">
              {m.settings_eq_presets_full({ max: MAX_CUSTOM_PRESETS })}
            </span>
          {/if}
        {/if}
      </div>

      <div
        class="flex items-end justify-between gap-1 {settings.eqEnabled ? '' : 'opacity-40'}"
      >
        {#each BAND_LABELS as label, i (label)}
          <div class="flex flex-col items-center gap-1">
            <span class="text-[10px] text-ink-muted tabular-nums">
              {settings.eqGainsDb[i] > 0 ? "+" : ""}{settings.eqGainsDb[i].toFixed(0)}
            </span>
            <input
              type="range"
              class="eq-slider accent-(--color-accent)"
              min="-12"
              max="12"
              step="0.5"
              disabled={!settings.eqEnabled}
              bind:value={settings.eqGainsDb[i]}
              onchange={save}
            />
            <span class="text-[10px] text-ink-muted">{label}</span>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if extras}
    <section class="mb-8 rounded-lg bg-card p-5">
      <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_scrobbling()}
      </h2>
      <label class="block">
        <span class="mb-1 block text-sm font-medium">{m.settings_listenbrainz_token()}</span>
        <div class="flex items-center gap-2">
          <input
            type="password"
            autocomplete="off"
            class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
            placeholder={extras.listenbrainzConfigured ? m.settings_listenbrainz_saved() : ""}
            value={extras.listenbrainzToken ?? ""}
            oninput={(e) => {
              if (extras) extras.listenbrainzToken = e.currentTarget.value;
            }}
          />
          <button
            class="shrink-0 rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover"
            onclick={saveToken}
          >
            {m.settings_save()}
          </button>
        </div>
        <p class="mt-1.5 text-xs text-ink-muted">{m.settings_listenbrainz_hint()}</p>
      </label>
    </section>

    <section class="mb-8 rounded-lg bg-card p-5">
      <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_integrations()}
      </h2>
      <label class="flex items-center justify-between gap-4">
        <div>
          <p class="text-sm font-medium">{m.settings_autodj()}</p>
          <p class="mt-0.5 max-w-md text-xs text-ink-muted">{m.settings_autodj_hint()}</p>
        </div>
        <input
          type="checkbox"
          class="h-4 w-4 accent-(--color-accent)"
          bind:checked={extras.autoDjEnabled}
          onchange={saveExtras}
        />
      </label>
      {#if extras.autoDjEnabled}
        <div class="mt-4 grid gap-4 sm:grid-cols-3">
          <label class="block">
            <span class="mb-1 block text-xs text-ink-muted">{m.settings_autodj_seed()}</span>
            <select
              class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
              bind:value={extras.autoDjSeedMode}
              onchange={saveExtras}
            >
              <option value="track">{m.settings_autodj_seed_track()}</option>
              <option value="artist">{m.settings_autodj_seed_artist()}</option>
              <option value="genre">{m.settings_autodj_seed_genre()}</option>
              <option value="album">{m.settings_autodj_seed_album()}</option>
              <option value="playlist">{m.settings_autodj_seed_playlist()}</option>
            </select>
          </label>
          <label class="block">
            <span class="mb-1 block text-xs text-ink-muted">{m.settings_autodj_length()}</span>
            <input
              type="number"
              min="5"
              max="100"
              class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
              bind:value={extras.autoDjMaxTracks}
              onchange={saveExtras}
            />
          </label>
          <label class="block">
            <span class="mb-1 block text-xs text-ink-muted">{m.settings_autodj_recent()}</span>
            <input
              type="number"
              min="1"
              max="500"
              class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
              bind:value={extras.autoDjRecentTracks}
              onchange={saveExtras}
            />
          </label>
        </div>
      {/if}
    </section>
  {/if}

  {#if session.info}
    <section class="mb-8 rounded-lg bg-card p-5">
      <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_account()}
      </h2>
      <div class="flex flex-wrap items-center justify-between gap-3">
        <p class="min-w-0 text-sm">
          <span class="block font-medium">{session.info.username}</span>
          <span class="block break-all text-xs text-ink-muted">{session.info.serverUrl}</span>
        </p>
        <button
          class="shrink-0 rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-red-400 hover:text-red-400"
          onclick={signOut}
        >
          {m.settings_sign_out()}
        </button>
      </div>
    </section>
  {/if}

  <TrustedCertSettings />

  <section class="mb-8 rounded-lg bg-card p-5">
    <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
      {m.settings_cache_diagnostics()}
    </h2>

    <div class="flex flex-wrap items-end justify-between gap-4">
      <div>
        <p class="text-sm font-medium">{m.settings_cover_cache()}</p>
        {#if cacheInfo}
          <p class="mt-0.5 text-xs text-ink-muted">
            {(cacheInfo.fileCount === 1 ? m.settings_cache_usage_one : m.settings_cache_usage_many)({
              size: formatBytes(cacheInfo.sizeBytes),
              count: cacheInfo.fileCount,
            })}
          </p>
        {/if}
      </div>
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
        disabled={maintenanceBusy || !cacheInfo}
        onclick={() => void clearCache()}
      >
        {m.settings_cache_clear()}
      </button>
    </div>

    {#if desktop}
      <label class="mt-4 block max-w-xs">
        <span class="mb-1 block text-xs text-ink-muted">{m.settings_cache_limit()}</span>
        <select
          class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
          bind:value={desktop.coverCacheLimitMb}
          onchange={() => void saveDesktop()}
        >
          {#each CACHE_LIMITS_MB as limit (limit)}
            <option value={limit}>{formatBytes(limit * 1024 * 1024)}</option>
          {/each}
        </select>
      </label>
    {/if}

    <div class="mt-6 border-t border-edge pt-5">
      <p class="text-sm font-medium">{m.settings_diagnostics()}</p>
      <p class="mt-0.5 max-w-lg text-xs text-ink-muted">{m.settings_diagnostics_hint()}</p>
      <button
        class="mt-3 rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
        disabled={maintenanceBusy}
        onclick={() => void exportDiagnostics()}
      >
        {m.settings_diagnostics_export()}
      </button>
      {#if diagnosticsPath}
        <div class="mt-3 rounded-md bg-base px-3 py-2">
          <p class="text-[11px] text-ink-muted">{m.settings_diagnostics_saved_at()}</p>
          <p class="mt-0.5 break-all font-mono text-[11px] text-ink">{diagnosticsPath}</p>
          <button
            class="mt-2 rounded-full border border-edge px-3 py-1 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
            onclick={revealDiagnostics}
          >
            {m.download_open_folder()}
          </button>
        </div>
      {/if}
    </div>

    {#if maintenanceMessage}
      <p class="mt-3 text-xs font-medium text-accent">{maintenanceMessage}</p>
    {/if}
    {#if maintenanceError}
      <p class="mt-3 text-xs text-red-400">
        {m.error_generic({ message: maintenanceError })}
      </p>
    {/if}
  </section>

  <section class="rounded-lg bg-card p-5">
    <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
      {m.settings_backup()}
    </h2>
    <div class="flex items-center gap-2">
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={exportToFile}
      >
        {m.settings_export()}
      </button>
      <button
        class="rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={() => importInput?.click()}
      >
        {m.settings_import()}
      </button>
    </div>
    {#if importedCount !== null}
      <p class="mt-3 text-xs font-medium text-accent">
        {(importedCount === 1 ? m.settings_imported_one : m.settings_imported_many)({
          count: importedCount,
        })}
      </p>
    {/if}
    {#if backupError}
      <p
        class="mt-3 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-xs text-red-300"
      >
        {m.error_generic({ message: backupError })}
      </p>
    {/if}
    <input
      bind:this={importInput}
      type="file"
      accept="application/json,.json"
      class="hidden"
      onchange={(e) => {
        const file = e.currentTarget.files?.[0];
        e.currentTarget.value = "";
        if (file) importFromFile(file);
      }}
    />
  </section>
</div>

<style>
  .eq-slider {
    writing-mode: vertical-lr;
    direction: rtl;
    height: 8rem;
    width: 1rem;
  }
</style>
