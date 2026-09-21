<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { confirm } from "$lib/state/confirm.svelte";

  const req = $derived(confirm.request);

  // Only Escape is bound to a key — a destructive confirm must be an explicit
  // click, never a stray Enter.
  function onKeydown(event: KeyboardEvent) {
    if (confirm.request && event.key === "Escape") {
      event.preventDefault();
      confirm.answer(false);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if req}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-4"
    onclick={(e) => {
      if (e.target === e.currentTarget) confirm.answer(false);
    }}
  >
    <div
      class="overlay w-full max-w-sm rounded-overlay p-5"
      role="dialog"
      aria-modal="true"
      aria-label={req.title}
    >
      <h2 class="text-md font-bold tracking-tight">{req.title}</h2>
      <p class="mt-2 text-sm leading-relaxed text-ink-muted">{req.body}</p>
      <div class="mt-5 flex justify-end gap-2">
        <button
          class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
          onclick={() => confirm.answer(false)}
          {@attach (node: HTMLButtonElement) => node.focus()}
        >
          {m.cancel()}
        </button>
        <button
          class="rounded-full px-4 py-1.5 text-sm font-bold transition-colors {req.danger
            ? 'bg-red-600 text-white hover:bg-red-500'
            : 'bg-accent text-(--color-on-accent) hover:bg-accent-hover'}"
          onclick={() => confirm.answer(true)}
        >
          {req.confirmLabel}
        </button>
      </div>
    </div>
  </div>
{/if}
