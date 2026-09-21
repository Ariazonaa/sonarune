<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import LyricsView from "./LyricsView.svelte";
</script>

<aside class="surface flex w-80 shrink-0 flex-col rounded-panel bg-panel">
  <div class="flex items-center justify-between px-4 py-3">
    <h2 class="text-sm font-bold">{m.lyrics_title()}</h2>
  </div>
  <div class="min-h-0 flex-1">
    <LyricsView variant="panel" />
  </div>
</aside>
