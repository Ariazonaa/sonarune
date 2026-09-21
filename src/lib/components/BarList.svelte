<script module lang="ts">
  export interface BarItem {
    /** What the row is about. */
    label: string;
    value: number;
    /** Shown at the end of the row; the bar length is the comparison, this is
     *  the number. */
    valueLabel: string;
    /** Makes the row a link — a chart you can walk into. */
    href?: string;
  }
</script>

<script lang="ts">
  let { items, max: given }: { items: BarItem[]; max?: number } = $props();

  // One scale for every row, so the bars compare to each other and not each
  // to itself. Zero-safe: an empty library must not divide by nothing.
  const max = $derived(Math.max(1, given ?? Math.max(...items.map((item) => item.value), 0)));
</script>

<!-- One measure, one colour: the length is the comparison, so there is nothing
     for a legend to explain. Every value is written out as well, which is what
     keeps the chart readable without colour. -->
<ul class="space-y-2">
  {#each items as item (item.label)}
    <li>
      <svelte:element
        this={item.href ? "a" : "div"}
        href={item.href}
        class="block rounded-md px-1 py-1 transition-colors {item.href ? 'hover:bg-panel-2' : ''}"
      >
        <div class="mb-1 flex items-baseline justify-between gap-3">
          <span class="min-w-0 truncate text-sm" title={item.label}>{item.label}</span>
          <span class="shrink-0 text-xs text-ink-muted tabular-nums">{item.valueLabel}</span>
        </div>
        <!-- The track is the full scale; the fill starts at the baseline on
             the left and is rounded only at its data end. -->
        <div class="h-1.5 w-full overflow-hidden rounded-[4px] bg-panel-2">
          <div
            class="h-full rounded-r-[4px] bg-accent"
            style:width="{Math.max(2, Math.round((item.value / max) * 100))}%"
          ></div>
        </div>
      </svelte:element>
    </li>
  {/each}
</ul>
