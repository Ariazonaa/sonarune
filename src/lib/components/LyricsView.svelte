<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import EmptyState from "./EmptyState.svelte";
  import { lyrics as lyricsStore } from "$lib/state/lyrics.svelte";
  import { player } from "$lib/state/player.svelte";
  import type { LyricLineDto } from "$lib/types";

  let { variant }: { variant: "panel" | "fullscreen" } = $props();

  const data = $derived(lyricsStore.lyrics);
  const offsetMs = $derived(data?.offsetMs ?? 0);
  const hasLines = $derived((data?.lines.length ?? 0) > 0);

  let positionMs = $state(0);
  let scroller = $state<HTMLElement | null>(null);

  // Manual per-track sync nudge on top of the server offset. It lives in the
  // lyrics store, so the panel and now playing always show the same value.
  const manualOffsetMs = $derived(lyricsStore.manualOffsetMs);
  const effectiveOffsetMs = $derived(offsetMs + manualOffsetMs);

  // Line-level sync: LRC convention — a positive offset shifts lyrics earlier.
  const activeIndex = $derived.by(() => {
    if (!data?.synced) return -1;
    let index = -1;
    for (let i = 0; i < data.lines.length; i++) {
      const start = data.lines[i].startMs;
      if (start != null && start - effectiveOffsetMs <= positionMs) index = i;
    }
    return index;
  });

  onMount(() => {
    positionMs = player.estimatedPositionMs();
    const tick = setInterval(() => {
      positionMs = player.estimatedPositionMs();
    }, 200);
    return () => clearInterval(tick);
  });

  // Manual scrolling suspends auto-centering for a moment.
  let suspendUntil = 0;
  let lastCentered = -1;
  const suspend = () => (suspendUntil = performance.now() + 3000);

  $effect(() => {
    void positionMs; // also re-check when a suspension expires, not only on line change
    const index = activeIndex;
    const el = scroller;
    if (index < 0 || !el || performance.now() < suspendUntil || index === lastCentered) return;
    lastCentered = index;
    const line = el.querySelector<HTMLElement>(`[data-line="${index}"]`);
    if (!line) return;
    el.scrollTo({
      top: line.offsetTop - el.clientHeight / 2 + line.clientHeight / 2,
      behavior: "smooth",
    });
  });

  // New track: back to the top, forget any suspension (the store loads its
  // saved nudge).
  $effect(() => {
    void lyricsStore.forTrackId;
    suspendUntil = 0;
    lastCentered = -1;
    scroller?.scrollTo({ top: 0 });
  });

  function seekTo(line: LyricLineDto) {
    if (line.startMs == null) return;
    player.run(api.playerSeek(Math.max(0, line.startMs - effectiveOffsetMs)));
  }

  // Word-level cues: cue.position is a char offset into the line; a cue's
  // segment runs up to the next cue's position.
  type Segment = { text: string; startMs: number | null };
  function segments(line: LyricLineDto): Segment[] {
    const cues = line.cues
      .filter((c) => c.position != null)
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

  const lineSize = $derived(
    variant === "fullscreen"
      ? "text-2xl font-bold leading-snug tracking-tight"
      : "text-[15px] font-semibold leading-snug",
  );
</script>

{#if data && hasLines}
  <div class="flex h-full flex-col">
  <!-- svelte-ignore a11y_no_static_element_interactions (handlers only detect manual scrolling) -->
  <div
    bind:this={scroller}
    class="relative min-h-0 flex-1 overflow-y-auto {variant === 'fullscreen'
      ? 'px-1 [mask-image:linear-gradient(transparent,black_12%,black_88%,transparent)]'
      : 'px-4 pt-2 pb-24'}"
    onwheel={suspend}
    ontouchmove={suspend}
    onpointerdown={(e) => {
      // Scrollbar drags target the scroller itself; line clicks must not
      // suspend (their seek should still recenter).
      if (e.target === e.currentTarget) suspend();
    }}
  >
    <!-- Room to center the first and last line. Spacers, not padding: padding
         cannot shrink, and 2×38vh of it outgrew the view in low windows,
         pushing the sync row below the edge. -->
    {#if variant === "fullscreen"}<div class="h-[38vh]" aria-hidden="true"></div>{/if}
    {#if data.synced}
      <div class="flex flex-col {variant === 'fullscreen' ? 'gap-5' : 'gap-3'}">
        {#each data.lines as line, i (i)}
          {@const phase = i === activeIndex ? "active" : i < activeIndex ? "past" : "future"}
          <button
            type="button"
            data-line={i}
            disabled={line.startMs == null}
            class="block w-full whitespace-pre-wrap text-left transition-colors duration-300 {lineSize}
              {phase === 'active'
              ? 'text-ink'
              : phase === 'past'
                ? 'text-ink-muted/50'
                : 'text-ink-muted/80'}
              {line.startMs != null ? 'cursor-pointer hover:text-ink' : 'cursor-default'}"
            onclick={() => seekTo(line)}
          >
            {#if phase === "active" && line.cues.length > 0}
              {#each segments(line) as segment}
                <span
                  class={segment.startMs == null || segment.startMs - effectiveOffsetMs <= positionMs
                    ? "text-ink"
                    : "text-ink-muted/60"}>{segment.text}</span
                >
              {/each}
            {:else}
              {line.text || " "}
            {/if}
          </button>
        {/each}
      </div>
    {:else}
      <div class="flex flex-col {variant === 'fullscreen' ? 'gap-4' : 'gap-2.5'}">
        {#each data.lines as line, i (i)}
          <p class="whitespace-pre-wrap {lineSize} text-ink-muted">{line.text || " "}</p>
        {/each}
      </div>
    {/if}
    {#if variant === "fullscreen"}<div class="h-[38vh]" aria-hidden="true"></div>{/if}
  </div>
    {#if data.synced}
      <div class="flex shrink-0 items-center justify-center gap-1.5 border-t border-edge/50 px-3 py-1.5 text-xs text-ink-muted">
        <button
          class="rounded px-1.5 py-0.5 font-bold transition-colors hover:bg-ink/10 hover:text-ink"
          onclick={() => lyricsStore.nudgeOffset(-250)}
          aria-label={m.lyrics_sync_later()}
          title={m.lyrics_sync_later()}
        >
          −
        </button>
        <span class="min-w-14 text-center tabular-nums">
          {manualOffsetMs === 0
            ? "0.00s"
            : `${manualOffsetMs > 0 ? "+" : ""}${(manualOffsetMs / 1000).toFixed(2)}s`}
        </span>
        <button
          class="rounded px-1.5 py-0.5 font-bold transition-colors hover:bg-ink/10 hover:text-ink"
          onclick={() => lyricsStore.nudgeOffset(250)}
          aria-label={m.lyrics_sync_earlier()}
          title={m.lyrics_sync_earlier()}
        >
          +
        </button>
        {#if manualOffsetMs !== 0}
          <button
            class="ml-1 rounded px-1.5 py-0.5 transition-colors hover:bg-ink/10 hover:text-ink"
            onclick={() => lyricsStore.resetOffset()}
            title={m.lyrics_sync_reset()}
          >
            {m.lyrics_sync_reset()}
          </button>
        {/if}
      </div>
    {/if}
  </div>
{:else if !lyricsStore.loading}
  <div class="flex h-full items-center justify-center">
    <EmptyState icon="lyrics" title={m.lyrics_none()} hint={m.lyrics_none_hint()} compact />
  </div>
{/if}
