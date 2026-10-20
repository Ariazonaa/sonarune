<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { goto, onNavigate } from "$app/navigation";
  import Sidebar from "#lib/components/Sidebar.svelte";
  import PlayerBar from "#lib/components/PlayerBar.svelte";
  import QueuePanel from "#lib/components/QueuePanel.svelte";
  import LyricsPanel from "#lib/components/LyricsPanel.svelte";
  import SetupScreen from "#lib/components/SetupScreen.svelte";
  import CertificateBanner from "#lib/components/CertificateBanner.svelte";
  import TitleBar from "#lib/components/TitleBar.svelte";
  import CommandPalette from "#lib/components/CommandPalette.svelte";
  import ShortcutHelp from "#lib/components/ShortcutHelp.svelte";
  import DownloadPanel from "#lib/components/DownloadPanel.svelte";
  import ConfirmDialog from "#lib/components/ConfirmDialog.svelte";
  import Toaster from "#lib/components/Toaster.svelte";
  import { session } from "#lib/state/session.svelte.js";
  import { player } from "#lib/state/player.svelte.js";
  import { library } from "#lib/state/library.svelte.js";
  import { sonic } from "#lib/state/sonic.svelte.js";
  import { net } from "#lib/state/net.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import { updates } from "#lib/state/updates.svelte.js";
  import { palette } from "#lib/state/palette.svelte.js";
  import { help } from "#lib/state/help.svelte.js";
  import { layoutPreferences } from "#lib/state/layout.svelte.js";
  import { installShortcuts } from "#lib/shortcuts.js";
  import { isTextField } from "#lib/controls.js";
  import { syncTrayLabels } from "#lib/tray.js";
  import { coverUrl } from "#lib/api.js";
  import { ambientColors } from "#lib/ambient.js";
  import { setCoverHue } from "#lib/theme.js";
  import { m } from "#lib/paraglide/messages.js";
  import { getLocale } from "#lib/paraglide/runtime.js";

  let { children } = $props();

  // The document's language, for screen readers (a German label read with an
  // English voice is barely understandable). app.html says "en"; Paraglide
  // only resolves the locale, and changing it reloads the page, so setting
  // it once per load is enough.
  document.documentElement.lang = getLocale();

  // Cross-fade route changes via the View Transitions API (Chromium/WebView2).
  onNavigate((navigation) => {
    if (navigation.shallow && navigation.type === 'goto') return;

    const doc = document as 
      Document &
      { 
        startViewTransition?: (cb: () => Promise<void> | void) => void
       };
    if (!doc.startViewTransition) return;
    return new Promise<void>((resolve) => {
      (doc.startViewTransition!)(async () => {
        resolve();
        await navigation.complete;
      });
    });
  });

  // Secondary windows render standalone without the main app shell. The mini
  // page initializes its player locally; the projector also restores the
  // session so its synchronized-lyrics store can fetch the current track.
  const isProjector = $derived(page.url.pathname === "/projector");
  const isStandalone = $derived(page.url.pathname === "/mini" || isProjector);
  const viewLayout = $derived(layoutPreferences.active);

  onMount(() => {
    // Suppress the WebView's default right-click menu (Back / Refresh / Print
    // / Save as …) everywhere except text fields (keep copy/paste there) — a
    // slider or a checkbox is an <input> too, but has nothing to paste, and
    // Refresh would reload the whole UI. The app's own context menus still
    // fire — their handlers run on the target first, this only kills the
    // browser menu on unhandled elements.
    window.addEventListener("contextmenu", (e) => {
      if (isTextField(e.target)) return;
      e.preventDefault();
    });

    if (isStandalone) {
      if (isProjector) {
        session.restore();
        player.init();
      }
      return;
    }
    void syncTrayLabels();
    // A session kept without reaching the server (offline grace, or on a
    // certificate that is not trusted) shows its banner right away, not only
    // at the next focus. Only this window follows a session the backend
    // ends: it is the one with the sign-in screen.
    // Signed out, the check is what announces a session the startup restore
    // in Rust ended before this window listened: check_server repeats it.
    void session.restore({ followEnds: true }).then(() => net.check());
    player.init();
    library.init();
    void sonic.init();
    net.init();
    updates.init();
  });

  // A found update is announced once, as a pill — it is never urgent enough to
  // interrupt, and Settings holds the button that acts on it.
  $effect(() => {
    const version = updates.info?.version;
    if (!version || updates.announced === version) return;
    updates.announced = version;
    toast.show(m.toast_update_available({ version }), { kind: "info", ms: 6000 });
  });

  // Global shortcuts only for the signed-in main-window shell.
  $effect(() => {
    if (isStandalone || !session.info) return;
    return installShortcuts();
  });

  // Land on the user's chosen startup view once, right after sign-in — once
  // per app start, not per page load. Switching the language reloads the
  // page, and the user who just picked Deutsch in Settings must stay there.
  // sessionStorage survives a reload of this window but not a restart.
  const STARTUP_NAV_KEY = "sonarune.startupNavDone";
  function startupNavDone(): boolean {
    try {
      return sessionStorage.getItem(STARTUP_NAV_KEY) !== null;
    } catch {
      return false;
    }
  }
  function markStartupNavDone() {
    try {
      sessionStorage.setItem(STARTUP_NAV_KEY, "1");
    } catch {
      /* no session storage: a reload goes to the startup view again */
    }
  }
  let didStartupNav = startupNavDone();
  $effect(() => {
    if (isStandalone || !session.info || didStartupNav) return;
    didStartupNav = true;
    markStartupNavDone();
    const startup = localStorage.getItem("sonarune.startupView");
    // goto rejects a path that is no route (since SvelteKit 3); a stored view
    // from an older build then just leaves the app where it is.
    if (startup && startup !== page.url.pathname) goto(startup).catch(() => {});
  });

  // Reflect what's playing in the native window title (also shown on the
  // taskbar hover and in Alt-Tab). Derived so the effect only fires on a real
  // title change, not on every 400 ms position tick.
  const windowTitle = $derived.by(() => {
    const s = player.state;
    const c = s.current;
    if (!c || s.status === "idle") return "Sonarune";
    // A load counts as what it arrives as, like the Play/Pause button.
    const icon = player.playing ? "▶" : "⏸";
    return `${icon} ${c.artist} – ${c.name}`;
  });

  async function setNativeWindowTitle(title: string) {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTitle(title);
  }

  $effect(() => {
    if (isStandalone) return;
    void setNativeWindowTitle(windowTitle);
  });

  // Tint the app-wide Liquid Glass backdrop from the current cover: the whole
  // chrome floats over a soft field of the playing album's colors. Reuses the
  // now-playing ambient extraction; a stale resolve just re-tints for the newer
  // track on its own pass. The same extraction feeds the "accent from cover"
  // theme option (a no-op while the accent is fixed). Keyed on the cover, not
  // on the player state: that is replaced on every 400 ms tick, and each pass
  // re-reads the theme, forces a style flush, rewrites storage and wakes the
  // canvas drawers — or refetches a cover that failed to load.
  const coverSrc = $derived.by(() => {
    const cur = player.state.current;
    return cur ? coverUrl(cur.imageItemId, cur.imageTag, 360) : null;
  });
  $effect(() => {
    const src = coverSrc;
    if (!src) return;
    void ambientColors(src).then((colors) => {
      if (!colors || coverSrc !== src) return;
      const root = document.documentElement;
      root.style.setProperty("--cover-a", colors.a);
      root.style.setProperty("--cover-b", colors.b);
      setCoverHue(colors.accent);
    });
  });
