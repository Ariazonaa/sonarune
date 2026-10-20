<!-- Test-only host for ContextMenu.test.ts: a focusable row that opens its
     context menu on right-click and, through `contextMenuKey`, from the
     keyboard. Alpha and Charlie are disabled. With `opensDialog`, picking an
     item opens a modal dialog (as "Info" does); the "More" button toggles a
     menu of its own. -->
<script lang="ts">
  import ContextMenu, { type ContextMenuItem } from "./ContextMenu.svelte";
  import { contextMenuKey } from "#lib/menu.js";
  import { modalFocus } from "#lib/modal.js";
  import { portal } from "#lib/portal.js";

  let {
    onpick = () => {},
    opensDialog = false,
  }: { onpick?: (label: string) => void; opensDialog?: boolean } = $props();

  let menu = $state<{ x: number; y: number; trigger?: HTMLElement } | null>(null);
  let dialog = $state(false);

  const items: ContextMenuItem[] = ["Alpha", "Bravo", "Charlie", "Delta"].map((label) => ({
    label,
    disabled: label === "Alpha" || label === "Charlie",
    action: () => {
      onpick(label);
      if (opensDialog) dialog = true;
    },
  }));

  function open(event: MouseEvent) {
    event.preventDefault();
    menu = { x: event.clientX, y: event.clientY };
  }

  function toggleMore(event: MouseEvent) {
    const trigger = event.currentTarget as HTMLElement;
    menu = menu?.trigger === trigger ? null : { x: 10, y: 10, trigger };
  }
</script>

<button id="row" oncontextmenu={open} {@attach contextMenuKey}>row</button>
<button id="elsewhere">elsewhere</button>
<button id="more" onclick={toggleMore}>more</button>
{#if menu}
  <ContextMenu x={menu.x} y={menu.y} {items} trigger={menu.trigger} onclose={() => (menu = null)} />
{/if}
{#if dialog}
  <div class="contents">
    <div {@attach portal} {@attach modalFocus}>
      <div id="dialog" role="dialog" aria-modal="true" tabindex="-1" data-autofocus>
        <button id="dialog-close" onclick={() => (dialog = false)}>close</button>
      </div>
    </div>
  </div>
{/if}
