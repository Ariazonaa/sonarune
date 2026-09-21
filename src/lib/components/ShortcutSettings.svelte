<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { shortcutActionLabel } from "$lib/shortcutLabels";
  import {
    formatShortcutBinding,
    shortcutBindingFromEvent,
    shortcutSettings,
    SHORTCUT_ACTIONS,
    type ShortcutAction,
  } from "$lib/state/shortcuts.svelte";

  let capturing = $state<ShortcutAction | null>(null);
  let feedback = $state<string | null>(null);
  /** The "listening" button. A press anywhere else or focus leaving it ends
   *  the capture — the next key must not be taken from whatever the user
   *  moved on to (another input, a different setting). */
  let captureButton: HTMLElement | null = null;

  function stopCapture() {
    capturing = null;
    captureButton = null;
    feedback = null;
  }

  function capture(event: KeyboardEvent) {
    if (!capturing) return;
    if (event.key === "Tab") {
      stopCapture();
      return;
    }
    event.preventDefault();
    event.stopImmediatePropagation();
    if (event.key === "Escape") {
      stopCapture();
      return;
    }
    const binding = shortcutBindingFromEvent(event);
    if (!binding) return;
    const action = capturing;
    const result = shortcutSettings.addBinding(action, binding);
    if (result.kind === "reserved") {
      feedback = m.shortcut_reserved({
        binding: formatShortcutBinding(result.binding),
      });
      return;
    }
    if (result.kind === "conflict") {
      feedback = m.shortcut_conflict({
        binding: formatShortcutBinding(result.binding),
        action: shortcutActionLabel(result.action),
      });
      return;
    }
    stopCapture();
  }

  function startCapture(action: ShortcutAction, button: HTMLElement) {
    capturing = action;
    captureButton = button;
    feedback = m.shortcut_capture_hint();
    // Focus leaving the button ends the capture, so make sure it has it.
    button.focus();
  }

  function endCaptureOnOutsidePress(event: PointerEvent) {
    if (!capturing) return;
    if (event.target instanceof Node && captureButton?.contains(event.target)) return;
    stopCapture();
  }
</script>

<svelte:window onkeydowncapture={capture} onpointerdowncapture={endCaptureOnOutsidePress} />

<section class="mb-8 rounded-lg bg-card p-5">
  <div class="mb-4 flex items-start justify-between gap-4">
    <div>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_shortcuts()}
      </h2>
      <p class="mt-1 max-w-lg text-xs text-ink-muted">{m.settings_shortcuts_hint()}</p>
    </div>
    <button
      class="shrink-0 rounded-full border border-edge px-3 py-1.5 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
      onclick={() => {
        shortcutSettings.resetAll();
        stopCapture();
      }}
    >
      {m.shortcut_reset_all()}
    </button>
  </div>

  {#if feedback}
    <p class="mb-3 rounded-md border border-edge bg-panel px-3 py-2 text-xs text-ink-muted" role="status">
      {feedback}
    </p>
  {/if}

  <div class="divide-y divide-edge">
    {#each SHORTCUT_ACTIONS as action (action)}
      <div class="flex min-h-12 items-center gap-3 py-2">
        <span class="min-w-32 flex-1 text-sm font-medium">{shortcutActionLabel(action)}</span>
        <div class="flex max-w-[55%] flex-wrap justify-end gap-1">
          {#each shortcutSettings.bindings[action] as binding (binding)}
            <button
              class="group flex items-center gap-1 rounded border border-edge bg-panel px-2 py-1 text-xs font-semibold transition-colors hover:border-red-500 hover:text-red-400"
              onclick={() => shortcutSettings.removeBinding(action, binding)}
              aria-label={m.shortcut_remove_binding({
                binding: formatShortcutBinding(binding),
                action: shortcutActionLabel(action),
              })}
              title={m.shortcut_remove()}
            >
              <kbd>{formatShortcutBinding(binding)}</kbd>
              <span aria-hidden="true" class="text-ink-muted group-hover:text-red-400">×</span>
            </button>
          {/each}
          {#if shortcutSettings.bindings[action].length === 0}
            <span class="self-center text-xs text-ink-muted">{m.shortcut_unassigned()}</span>
          {/if}
        </div>
        <button
          class="rounded-full px-2.5 py-1 text-xs font-semibold transition-colors {capturing === action
            ? 'bg-accent text-(--color-on-accent)'
            : 'bg-panel-2 text-ink-muted hover:text-ink'}"
          onclick={(event) => startCapture(action, event.currentTarget)}
          onfocusout={() => {
            if (capturing === action) stopCapture();
          }}
          aria-pressed={capturing === action}
        >
          {capturing === action ? m.shortcut_listening() : m.shortcut_add()}
        </button>
        <button
          class="rounded-full p-1.5 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
          onclick={() => shortcutSettings.resetAction(action)}
          aria-label={m.shortcut_reset_action({ action: shortcutActionLabel(action) })}
          title={m.shortcut_reset_action({ action: shortcutActionLabel(action) })}
        >
          <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
            <path d="M12 5V2L8 6l4 4V7a5 5 0 1 1-4.9 6H5.02A7 7 0 1 0 12 5z" />
          </svg>
        </button>
      </div>
    {/each}
  </div>
</section>
