<script lang="ts">
  import { apiLibrary } from "$lib/api/library";
  import { confirm } from "$lib/state/confirm.svelte";
  import { toast } from "$lib/state/toast.svelte";
  import { m } from "$lib/paraglide/messages";
  import { portal } from "$lib/portal";
  import type { EditableMetadata, MetadataEdits } from "$lib/types";

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
        if (id === itemId) loadError = String(e);
      });
  });

  /** Comma-split, trim, drop empties, dedupe case-insensitively (mirrors Rust). */
  function splitList(s: string): string[] {
    const out: string[] = [];
    const seen = new Set<string>();
    for (const raw of s.split(",")) {
      const t = raw.trim();
      const key = t.toLowerCase();
      if (t && !seen.has(key)) {
        seen.add(key);
        out.push(t);
      }
    }
    return out;
  }

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

  const edits = $derived<MetadataEdits>({
    name: name.trim(),
    albumArtists: splitList(albumArtistsText),
    year: yearParsed.value,
    genres: splitList(genresText),
    trackNumber: trackParsed.value,
    discNumber: discParsed.value,
  });

  const diff = $derived.by(() => {
    const o = original;
    if (!o) return [] as { label: string; before: string; after: string }[];
    const rows: { label: string; before: string; after: string }[] = [];
    const show = (v: string) => (v === "" ? m.edit_value_empty() : v);
    const num = (n: number | null) => (n != null ? String(n) : "");
    const push = (label: string, before: string, after: string) => {
      if (before !== after) rows.push({ label, before: show(before), after: show(after) });
    };
    push(m.edit_field_name(), o.name, edits.name);
    push(m.edit_field_album_artist(), o.albumArtists.join(", "), edits.albumArtists.join(", "));
    push(m.edit_field_year(), num(o.year), num(edits.year));
    push(m.edit_field_genres(), o.genres.join(", "), edits.genres.join(", "));
    if (o.isAudio) {
      push(m.edit_field_track_number(), num(o.trackNumber), num(edits.trackNumber));
      push(m.edit_field_disc_number(), num(o.discNumber), num(edits.discNumber));
    }
    return rows;
  });

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
      toast.show(m.edit_saved());
      onsaved?.();
      onclose();
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
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
          <span class="mt-1 block text-xs text-ink-muted">{m.edit_multi_hint()}</span>
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
          <span class="mt-1 block text-xs text-ink-muted">{m.edit_multi_hint()}</span>
        </label>

        <div class="rounded-lg border border-edge bg-base/50 p-3">
          <p class="mb-2 text-xs font-bold uppercase tracking-wider text-ink-muted">{m.edit_diff_title()}</p>
          {#if diff.length === 0}
            <p class="text-sm text-ink-muted">{m.edit_diff_empty()}</p>
          {:else}
            <ul class="space-y-1.5 text-sm">
              {#each diff as row (row.label)}
                <li class="flex flex-wrap items-baseline gap-x-2">
                  <span class="font-medium text-ink-muted">{row.label}:</span>
                  <span class="text-ink-muted line-through">{row.before}</span>
                  <span aria-hidden="true">→</span>
                  <span class="font-semibold">{row.after}</span>
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
