<script lang="ts">
  import { m } from "#lib/paraglide/messages.js";
  import { modalFocus } from "#lib/modal.js";
  import { portal } from "#lib/portal.js";
  import { confirm } from "#lib/state/confirm.svelte.js";

  const uid = $props.id();
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

<!-- The wrapper stays put; the overlay moves to <body> (see portal.ts). This
     dialog is asked for *by* other dialogs — the metadata editor asks before it
     writes — and those are portaled to <body> themselves. Declared in the
     layout it stayed inside `.app-root`, which is painted before anything
     appended to <body>, so at the same `z-50` it ended up *behind* the very
     dialog that had asked: the editor's 75 % black backdrop covered it, the
     click went to that backdrop, and pressing Save looked like it did nothing
     at all. Hence the portal, and a z above the dialogs that ask.
     Focus starts on Cancel, never on the destructive button, stays inside
     while the request is open and goes back to where it was after — see
     `modalFocus`, declared after `portal` so it runs after the move. -->
{#if req}
  <div class="contents">
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div
      {@attach portal}
      {@attach modalFocus}
      class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 p-4"
      onclick={(e) => {
        if (e.target === e.currentTarget) confirm.answer(false);
      }}
    >
      <div
        class="overlay w-full max-w-sm rounded-overlay p-5"
        role="dialog"
        aria-modal="true"
        aria-label={req.title}
        aria-describedby="{uid}-body"
      >
        <h2 class="text-md font-bold tracking-tight">{req.title}</h2>
        <p id="{uid}-body" class="mt-2 text-sm leading-relaxed text-ink-muted">{req.body}</p>
        <div class="mt-5 flex justify-end gap-2">
          <button
            class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
            data-autofocus
            onclick={() => confirm.answer(false)}
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
  </div>
{/if}
