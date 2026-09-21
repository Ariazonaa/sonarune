<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { goto, onNavigate } from "$app/navigation";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import PlayerBar from "$lib/components/PlayerBar.svelte";
  import QueuePanel from "$lib/components/QueuePanel.svelte";
  import LyricsPanel from "$lib/components/LyricsPanel.svelte";
  import SetupScreen from "$lib/components/SetupScreen.svelte";
  import TitleBar from "$lib/components/TitleBar.svelte";
  import CommandPalette from "$lib/components/CommandPalette.svelte";
  import ShortcutHelp from "$lib/components/ShortcutHelp.svelte";
  import DownloadPanel from "$lib/components/DownloadPanel.svelte";
  import ConfirmDialog from "$lib/components/ConfirmDialog.svelte";
  import Toaster from "$lib/components/Toaster.svelte";
  import { session } from "$lib/state/session.svelte";
  import { player } from "$lib/state/player.svelte";
  import { library } from "$lib/state/library.svelte";
  import { net } from "$lib/state/net.svelte";
  import { palette } from "$lib/state/palette.svelte";
  import { help } from "$lib/state/help.svelte";
  import { layoutPreferences } from "$lib/state/layout.svelte";
  import { installShortcuts } from "$lib/shortcuts";
  import { syncTrayLabels } from "$lib/tray";
  import { coverUrl } from "$lib/api";
  import { ambientColors } from "$lib/ambient";
  import { setCoverHue } from "$lib/theme";
  import { m } from "$lib/paraglide/messages";

  let { children } = $props();

  // Cross-fade route changes via the View Transitions API (Chromium/WebView2).
  onNavigate((navigation) => {
    const doc = document as Document & {
      startViewTransition?: (cb: () => Promise<void> | void) => void;
    };
    if (!doc.startViewTransition) return;
    return new Promise<void>((resolve) => {
      doc.startViewTransition!(async () => {
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
    // Suppress the WebView's default right-click menu (Share link / Print /
    // Save as …) everywhere except text inputs (keep copy/paste there). The
    // app's own context menus still fire — their handlers run on the target
    // first, this only kills the browser menu on unhandled elements.
    window.addEventListener("contextmenu", (e) => {
      const el = e.target as HTMLElement | null;
      const tag = el?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || el?.isContentEditable) return;
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
    session.restore();
    player.init();
    library.init();
    net.init();
  });

  // Global shortcuts only for the signed-in main-window shell.
  $effect(() => {
    if (isStandalone || !session.info) return;
    return installShortcuts();
  });

  // Land on the user's chosen startup view once, right after sign-in.
  let didStartupNav = false;
  $effect(() => {
    if (isStandalone || !session.info || didStartupNav) return;
    didStartupNav = true;
    const startup = localStorage.getItem("jellysic.startupView");
    if (startup && startup !== page.url.pathname) goto(startup);
  });

  // Reflect what's playing in the native window title (also shown on the
  // taskbar hover and in Alt-Tab). Derived so the effect only fires on a real
  // title change, not on every 400 ms position tick.
  const windowTitle = $derived.by(() => {
    const s = player.state;
    const c = s.current;
    if (!c || s.status === "idle") return "Jellysic";
    const icon = s.status === "paused" ? "⏸" : "▶";
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
  // theme option (a no-op while the accent is fixed).
  $effect(() => {
    const cur = player.state.current;
    const src = cur ? coverUrl(cur.imageItemId, cur.imageTag, 360) : null;
    if (!src) return;
    void ambientColors(src).then((colors) => {
      if (!colors || player.state.current?.imageItemId !== cur?.imageItemId) return;
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
      {#if !net.online}
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
          onclick={() => (player.error = null)}
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
