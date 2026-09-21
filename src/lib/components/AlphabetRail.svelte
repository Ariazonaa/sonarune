<script lang="ts">
  /** Slim vertical A–Z scrubber rail for the virtualized grids. */
  const LETTERS = ["#", ..."ABCDEFGHIJKLMNOPQRSTUVWXYZ"];

  let { onselect }: { onselect: (letter: string) => void } = $props();
</script>

<div class="flex h-full w-5 shrink-0 flex-col items-stretch justify-center select-none">
  {#each LETTERS as letter (letter)}
    <button
      class="min-h-0 flex-1 text-center text-[10px] leading-none font-semibold text-ink-muted transition-colors hover:text-accent"
      onclick={() => onselect(letter)}
      aria-label={letter}
    >
      {letter}
    </button>
  {/each}
</div>