</script>

{#if isStandalone}
  {@render children()}
{:else}
  <div class="app-root flex flex-col">
    <TitleBar />
    {#if session.restoring}
      <div class="flex flex-1 items-center justify-center text-sm text-ink-muted">
        {m.session_restoring()}
      </div>
    {:else if !session.info}
      <div class="min-h-0 flex-1">
        <SetupScreen />
      </div>
    {:else}
      {#if net.untrustedCertificate}
        <CertificateBanner fingerprint={net.untrustedCertificate} />
      {:else if !net.online}
        <button
          class="flex w-full items-center justify-center gap-3 bg-amber-500/90 px-4 py-1.5 text-xs font-semibold text-black"
          onclick={() => net.retry()}
        >
          {m.offline_banner()}
          <span class="rounded-full bg-black/20 px-2 py-0.5">{m.retry()}</span>
        </button>
      {/if}
      <div class="flex min-h-0 flex-1 gap-2 p-2 pb-0">
        <Sidebar />
        <main
          class="surface min-w-0 flex-1 overflow-y-auto rounded-panel bg-panel"
          data-testid="app-shell"
          data-view-density={viewLayout.density}
          style="--view-card-min: {layoutPreferences.cardPixels.min}px; --view-card-row: {layoutPreferences.cardPixels.min}px;"
        >
          {@render children()}
        </main>
        {#if player.showQueue}
          <QueuePanel />
        {:else if player.showLyrics}
          <LyricsPanel />
        {/if}
      </div>
      {#if player.error}
        <button
          class="border-t border-red-900 bg-red-950 px-4 py-1.5 text-left text-xs text-red-300"
          data-testid="player-error"
          onclick={() => player.error = null}
          title={m.dismiss()}
        >
          {player.error}
        </button>
      {/if}
      <PlayerBar />
      <DownloadPanel />
      {#if palette.open}
        <CommandPalette />
      {/if}
      {#if help.open}
        <ShortcutHelp />
      {/if}
      <ConfirmDialog />
      <Toaster />
    {/if}
  </div>
{/if}
