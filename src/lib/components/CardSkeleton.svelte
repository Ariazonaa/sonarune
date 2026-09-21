<script lang="ts">
  // A placeholder shaped like an album/artist card, shown while a grid loads.
  let { round = false }: { round?: boolean } = $props();
</script>

<div class="rounded-lg bg-card p-3">
  <div class="skeleton aspect-square w-full {round ? 'rounded-full' : 'rounded-md'}"></div>
  <div class="skeleton mt-3 h-3.5 w-3/4 rounded"></div>
  <div class="skeleton mt-2 h-3 w-1/2 rounded"></div>
</div>
