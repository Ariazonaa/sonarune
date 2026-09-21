<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { m } from "$lib/paraglide/messages";

  const win = getCurrentWindow();

  /** Windows swaps the maximize button for a restore button; mirror that. */
  let maximized = $state(false);

  $effect(() => {
    let unlisten: (() => void) | null = null;
    let gone = false;
    const sync = () => void win.isMaximized().then((v) => (maximized = v));
    sync();
    void win.onResized(sync).then((fn) => {
      // The listener may only arrive after the component is already gone.
      if (gone) fn();
      else unlisten = fn;
    });
    return () => {
      gone = true;
      unlisten?.();
    };
  });
</script>

<!-- Custom title bar for the frameless window. The whole strip is the drag
     region; the window controls sit on the right, as on Windows. -->
<div class="tb flex h-8 shrink-0 items-center justify-end" data-tauri-drag-region>
  <button
    class="ctl"
    onclick={() => win.minimize()}
    aria-label={m.window_minimize()}
    title={m.window_minimize()}
  >
    <svg viewBox="0 0 10 10" class="glyph" aria-hidden="true">
      <path d="M0 5h10" />
    </svg>
  </button>
  <button
    class="ctl"
    onclick={() => win.toggleMaximize()}
    aria-label={maximized ? m.window_restore() : m.window_maximize()}
    title={maximized ? m.window_restore() : m.window_maximize()}
  >
    <svg viewBox="0 0 10 10" class="glyph" aria-hidden="true">
      {#if maximized}
        <path d="M2.6 2.6V1.4a.9.9 0 0 1 .9-.9h5.1a.9.9 0 0 1 .9.9v5.1a.9.9 0 0 1-.9.9H7.4" />
        <rect x="0.5" y="2.6" width="6.9" height="6.9" rx="0.9" />
      {:else}
        <rect x="0.5" y="0.5" width="9" height="9" rx="0.9" />
      {/if}
    </svg>
  </button>
  <button
    class="ctl ctl-close"
    onclick={() => win.close()}
    aria-label={m.window_close()}
    title={m.window_close()}
  >
    <svg viewBox="0 0 10 10" class="glyph" aria-hidden="true">
      <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" />
    </svg>
  </button>
</div>

<style>
  .tb {
    -webkit-app-region: drag;
  }
  /* 46x32 is the Windows hit area. The app-root's rounded corner clips the
     close button's hover, which is what every rounded Windows window does. */
  .ctl {
    -webkit-app-region: no-drag;
    display: grid;
    place-items: center;
    width: 46px;
    height: 32px;
    color: var(--color-ink-muted);
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }
  .ctl:hover {
    background-color: var(--color-panel-2);
    color: var(--color-ink);
  }
  .ctl-close:hover {
    background-color: var(--color-window-close);
    color: var(--color-ink);
  }
  .glyph {
    width: 10px;
    height: 10px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1;
  }
</style>
