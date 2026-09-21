<!-- Test-only host for Popover.test.ts: the popover sits in an {#if} next to
     a sibling node of the same block, the case a naive portal breaks. -->
<script lang="ts">
  import Popover from "./Popover.svelte";

  let {
    anchored = false,
    role = "menu",
    onclose = () => {},
  }: { anchored?: boolean; role?: "menu" | "dialog"; onclose?: () => void } = $props();

  let open = $state(true);
  let anchorEl = $state<HTMLElement | null>(null);

  function close() {
    open = false;
    onclose();
  }
</script>

<p id="before">before</p>
<button id="anchor" bind:this={anchorEl}>anchor</button>
{#if open}
  {#if anchored}
    <Popover anchor={anchorEl} placement="above" align="end" {role} onclose={close}>
      <button id="inside">inside</button>
    </Popover>
  {:else}
    <Popover at={{ x: 5000, y: 5000 }} {role} onclose={close}>
      <button id="inside">inside</button>
    </Popover>
  {/if}
  <p id="sibling">sibling in the same block</p>
{/if}
<p id="after">after</p>
