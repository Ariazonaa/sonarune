<script lang="ts">
  import { apiLibrary } from "#lib/api/library.js";
  import { errorCause } from "#lib/appErrors.js";
  import { sameNames, splitNames } from "#lib/metadataNames.js";
  import { confirm } from "#lib/state/confirm.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import { m } from "#lib/paraglide/messages.js";
  import { modalFocus } from "#lib/modal.js";
  import { portal } from "#lib/portal.js";
  import type { EditableMetadata, MetadataEdits } from "#lib/types.js";

  let {
    itemId,
    displayName,
    onclose,
    onsaved,
  }: {
    itemId: string;
    displayName: string;
    onclose: () => void;
    onsaved?: () => void;
  } = $props();

  let original = $state<EditableMetadata | null>(null);
  let loadError = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  let saving = $state(false);

  // Form fields (all strings; multi-value fields are comma-separated).
  let name = $state("");
  let albumArtistsText = $state("");
  let year = $state("");
  let genresText = $state("");
  let trackNumber = $state("");
  let discNumber = $state("");

  // Load the current server values, stale-guarded on itemId (like SongInfo).
  $effect(() => {
    const id = itemId;
    original = null;
    loadError = null;
    apiLibrary
      .getItemMetadata(id)
      .then((meta) => {
        if (id !== itemId) return;
        original = meta;
        name = meta.name;
        albumArtistsText = meta.albumArtists.join(", ");
        year = meta.year != null ? String(meta.year) : "";
        genresText = meta.genres.join(", ");
        trackNumber = meta.trackNumber != null ? String(meta.trackNumber) : "";
        discNumber = meta.discNumber != null ? String(meta.discNumber) : "";
      })
      .catch((e) => {
        if (id === itemId) loadError = errorCause(e);
      });
  });

  function parseIntField(s: string): { value: number | null; invalid: boolean } {
    const t = s.trim();
    if (t === "") return { value: null, invalid: false };
    if (!/^\d+$/.test(t)) return { value: null, invalid: true };
    return { value: Number(t), invalid: false };
  }

  const yearParsed = $derived(parseIntField(year));
  const trackParsed = $derived(parseIntField(trackNumber));
  const discParsed = $derived(parseIntField(discNumber));
  const yearInvalid = $derived(
    yearParsed.invalid ||
      (yearParsed.value != null && (yearParsed.value < 1 || yearParsed.value > 9999)),
  );
  const nameInvalid = $derived(name.trim() === "");

  // Only what the user changed is sent; everything else stays as the server
  // has it. Writing back what the form was filled with is not a no-op: the
  // list fields went through a comma join and split, which cut "Earth, Wind &
  // Fire" in two on every save, whatever else was edited.
  const edits = $derived.by((): MetadataEdits => {
    const o = original;
    const out: MetadataEdits = {};
    if (!o) return out;
    const title = name.trim();
    if (title !== o.name.trim()) out.name = title;
    // Compared with what the untouched field reads as, so stray spaces or a
    // name the server lists twice do not count as an edit.
    const list = (text: string, names: string[]) => {
      const value = splitNames(text, names);
      return sameNames(value, splitNames(names.join(", "), names)) ? undefined : value;
    };
    const albumArtists = list(albumArtistsText, o.albumArtists);
    if (albumArtists) out.albumArtists = albumArtists;
    if (yearParsed.value !== o.year) out.year = yearParsed.value;
    const genres = list(genresText, o.genres);
    if (genres) out.genres = genres;
    if (o.isAudio) {
      if (trackParsed.value !== o.trackNumber) out.trackNumber = trackParsed.value;
      if (discParsed.value !== o.discNumber) out.discNumber = discParsed.value;
    }
    return out;
  });

  /** A review row. List fields show each value apart, so a name that would be
   *  split into two is seen as two. */
  interface DiffRow {
    label: string;
    before: string[];
    after: string[];
    list: boolean;
  }

  const diff = $derived.by((): DiffRow[] => {
    const o = original;
    if (!o) return [];
    const rows: DiffRow[] = [];
    const one = (v: string | number | null | undefined) => (v == null || v === "" ? [] : [String(v)]);
    const scalar = (label: string, before: string | number | null, after: string | number | null) =>
      rows.push({ label, before: one(before), after: one(after), list: false });
    const list = (label: string, before: string[], after: string[]) =>
      rows.push({ label, before, after, list: true });
    if (edits.name !== undefined) scalar(m.edit_field_name(), o.name, edits.name);
    if (edits.albumArtists) list(m.edit_field_album_artist(), o.albumArtists, edits.albumArtists);
    if (edits.year !== undefined) scalar(m.edit_field_year(), o.year, edits.year);
    if (edits.genres) list(m.edit_field_genres(), o.genres, edits.genres);
    if (edits.trackNumber !== undefined) {
      scalar(m.edit_field_track_number(), o.trackNumber, edits.trackNumber);
    }
    if (edits.discNumber !== undefined) {
      scalar(m.edit_field_disc_number(), o.discNumber, edits.discNumber);
    }
    return rows;
  });

  /** Whether one of the field's current names has a comma in it: its hint then
   *  says such a name stays whole. */
  const keepsCommaNames = (names: readonly string[]) => names.some((value) => value.includes(","));

  const canSave = $derived(
    original != null &&
      diff.length > 0 &&
      !nameInvalid &&
      !yearInvalid &&
      !trackParsed.invalid &&
      !discParsed.invalid &&
      !saving,
  );

  async function save() {
    if (!canSave) return;
    const ok = await confirm.ask({
      title: m.edit_confirm_title(),
      body: (diff.length === 1 ? m.edit_confirm_body_one : m.edit_confirm_body_many)({
        count: diff.length,
      }),
      confirmLabel: m.edit_save(),
    });
    if (!ok) return;
    saving = true;
    saveError = null;
    try {
      await apiLibrary.updateItemMetadata(itemId, edits);
    } catch (e) {
      saveError = errorCause(e);
      return;
    } finally {
      saving = false;
    }
    // Past this line the write is done. What follows is housekeeping, and it
    // must not be able to present a finished save as a failed one.
    toast.show(m.edit_saved());
    tellCaller();
    onclose();
  }

  /**
   * Let the list know it is stale, without letting it decide this dialog's
   * outcome. Reloading a list is the caller's business and can throw for its
   * own reasons; the write already happened, so the dialog closes either way.
   */
  function tellCaller() {
    try {
      onsaved?.();
    } catch (e) {
      console.error("refreshing the list after a metadata save failed", e);
    }
  }

  function onKeydown(e: KeyboardEvent) {
    // The save confirmation sits on top of this dialog and listens on the same
    // window, so both handlers would fire: Escape would dismiss the
    // confirmation AND close the editor, throwing away everything the user
    // typed. The topmost overlay owns the key. The confirmation is mounted
    // first, so it usually answered (and cleared its request) before this
    // runs — it marks the key handled with preventDefault.
    if (e.defaultPrevented || confirm.request) return;
    if (e.key === "Escape") {
      e.preventDefault();
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
  {@attach modalFocus}
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-4"
  onclick={(e) => {
    if (e.target === e.currentTarget) onclose();
  }}
>
  <div
    class="overlay max-h-[90vh] w-full max-w-lg overflow-y-auto rounded-overlay p-5"
    role="dialog"
    aria-modal="true"
    aria-label={m.edit_title()}
  >
    <div class="mb-4 flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h2 class="text-sm font-bold tracking-tight">{m.edit_title()}</h2>
        <p class="truncate text-xs text-ink-muted">{displayName}</p>
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

    {#if loadError}
      <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
        {m.error_generic({ message: loadError })}
      </p>
    {:else if !original}
      <div class="space-y-3">
        {#each Array(5) as _, i (i)}
          <div class="skeleton h-9 w-full rounded-md"></div>
        {/each}
      </div>
    {:else}
      <div class="space-y-3">
        <label class="block">
          <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_name()}</span>
          <input
            type="text"
            bind:value={name}
            class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
            class:border-red-800={nameInvalid}
          />
          {#if nameInvalid}
            <span class="mt-1 block text-xs text-red-400">{m.edit_name_required()}</span>
          {/if}
        </label>

        <label class="block">
          <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_album_artist()}</span>
          <input
            type="text"
            bind:value={albumArtistsText}
            class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <span class="mt-1 block text-xs text-ink-muted">
            {keepsCommaNames(original.albumArtists) ? m.edit_multi_hint_keep() : m.edit_multi_hint()}
          </span>
        </label>

        <div class="grid grid-cols-2 gap-3">
          <label class="block">
            <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_year()}</span>
            <input
              type="text"
              inputmode="numeric"
              bind:value={year}
              class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
              class:border-red-800={yearInvalid}
            />
            {#if yearInvalid}
              <span class="mt-1 block text-xs text-red-400">{m.edit_year_invalid()}</span>
            {/if}
          </label>
          {#if original.isAudio}
            <label class="block">
              <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_track_number()}</span>
              <input
                type="text"
                inputmode="numeric"
                bind:value={trackNumber}
                class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
                class:border-red-800={trackParsed.invalid}
              />
            </label>
            <label class="block">
              <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_disc_number()}</span>
              <input
                type="text"
                inputmode="numeric"
                bind:value={discNumber}
                class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
                class:border-red-800={discParsed.invalid}
              />
            </label>
          {/if}
        </div>

        <label class="block">
          <span class="mb-1 block text-xs font-medium text-ink-muted">{m.edit_field_genres()}</span>
          <input
            type="text"
            bind:value={genresText}
            class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
          />
          <span class="mt-1 block text-xs text-ink-muted">
            {keepsCommaNames(original.genres) ? m.edit_multi_hint_keep() : m.edit_multi_hint()}
          </span>
        </label>

        {#snippet values(list: string[], multiple: boolean, before: boolean)}
          {#if list.length === 0}
            <span class="text-ink-muted {before ? 'line-through' : ''}">{m.edit_value_empty()}</span>
          {:else if multiple}
            <span class="inline-flex flex-wrap gap-1">
              {#each list as value, index (index)}
                <span
                  class="rounded bg-panel-2 px-1.5 {before ? 'text-ink-muted line-through' : 'font-semibold'}"
                >{value}</span>
              {/each}
            </span>
          {:else}
            <span class={before ? "text-ink-muted line-through" : "font-semibold"}>{list[0]}</span>
          {/if}
        {/snippet}

        <div class="rounded-lg border border-edge bg-base/50 p-3">
          <p class="mb-2 text-xs font-bold uppercase tracking-wider text-ink-muted">{m.edit_diff_title()}</p>
          {#if diff.length === 0}
            <p class="text-sm text-ink-muted">{m.edit_diff_empty()}</p>
          {:else}
            <ul class="space-y-1.5 text-sm">
              {#each diff as row (row.label)}
                <li class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                  <span class="font-medium text-ink-muted">{row.label}:</span>
                  {@render values(row.before, row.list, true)}
                  <span aria-hidden="true">→</span>
                  {@render values(row.after, row.list, false)}
                </li>
              {/each}
            </ul>
          {/if}
        </div>

        {#if saveError}
          <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
            {m.error_generic({ message: saveError })}
          </p>
        {/if}

        <div class="flex justify-end gap-2 pt-1">
          <button
            class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink"
            onclick={onclose}
          >
            {m.cancel()}
          </button>
          <button
            class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-50"
            onclick={save}
            disabled={!canSave}
          >
            {saving ? m.edit_saving() : m.edit_save()}
          </button>
        </div>
      </div>
    {/if}
  </div>
</div>
</div>
