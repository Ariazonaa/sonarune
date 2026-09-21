<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { player } from "$lib/state/player.svelte";

  interface Props {
    /** "lg" is the now-playing view's larger transport. */
    size?: "sm" | "lg";
  }

  let { size = "sm" }: Props = $props();

  const s = $derived(player.state);
  const playing = $derived(s.status === "playing" || s.status === "loading");
  const lg = $derived(size === "lg");
  const icon = $derived(lg ? "h-6 w-6" : "h-5 w-5");
  const smallIcon = $derived(lg ? "h-5 w-5" : "h-4.5 w-4.5");

  const shuffleLabel = $derived(
    s.shuffleMode === "albums"
      ? m.player_shuffle_albums()
      : s.shuffleMode === "tracks"
        ? m.player_shuffle_tracks()
        : m.player_shuffle_off(),
  );
  const repeatTitle = $derived(
    s.repeat === "one"
      ? m.player_repeat_one()
      : s.repeat === "all"
        ? m.player_repeat_all()
        : m.player_repeat_off(),
  );
</script>

<div class="flex items-center {lg ? 'gap-5' : 'gap-2'}">
  <button
    class="rounded-full p-2 transition-colors disabled:opacity-40 {s.shuffle
      ? 'text-accent hover:text-accent-hover'
      : 'text-ink-muted hover:text-ink'}"
    onclick={() => player.run(player.toggleShuffle())}
    disabled={s.queueLen === 0}
    aria-label={shuffleLabel}
    title={shuffleLabel}
  >
    <svg viewBox="0 0 24 24" class={smallIcon} fill="currentColor" aria-hidden="true"><path d="M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z"/></svg>
    {#if s.shuffleMode === "albums"}
      <span class="-ml-1 rounded bg-accent px-1 text-[8px] font-black leading-3 text-(--color-on-accent)" aria-hidden="true">A</span>
    {/if}
  </button>
  <button
    class="rounded-full p-2 text-ink-muted transition-colors hover:text-ink disabled:opacity-40"
    onclick={() => player.run(player.prev())}
    disabled={!s.current}
    aria-label={m.player_previous()}
    title={m.player_previous()}
  >
    <svg viewBox="0 0 24 24" class={icon} fill="currentColor" aria-hidden="true"><path d="M6 6h2v12H6V6zm3.5 6l8.5 6V6l-8.5 6z"/></svg>
  </button>
  <button
    class="flex items-center justify-center rounded-full bg-ink text-(--color-base) transition-transform hover:scale-105 disabled:opacity-40
      {lg ? 'h-14 w-14' : 'h-9 w-9'}"
    onclick={() => player.run(player.toggle())}
    disabled={!s.current && s.queueLen === 0}
    aria-label={playing ? m.player_pause() : m.player_play()}
  >
    {#if playing}
      <svg viewBox="0 0 24 24" class={lg ? "h-7 w-7" : "h-4.5 w-4.5"} fill="currentColor" aria-hidden="true"><path d="M6 5h4v14H6V5zm8 0h4v14h-4V5z"/></svg>
    {:else}
      <svg viewBox="0 0 24 24" class={lg ? "h-7 w-7" : "h-4.5 w-4.5"} fill="currentColor" aria-hidden="true"><path d="M8 5v14l11-7L8 5z"/></svg>
    {/if}
  </button>
  <button
    class="rounded-full p-2 text-ink-muted transition-colors hover:text-ink disabled:opacity-40"
    onclick={() => player.run(player.next())}
    disabled={!s.current || (s.index + 1 >= s.queueLen && s.repeat === "off" && !s.shuffle)}
    aria-label={m.player_next()}
    title={m.player_next()}
  >
    <svg viewBox="0 0 24 24" class={icon} fill="currentColor" aria-hidden="true"><path d="M16 6h2v12h-2V6zM6 18l8.5-6L6 6v12z"/></svg>
  </button>
  <button
    class="relative rounded-full p-2 transition-colors disabled:opacity-40 {s.repeat !== 'off'
      ? 'text-accent hover:text-accent-hover'
      : 'text-ink-muted hover:text-ink'}"
    onclick={() => player.run(player.cycleRepeat())}
    disabled={s.queueLen === 0}
    aria-label={m.player_repeat()}
    title={repeatTitle}
  >
    <svg viewBox="0 0 24 24" class={smallIcon} fill="currentColor" aria-hidden="true"><path d="M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z"/></svg>
    {#if s.repeat === "one"}
      <span class="absolute right-1 bottom-1 text-[9px] font-bold leading-none">1</span>
    {/if}
  </button>
</div>
