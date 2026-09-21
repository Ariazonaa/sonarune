<script lang="ts" module>
  export interface ContextMenuItem {
    label: string;
    /** SVG path data (24x24 viewBox), optional. */
    icon?: string;
    action: () => void;
    /** Shown dimmed; skipped by the arrow keys and cannot be activated. */
    disabled?: boolean;
    /** Render in red — for destructive actions like deleting from the server. */
    danger?: boolean;
  }
</script>

<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import Popover from "./Popover.svelte";

  let {
    x,
    y,
    items,
    label,
    onclose,
  }: {
    x: number;
    y: number;
    items: ContextMenuItem[];
    /** Accessible name of the menu; a generic "Actions" by default. */
    label?: string;
    onclose: () => void;
  } = $props();

  function pick(item: ContextMenuItem) {
    if (item.disabled) return;
    onclose();
    item.action();
  }
</script>

<!-- The keys (arrows, Home/End, Enter/Space, type-ahead, Escape, Tab) are
     handled by Popover. Disabled items use aria-disabled, not the disabled
     attribute, so screen readers still list them. -->
<Popover at={{ x, y }} {onclose} label={label ?? m.context_menu_label()} class="min-w-44">
  {#each items as item (item.label)}
    <button
      class="flex w-full items-center gap-2.5 rounded-md px-3 py-1.5 text-left text-sm outline-none transition-colors focus-visible:ring-1 focus-visible:ring-accent focus-visible:ring-inset
        {item.disabled
        ? 'cursor-default text-ink-muted/50'
        : item.danger
          ? 'text-red-400 hover:bg-red-500/15 focus-visible:bg-red-500/15'
          : 'hover:bg-ink/10 focus-visible:bg-ink/10'}"
      role="menuitem"
      tabindex="-1"
      aria-disabled={item.disabled ? "true" : undefined}
      onclick={() => pick(item)}
    >
      {#if item.icon}
        <svg
          viewBox="0 0 24 24"
          class="h-4 w-4 shrink-0 {item.danger ? 'text-red-400' : 'text-ink-muted'}"
          fill="currentColor"
          aria-hidden="true"
        >
          <path d={item.icon} />
        </svg>
      {/if}
      <span class="truncate">{item.label}</span>
    </button>
  {/each}
</Popover>
