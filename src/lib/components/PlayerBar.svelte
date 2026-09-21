<script lang="ts">
  import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { player } from "$lib/state/player.svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import Cover from "./Cover.svelte";
  import PlaybackProgress from "./PlaybackProgress.svelte";
  import Popover from "./Popover.svelte";
  import TransportControls from "./TransportControls.svelte";

  const s = $derived(player.state);

  // Closing the visualizer should return to wherever it was opened from (e.g.
  // the album you were viewing), not the landing view. Track the last route
  // that wasn't the visualizer, however it was opened (button/palette/direct).
  let lastNonVisualizerPath = $state("/");
  $effect(() => {
    const path = page.url.pathname;
    if (path !== "/visualizer") lastNonVisualizerPath = path;
  });
  function toggleVisualizer() {
    goto(page.url.pathname === "/visualizer" ? lastNonVisualizerPath : "/visualizer");
  }

  // Perceptual volume taper: loudness is ~logarithmic, so a linear slider
  // crams the useful quiet/mid range into a tiny stretch. Map the slider
  // position through a power curve → more travel (finer control) where it
  // matters. The stored value stays the linear gain.
  const VOL_EXP = 2.5;
  const volToPos = (v: number) => Math.pow(Math.min(1, Math.max(0, v)), 1 / VOL_EXP);
  const posToVol = (p: number) => Math.pow(Math.min(1, Math.max(0, p)), VOL_EXP);

  const HEART_ON =
    "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z";
  const HEART_OFF =
    "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z";

  // Sleep timer: active state + popover. 0 remaining = waiting for track end.
  let sleepOpen = $state(false);
  let sleepButton = $state<HTMLElement | null>(null);
  let sleepFadeSeconds = $state(30);
  const sleepDetail = $derived(
    s.sleepRemainingMs === null
      ? null
      : s.sleepRemainingMs === 0
        ? m.sleep_end_of_track()
        : m.sleep_minutes({ minutes: Math.max(1, Math.ceil(s.sleepRemainingMs / 60000)) }),
  );

  function setSleep(minutes: number | null, endOfTrack: boolean) {
    sleepOpen = false;
    player.run(api.setSleepTimer(minutes, endOfTrack, sleepFadeSeconds));
  }

  /** Open (or focus) the frameless mini-player window. */
  async function openMini() {
    try {
      const existing = await WebviewWindow.getByLabel("mini");
      if (existing) {
        await existing.setFocus();
        return;
      }
      new WebviewWindow("mini", {
        url: "/mini",
        width: 380,
        height: 120,
        resizable: false,
        alwaysOnTop: true,
        decorations: false,
        title: "Jellysic Mini",
      });
    } catch (e) {
      player.error = String(e);
    }
  }

  // Mute state (the toggle itself lives in the player store so the global
  // shortcut and this button share one pre-mute level).
  const muted = $derived(s.volume === 0);
</script>

