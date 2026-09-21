<script lang="ts">
  import { apiLibrary } from "$lib/api/library";
  import { m } from "$lib/paraglide/messages";
  import { portal } from "$lib/portal";
  import { toast } from "$lib/state/toast.svelte";
  import type { BulkMetadataEdits } from "$lib/types";

  let {
    itemIds,
    onclose,
    onsaved,
  }: {
    itemIds: string[];
    onclose: () => void;
    /** Ran after a write that changed something, so the list can refetch. */
    onsaved?: () => void;
  } = $props();

  // Empty means untouched, so nothing is loaded and nothing is prefilled: with
  // several tracks there is no single current value to show, and a form that
  // filled itself with one track's values would quietly spread them over the
  // rest on save.
  let albumArtistsText = $state("");
  let genresText = $state("");
  let addGenres = $state(true);
  let year = $state("");
  let clearYear = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  /** Comma-split, trim, drop empties, dedupe case-insensitively (as in Rust). */
  function splitList(text: string): string[] {
    const out: string[] = [];
    const seen = new Set<string>();
    for (const raw of text.split(",")) {
      const value = raw.trim();
      const key = value.toLowerCase();
      if (value && !seen.has(key)) {
        seen.add(key);
        out.push(value);
      }
    }
    return out;
  }

  const yearParsed = $derived.by(() => {
    const text = year.trim();
    if (!text) return { value: undefined, invalid: false };
    const n = Number(text);
    const ok = /^\d+$/.test(text) && n >= 1 && n <= 9999;
    return { value: ok ? n : undefined, invalid: !ok };
  });

  const edits = $derived.by((): BulkMetadataEdits => {
    const out: BulkMetadataEdits = {};
    // A field is only sent when it says something. Clearing is explicit: the
    // checkbox next to the year, never an empty box.
    if (albumArtistsText.trim()) out.albumArtists = splitList(albumArtistsText);
    if (genresText.trim()) {
      out.genres = splitList(genresText);
      out.addGenres = addGenres;
    }
    if (clearYear) out.clearYear = true;
    else if (yearParsed.value !== undefined) out.year = yearParsed.value;
    return out;
  });

  const changes = $derived(Object.keys(edits).filter((key) => key !== "addGenres").length);
  const canSave = $derived(changes > 0 && !yearParsed.invalid && !saving && itemIds.length > 0);

  async function save() {
    if (!canSave) return;
    saving = true;
    saveError = null;
    try {
      const result = await apiLibrary.updateItemsMetadata(itemIds, edits);
      if (result.changed > 0) {
        toast.show(m.edit_bulk_done({ count: result.changed }));
        onsaved?.();
      }
      if (result.failed > 0) {
        // Partly through: say how many, and keep the dialog open with the
        // reason rather than closing on a half-done write.
        saveError = m.edit_bulk_partial({
          count: result.failed,
          message: result.error ?? "",
        });
        return;
      }
      onclose();
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && !saving && !event.defaultPrevented) {
      event.preventDefault();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- The wrapper stays in place; the overlay moves to <body> (see portal.ts). -->
<div class="contents">
<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div
  {@attach portal}
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-4"
  onclick={(event) => {
    if (event.target === event.currentTarget && !saving) onclose();
  }}
>
  <div
    class="overlay max-h-[90vh] w-full max-w-lg overflow-y-auto rounded-overlay p-5"
    role="dialog"
    aria-modal="true"
    aria-label={m.edit_bulk_title()}
  >
    <div class="mb-4 flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h2 class="text-sm font-bold tracking-tight">{m.edit_bulk_title()}</h2>
        <p class="truncate text-xs text-ink-muted">
          {m.edit_bulk_subtitle({ count: itemIds.length })}
        </p>
      </div>
      <button
        class="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
        onclick={onclose}
        aria-label={m.dismiss()}
        title={m.dismiss()}
      >
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
          <path d="M18.3 5.71 12 12l6.3 6.29-1.41 1.42L10.59 13.4 4.3 19.71 2.88 18.3 9.17 12 2.88 5.71 4.3 4.29l6.29 6.3 6.3-6.3z" />
        </svg>
      </button>
    </div>

    <div class="space-y-3">
      <p class="rounded-md bg-base px-3 py-2 text-xs text-ink-muted">{m.edit_bulk_hint()}</p>

      <label class="block">
        <span class="mb-1 block text-xs font-medium text-ink-muted">
          {m.edit_field_album_artist()}
        </span>
        <input
          type="text"
          bind:value={albumArtistsText}
          class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
        />
        <span class="mt-1 block text-xs text-ink-muted">{m.edit_multi_hint()}</span>
      </label>

      <label class="block">
        <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_genres()}</span>
        <input
          type="text"
          bind:value={genresText}
          class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
        />
        <span class="mt-1 block text-xs text-ink-muted">{m.edit_multi_hint()}</span>
      </label>

      <label class="flex items-center gap-2 pl-0.5">
        <input
          type="checkbox"
          class="h-4 w-4 accent-(--color-accent)"
          bind:checked={addGenres}
          disabled={!genresText.trim()}
        />
        <span class="text-xs {genresText.trim() ? 'text-ink' : 'text-ink-muted'}">
          {m.edit_bulk_add_genres()}
        </span>
      </label>

      <div class="flex items-end gap-3">
        <label class="block flex-1">
          <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_year()}</span>
          <input
            type="text"
            inputmode="numeric"
            bind:value={year}
            disabled={clearYear}
            class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent disabled:opacity-40"
            class:border-red-800={yearParsed.invalid}
          />
          {#if yearParsed.invalid}
            <span class="mt-1 block text-xs text-red-400">{m.edit_year_invalid()}</span>
          {/if}
        </label>
        <label class="flex items-center gap-2 pb-2.5">
          <input type="checkbox" class="h-4 w-4 accent-(--color-accent)" bind:checked={clearYear} />
          <span class="text-xs">{m.edit_bulk_clear_year()}</span>
        </label>
      </div>

      {#if saveError}
        <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
          {saveError}
        </p>
      {/if}

      <div class="flex justify-end gap-2 pt-1">
        <button
          class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
          onclick={onclose}
          disabled={saving}
        >
          {m.cancel()}
        </button>
        <button
          class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-50"
          onclick={save}
          disabled={!canSave}
        >
          {saving ? m.edit_saving() : m.edit_bulk_save({ count: itemIds.length })}
        </button>
      </div>
    </div>
  </div>
</div>
</div>
