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
  import { m } from "#lib/paraglide/messages.js";
  import Popover from "./Popover.svelte";

  let {
    x,
    y,
    items,
    label,
    trigger = null,
    onclose,
  }: {
    x: number;
    y: number;
    items: ContextMenuItem[];
    /** Accessible name of the menu; a generic "Actions" by default. */
    label?: string;
    /** The menu button that opened it, if any — its click then closes the
     *  menu again (see Popover). */
    trigger?: HTMLElement | null;
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
     attribute, so screen readers still list them.
     Never taller or wider than the window: a track with many credited
     artists gets one "Go to artist" entry each, and a label can carry a
     track name — past the edge, Download and Delete could not be reached.
     A long menu scrolls (focusing an item brings it into view), a long label
     is cut off by its `truncate`.
     Keyed by position: labels are not unique (two credits of one name). -->
<Popover
  at={{ x, y }}
  {trigger}
  {onclose}
  label={label ?? m.context_menu_label()}
  class="max-h-[calc(100vh-8px)] min-w-44 max-w-[min(24rem,calc(100vw-8px))] overflow-y-auto"
>
  {#each items as item, i (i)}
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
