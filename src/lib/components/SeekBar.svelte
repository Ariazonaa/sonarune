<script lang="ts">
  import { untrack } from "svelte";
  import { formatDuration } from "$lib/api";
  import { percentOf, positionAtPointer, positionForKey, waveformBars } from "$lib/seek";

  interface Props {
    positionMs: number;
    durationMs: number;
    /** Downloaded range `[start, end]` in ms; drawn behind the progress. */
    bufferedMs?: [number, number] | null;
    /** Peaks (0–255) of the whole track; drawn as a waveform instead of the
     *  line once the track is fully downloaded and analysed. */
    peaks?: number[] | null;
    /** Identity of the track the position belongs to; a change cancels a
     *  drag in progress. */
    trackKey?: string | null;
    disabled?: boolean;
    label: string;
    /** "lg" is the thicker bar of the now-playing view. */
    size?: "sm" | "lg";
    /** Live position while the thumb is dragged; null once the drag ends. */
    onscrub?: (positionMs: number | null) => void;
    onseek: (positionMs: number) => void;
  }

  let {
    positionMs,
    durationMs,
    bufferedMs = null,
    peaks = null,
    trackKey = null,
    disabled = false,
    label,
    size = "sm",
    onscrub,
    onseek,
  }: Props = $props();

  let rail = $state<HTMLDivElement | null>(null);
  let scrubMs = $state<number | null>(null);
  let hover = $state<{ ms: number; x: number } | null>(null);

  const inactive = $derived(disabled || durationMs <= 0);
  const shownMs = $derived(scrubMs ?? positionMs);
  const progress = $derived(percentOf(shownMs, durationMs));
  const buffered = $derived.by(() => {
    if (!bufferedMs || inactive) return null;
    const left = percentOf(bufferedMs[0], durationMs);
    const width = percentOf(bufferedMs[1], durationMs) - left;
    return width > 0 ? { left, width } : null;
  });
  // A waveform only exists for a fully downloaded track, so the buffered
  // range has nothing left to show once it is drawn.
  const bars = $derived(
    peaks && peaks.length > 0 && !inactive ? waveformBars(peaks, size === "lg" ? 160 : 110) : null,
  );

  function pointerAt(clientX: number) {
    const rect = rail!.getBoundingClientRect();
    return {
      ms: positionAtPointer(clientX, rect.left, rect.width, durationMs),
      x: Math.min(Math.max(clientX - rect.left, 0), rect.width),
    };
  }

  function onpointerdown(event: PointerEvent) {
    if (inactive || !rail || event.button !== 0) return;
    // Capture keeps the drag alive when the pointer leaves the thin bar.
    rail.setPointerCapture(event.pointerId);
    scrubMs = pointerAt(event.clientX).ms;
    onscrub?.(scrubMs);
  }

  function onpointermove(event: PointerEvent) {
    if (inactive || !rail) return;
    const at = pointerAt(event.clientX);
    hover = at;
    if (scrubMs !== null) {
      scrubMs = at.ms;
      onscrub?.(at.ms);
    }
  }

  function onpointerup() {
    if (scrubMs === null) return;
    const target = scrubMs;
    scrubMs = null;
    onscrub?.(null);
    onseek(target);
  }

  // Drop the drag instead of seeking to wherever it happened to be: the
  // system took the pointer away (e.g. a window switch mid-drag), or the
  // track changed under the thumb.
  function cancelScrub() {
    if (scrubMs === null) return;
    scrubMs = null;
    onscrub?.(null);
  }

  // A gapless transition while the thumb is held: releasing would otherwise
  // seek the new track to a position picked on the old one.
  $effect(() => {
    void trackKey;
    untrack(cancelScrub);
  });

  function onkeydown(event: KeyboardEvent) {
    if (inactive) return;
    const target = positionForKey(event.key, positionMs, durationMs);
    if (target === null) return;
    // The global ←/→ shortcuts would otherwise seek a second time.
    event.preventDefault();
    onseek(target);
  }
</script>

<!-- Two copies of the waveform: a dim base, and the played part on top,
     clipped to the progress — a position change only moves the clip. -->
{#snippet wave(values: number[], tone: string, clipRight: number)}
  <svg
    class="absolute inset-0 h-full w-full {tone}"
    style:clip-path="inset(0 {clipRight}% 0 0)"
    viewBox="0 0 {values.length} 100"
    preserveAspectRatio="none"
    aria-hidden="true"
  >
    {#each values as value, i (i)}
      <rect x={i + 0.15} width="0.7" y={50 - value * 50} height={value * 100} fill="currentColor" />
    {/each}
  </svg>
{/snippet}

<div
  bind:this={rail}
  class="group relative flex flex-1 touch-none items-center outline-none select-none
    {size === 'lg' ? 'h-10' : 'h-6'}
    {inactive ? 'opacity-40' : 'cursor-pointer'}"
  role="slider"
  tabindex={inactive ? -1 : 0}
  aria-label={label}
  aria-disabled={inactive}
  aria-valuemin={0}
  aria-valuemax={Math.round(durationMs / 1000)}
  aria-valuenow={Math.round(shownMs / 1000)}
  aria-valuetext={`${formatDuration(shownMs)} / ${formatDuration(durationMs)}`}
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  onpointercancel={cancelScrub}
  onpointerleave={() => (hover = null)}
  {onkeydown}
>
  {#if bars}
    <div
      class="relative h-full w-full rounded-sm group-focus-visible:ring-2 group-focus-visible:ring-accent"
    >
      {@render wave(bars, "text-ink/25", 0)}
      {@render wave(
        bars,
        scrubMs !== null ? "text-accent" : "text-ink group-hover:text-accent",
        100 - progress,
      )}
    </div>
  {:else}
    <div
      class="relative w-full overflow-hidden rounded-full bg-ink/15 transition-[height] duration-150
        {size === 'lg' ? 'h-1.5 group-hover:h-2' : 'h-1 group-hover:h-1.5'}
        group-focus-visible:ring-2 group-focus-visible:ring-accent"
    >
      {#if buffered}
        <div
          class="absolute inset-y-0 bg-ink/25"
          style="left: {buffered.left}%; width: {buffered.width}%"
        ></div>
      {/if}
      <div
        class="absolute inset-y-0 left-0 {scrubMs !== null ? 'bg-accent' : 'bg-ink group-hover:bg-accent'}"
        style="width: {progress}%"
      ></div>
    </div>
  {/if}
  {#if !inactive && !bars}
    <div
      class="pointer-events-none absolute top-1/2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-ink shadow transition-opacity
        {size === 'lg' ? 'h-4 w-4' : 'h-3 w-3'}
        {scrubMs !== null ? 'opacity-100' : 'opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100'}"
      style="left: {progress}%"
    ></div>
  {/if}
  {#if hover && !inactive}
    <!-- Opaque: in the player bar it lands on the transport buttons right
         above the rail — over the white play disc a glass pill was unreadable. -->
    <div
      class="overlay pointer-events-none absolute bottom-full mb-2 -translate-x-1/2 rounded-md px-1.5 py-0.5 text-[11px] font-medium whitespace-nowrap text-ink tabular-nums"
      style="left: {hover.x}px"
    >
      {formatDuration(scrubMs ?? hover.ms)}
    </div>
  {/if}
</div>