<footer class="surface mx-2 mb-2 flex h-20 items-center gap-4 rounded-panel bg-panel px-4">
  <!-- Now playing (click -> full screen player) -->
  <div class="flex w-64 min-w-0 items-center gap-3">
    {#if s.current}
      <button
        class="flex min-w-0 items-center gap-3 rounded-md text-left transition-opacity hover:opacity-80"
        onclick={() => goto(page.url.pathname === "/now-playing" ? "/" : "/now-playing")}
        data-testid="now-playing-open"
        aria-label={m.nowplaying_open()}
        title={m.nowplaying_open()}
      >
        <div class="w-12 shrink-0">
          <Cover itemId={s.current.imageItemId} tag={s.current.imageTag} size={96} alt="" blurhash={s.current.imageBlurHash} />
        </div>
        <div class="min-w-0">
          <p class="truncate text-sm font-medium">{s.current.name}</p>
          <p class="truncate text-xs text-ink-muted">{s.current.artist}</p>
        </div>
      </button>
      <button
        class="shrink-0 rounded-full p-1.5 transition-colors {player.currentIsFavorite
          ? 'text-accent hover:text-accent-hover'
          : 'text-ink-muted hover:text-ink'}"
        onclick={() => player.toggleCurrentFavorite()}
        aria-pressed={player.currentIsFavorite}
        aria-label={player.currentIsFavorite ? m.favorite_remove() : m.favorite_add()}
        title={player.currentIsFavorite ? m.favorite_remove() : m.favorite_add()}
      >
        <svg viewBox="0 0 24 24" class="h-5 w-5" fill="currentColor" aria-hidden="true">
          <path d={player.currentIsFavorite ? HEART_ON : HEART_OFF} />
        </svg>
      </button>
    {:else}
      <p class="text-sm text-ink-muted">{m.player_nothing_playing()}</p>
    {/if}
  </div>

  <!-- Transport + seek -->
  <div class="flex min-w-0 flex-1 flex-col items-center gap-1">
    <TransportControls />
    <PlaybackProgress class="max-w-xl" />
  </div>

  <!-- Sleep timer + mini player + visualizer + panels + volume -->
  <div class="flex w-64 items-center justify-end gap-1.5">
    <div>
      {#if sleepOpen}
        <Popover
          anchor={sleepButton}
          placement="above"
          align="end"
          role="dialog"
          label={m.sleep_timer()}
          onclose={() => (sleepOpen = false)}
          class="w-48"
        >
          <p class="px-3 pt-1.5 pb-1 text-[11px] font-bold tracking-wide text-ink-muted uppercase">
            {m.sleep_timer()}
          </p>
          <button class="w-full rounded-md px-3 py-1.5 text-left text-sm hover:bg-ink/10" onclick={() => setSleep(null, true)}>
            {m.sleep_end_of_track()}
          </button>
          {#each [15, 30, 60] as minutes (minutes)}
            <button class="w-full rounded-md px-3 py-1.5 text-left text-sm hover:bg-ink/10" onclick={() => setSleep(minutes, false)}>
              {m.sleep_minutes({ minutes })}
            </button>
          {/each}
          <label class="mx-2 mt-1 block border-t border-edge px-1 pt-2 pb-1 text-xs text-ink-muted">
            <span class="mb-1 block">{m.sleep_fade()}</span>
            <select
              class="w-full rounded-md border border-edge bg-base px-2 py-1 text-xs text-ink outline-none focus:border-accent"
              bind:value={sleepFadeSeconds}
            >
              {#each [10, 30, 60] as seconds (seconds)}
                <option value={seconds}>{m.sleep_fade_seconds({ seconds })}</option>
              {/each}
            </select>
          </label>
          <button class="w-full rounded-md px-3 py-1.5 text-left text-sm hover:bg-ink/10" onclick={() => setSleep(null, false)}>
            {m.sleep_off()}
          </button>
        </Popover>
      {/if}
      <button
        bind:this={sleepButton}
        class="rounded p-1.5 transition-colors {s.sleepRemainingMs !== null
          ? 'text-accent'
          : 'text-ink-muted hover:text-ink'}"
        onclick={() => (sleepOpen = !sleepOpen)}
        aria-label={m.sleep_timer()}
        title={sleepDetail ? `${m.sleep_timer()} · ${sleepDetail}` : m.sleep_timer()}
      >
        <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor" aria-hidden="true">
          <path d="M12.1 22c-5.5 0-10-4.5-10-10 0-5 3.7-9.2 8.6-9.9.5-.1.9.4.7.9-.3.8-.4 1.6-.4 2.5 0 4.1 3.3 7.4 7.4 7.4.9 0 1.7-.1 2.5-.4.5-.2 1 .3.9.7-.8 4.9-5 8.8-9.7 8.8z"/>
        </svg>
      </button>
    </div>
    <button
      class="rounded p-1.5 text-ink-muted transition-colors hover:text-ink"
      onclick={openMini}
      aria-label={m.mini_open()}
      title={m.mini_open()}
    >
      <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor" aria-hidden="true">
        <path d="M19 7h-8v6h8V7zm4-4H1v18h22V3zm-2 16H3V5h18v14z"/>
      </svg>
    </button>
    <button
      class="rounded p-1.5 transition-colors {page.url.pathname === '/visualizer'
        ? 'text-accent'
        : 'text-ink-muted hover:text-ink'}"
      onclick={toggleVisualizer}
      aria-label={m.visualizer_show()}
      title={m.visualizer_show()}
      data-testid="visualizer-toggle"
    >
      <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor" aria-hidden="true">
        <path d="M3 13h2v4H3v-4zm4-6h2v10H7V7zm4 3h2v7h-2v-7zm4-5h2v12h-2V5zm4 8h2v4h-2v-4z"/>
      </svg>
    </button>
    <button
      class="rounded p-1.5 transition-colors {player.showLyrics
        ? 'text-accent'
        : 'text-ink-muted hover:text-ink'}"
      onclick={player.toggleLyricsPanel}
      aria-label={m.lyrics_show()}
      title={m.lyrics_show()}
    >
      <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor" aria-hidden="true">
        <path d="M12 2a4 4 0 0 0-4 4v6a4 4 0 0 0 8 0V6a4 4 0 0 0-4-4zm-6 10a6 6 0 0 0 5 5.92V21h2v-3.08A6 6 0 0 0 18 12h-2a4 4 0 0 1-8 0H6z"/>
      </svg>
    </button>
    <button
      class="rounded p-1.5 transition-colors {player.showQueue
        ? 'text-accent'
        : 'text-ink-muted hover:text-ink'}"
      onclick={player.toggleQueuePanel}
      aria-label={m.queue_show()}
      title={m.queue_show()}
    >
      <svg viewBox="0 0 24 24" class="h-4.5 w-4.5" fill="currentColor" aria-hidden="true">
        <path d="M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zM17 6v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V8h3V6h-5z"/>
      </svg>
    </button>
    <button
      class="rounded p-1 text-ink-muted transition-colors hover:text-ink"
      onclick={() => player.run(player.toggleMute())}
      aria-label={muted ? m.player_unmute() : m.player_mute()}
      title={muted ? m.player_unmute() : m.player_mute()}
    >
      {#if muted}
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
          <path d="M16.5 12A4.5 4.5 0 0 0 14 7.97v2.21l2.45 2.45c.03-.2.05-.41.05-.63zM19 12c0 .94-.2 1.82-.54 2.64l1.51 1.51A8.8 8.8 0 0 0 21 12c0-4.28-2.99-7.86-7-8.77v2.06c2.89.86 5 3.54 5 6.71zM4.27 3L3 4.27 7.73 9H3v6h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06a8.99 8.99 0 0 0 3.69-1.81L19.73 21 21 19.73 12 10.73 4.27 3zM12 4L9.91 6.09 12 8.18V4z"/>
        </svg>
      {:else}
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
          <path d="M3 10v4h4l5 5V5L7 10H3zm13.5 2A4.5 4.5 0 0 0 14 7.97v8.05A4.5 4.5 0 0 0 16.5 12z"/>
        </svg>
      {/if}
    </button>
    <input
      type="range"
      class="h-1 w-28 accent-ink"
      min="0"
      max="1"
      step="0.005"
      value={volToPos(s.volume)}
      oninput={(e) => player.run(player.setVolume(posToVol(Number(e.currentTarget.value))))}
      aria-label={m.player_volume()}
    />
  </div>
</footer>
