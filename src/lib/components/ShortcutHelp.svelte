<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { help } from "$lib/state/help.svelte";
  import {
    formatShortcutBinding,
    shortcutSettings,
    SHORTCUT_ACTIONS,
  } from "$lib/state/shortcuts.svelte";
  import { shortcutActionLabel } from "$lib/shortcutLabels";

  const rows = $derived(
    SHORTCUT_ACTIONS.map((action) => ({
      action,
      label: shortcutActionLabel(action),
      bindings: shortcutSettings.bindings[action],
    })),
  );

  function onKeydown(event: KeyboardEvent) {
    // defaultPrevented: an overlay on top (a confirmation) already handled it.
    if (event.key === "Escape" && !event.defaultPrevented) {
      event.preventDefault();
      help.close();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div
  class="fixed inset-0 z-40 flex justify-center bg-black/75 p-4 pt-[10vh]"
  onclick={(event) => {
    if (event.target === event.currentTarget) help.close();
  }}
>
  <div
    class="overlay flex max-h-[80vh] w-full max-w-lg flex-col overflow-hidden rounded-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={m.shortcuts_title()}
  >
    <div class="flex items-center justify-between border-b border-edge px-5 py-3.5">
      <h2 class="text-sm font-bold tracking-tight">{m.shortcuts_title()}</h2>
      <button
        class="flex h-7 w-7 items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
        onclick={() => help.close()}
        aria-label={m.dismiss()}
        title={m.dismiss()}
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
          <path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z" />
        </svg>
      </button>
    </div>
    <ul class="min-h-0 overflow-y-auto p-2">
      {#each rows as row (row.action)}
        <li class="flex items-center justify-between gap-4 rounded-md px-3 py-2 text-sm">
          <span class="min-w-0 truncate">{row.label}</span>
          <span class="flex max-w-[55%] shrink-0 flex-wrap justify-end gap-1">
            {#if row.bindings.length === 0}
              <span class="text-xs text-ink-muted">{m.shortcut_unassigned()}</span>
            {:else}
              {#each row.bindings as binding (binding)}
                <kbd
                  class="rounded border border-edge bg-panel-2 px-1.5 py-0.5 text-center text-xs font-semibold tabular-nums"
                >
                  {formatShortcutBinding(binding)}
                </kbd>
              {/each}
            {/if}
          </span>
        </li>
      {/each}
    </ul>
  </div>
</div>
