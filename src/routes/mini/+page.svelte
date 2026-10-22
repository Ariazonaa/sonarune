<script lang="ts">
  import { onMount } from "svelte";
  import { m } from "#lib/paraglide/messages.js";
  import Cover from "#lib/components/Cover.svelte";
  import { hasNext } from "#lib/queueOrder.js";
  import PlaybackProgress from "#lib/components/PlaybackProgress.svelte";
  import { player } from "#lib/state/player.svelte.js";

  const s = $derived(player.state);
  const playing = $derived(player.playing);
  // The player bar's rule (TransportControls): follows queue and repeat mode.
  const repeat = $derived(player.state.repeat);
  const canGoNext = $derived(hasNext(player.queue, repeat));

  // This window bypasses the app shell (see root layout), so it wires up its
  // own player event listeners. Rust state/session are shared process-wide.
  onMount(() => {
    player.init();
  });

  async function closeWindow() {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  }
</script>

<div class="relative flex h-screen w-screen items-center gap-3 overflow-hidden bg-base p-3" data-tauri-drag-region>
  <div class="relative w-[88px] shrink-0">
    <Cover
      itemId={s.current?.imageItemId ?? null}
      tag={s.current?.imageTag ?? null}
      size={192}
      alt=""
      blurhash={s.current?.imageBlurHash ?? null}
    />
    <!-- Cover stays part of the drag surface. -->
    <div class="absolute inset-0" data-tauri-drag-region></div>
  </div>

  <div class="flex min-w-0 flex-1 flex-col gap-1.5" data-tauri-drag-region>
    <div class="min-w-0 pr-6" data-tauri-drag-region>
      {#if s.current}
        <p class="truncate text-sm font-semibold" data-tauri-drag-region>{s.current.name}</p>
        <p class="truncate text-xs text-ink-muted" data-tauri-drag-region>{s.current.artist}</p>
      {:else}
        <p class="truncate text-sm text-ink-muted" data-tauri-drag-region>{m.player_nothing_playing()}</p>
      {/if}
    </div>

    <div class="flex items-center gap-2">
      <button
        class="rounded-full p-1 text-ink-muted transition-colors hover:text-ink disabled:opacity-40"
        onclick={() => player.run(player.prev())}
        disabled={!s.current}
        aria-label={m.cmd_prev()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M6 6h2v12H6V6zm3.5 6l8.5 6V6l-8.5 6z"/></svg>
      </button>
      <button
        class="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-ink text-(--color-base) transition-transform hover:scale-105 disabled:opacity-40"
        onclick={() => player.run(player.toggle())}
        disabled={!s.current && s.queueLen === 0}
        aria-label={m.cmd_play_pause()}
      >
        {#if playing}
          <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor"><path d="M6 5h4v14H6V5zm8 0h4v14h-4V5z"/></svg>
        {:else}
          <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor"><path d="M8 5v14l11-7L8 5z"/></svg>
        {/if}
      </button>
      <button
        class="rounded-full p-1 text-ink-muted transition-colors hover:text-ink disabled:opacity-40"
        onclick={() => player.run(player.next())}
        disabled={!s.current || !canGoNext}
        aria-label={m.cmd_next()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor"><path d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z"/></svg>
      </button>
    </div>

    <!-- The player bar's own seek row: 5 s key steps instead of a range
         input's 1 ms (each press reopened a stream not yet downloaded), and a
         drag held across a track change is dropped instead of seeking the new
         track to a spot picked on the old one. -->
    <PlaybackProgress />
  </div>

  <button
    class="absolute top-1.5 right-1.5 rounded p-1 text-ink-muted transition-colors hover:text-ink"
    onclick={closeWindow}
    aria-label={m.mini_close()}
    title={m.mini_close()}
  >
    <svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor">
      <path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/>
    </svg>
  </button>
</div>
