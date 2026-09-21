<!-- Test-only host for ContextMenu.test.ts: a focusable row that opens its
     context menu on right-click and, through `contextMenuKey`, from the
     keyboard. Alpha and Charlie are disabled. -->
<script lang="ts">
  import ContextMenu, { type ContextMenuItem } from "./ContextMenu.svelte";
  import { contextMenuKey } from "$lib/menu";

  let { onpick = () => {} }: { onpick?: (label: string) => void } = $props();

  let menu = $state<{ x: number; y: number } | null>(null);

  const items: ContextMenuItem[] = ["Alpha", "Bravo", "Charlie", "Delta"].map((label) => ({
    label,
    disabled: label === "Alpha" || label === "Charlie",
    action: () => onpick(label),
  }));

  function open(event: MouseEvent) {
    event.preventDefault();
    menu = { x: event.clientX, y: event.clientY };
  }
</script>

<button id="row" oncontextmenu={open} {@attach contextMenuKey}>row</button>
<button id="elsewhere">elsewhere</button>
{#if menu}
  <ContextMenu x={menu.x} y={menu.y} {items} onclose={() => (menu = null)} />
{/if}
