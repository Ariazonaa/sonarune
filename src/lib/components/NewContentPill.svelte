<script lang="ts">
  import { m } from "$lib/paraglide/messages";

  // Shown at the top of a scrolled list when the server library changed, so a
  // deep scroll position isn't yanked away — the user refreshes when ready.
  let { onrefresh }: { onrefresh: () => void } = $props();
</script>

<button
  class="fixed left-1/2 top-4 z-40 flex -translate-x-1/2 items-center gap-2 rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) shadow-xl transition-transform hover:scale-105"
  onclick={onrefresh}
>
  <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
    <path d="M17.65 6.35A7.958 7.958 0 0 0 12 4a8 8 0 1 0 7.73 10h-2.08A6 6 0 1 1 12 6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35z" />
  </svg>
  {m.new_music_available()}
</button>
