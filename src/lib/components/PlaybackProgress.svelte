<script lang="ts">
  import { formatDuration } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import { player } from "$lib/state/player.svelte";
  import SeekBar from "./SeekBar.svelte";

  interface Props {
    size?: "sm" | "lg";
    class?: string;
  }

  let { size = "sm", class: className = "" }: Props = $props();

  const s = $derived(player.state);
  const peaks = $derived(s.current ? (player.waveforms[s.current.itemId] ?? null) : null);

  // While the thumb is dragged, show the drag position, not the (still
  // ticking) player position. After a seek, keep showing its target until a
  // state update lands near it — otherwise the thumb snaps back to the
  // pre-seek position for one tick.
  let scrubbing = $state<number | null>(null);
  let pending = $state<number | null>(null);
  let seekToken = 0;
  const shownPosition = $derived(scrubbing ?? pending ?? s.positionMs);
  // The queue entry, not just the item: the same track queued twice in a row
  // is still a track change for a drag in progress.
  const trackKey = $derived(s.current ? `${s.current.itemId}:${s.current.playSessionId}` : null);

  $effect(() => {
    if (pending !== null && Math.abs(s.positionMs - pending) < 2000) pending = null;
  });

  function seek(positionMs: number) {
    pending = positionMs;
    player.run(player.seek(positionMs));
    // Give up waiting after 2 s — but only for this seek, not a newer one.
    const token = ++seekToken;
    setTimeout(() => {
      if (token === seekToken) pending = null;
    }, 2000);
  }
</script>

<div
  class="flex w-full items-center gap-2 text-ink-muted tabular-nums
    {size === 'lg' ? 'text-xs' : 'text-[11px]'} {className}"
>
  <span class="min-w-10 shrink-0 text-right">{formatDuration(shownPosition)}</span>
  <SeekBar
    positionMs={shownPosition}
    durationMs={s.durationMs}
    bufferedMs={s.bufferedMs}
    {peaks}
    {trackKey}
    disabled={!s.current || s.status === "loading"}
    label={m.player_seek()}
    {size}
    onscrub={(positionMs) => (scrubbing = positionMs)}
    onseek={seek}
  />
  <span class="min-w-10 shrink-0">{formatDuration(s.durationMs)}</span>
</div>
