<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { layoutPreferences, type CardSize, type ViewDensity } from "$lib/state/layout.svelte";
  import { homeRows, type HomeRowId } from "$lib/state/homeRows.svelte";

  const current = $derived(layoutPreferences.active);

  const ROW_TITLE: Record<HomeRowId, () => string> = {
    recentlyPlayed: m.home_recently_played,
    recentlyAdded: m.home_recently_added,
    mostPlayed: m.home_most_played,
    forgottenFavorites: m.home_forgotten_favorites,
  };
</script>

<section class="mb-8 rounded-lg bg-card p-5">
  <div class="mb-4 flex items-start justify-between gap-4">
    <div>
      <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted">
        {m.settings_layout()}
      </h2>
      <p class="mt-1 max-w-lg text-xs text-ink-muted">{m.settings_layout_hint()}</p>
    </div>
    <button
      class="shrink-0 rounded-full border border-edge px-3 py-1.5 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
      onclick={() => layoutPreferences.reset()}
    >
      {m.settings_layout_reset()}
    </button>
  </div>

  <div>
    <p class="mb-2 text-sm font-medium">{m.settings_layout_presets()}</p>
    <div class="flex flex-wrap gap-2">
      <button
        class="rounded-full px-4 py-1.5 text-sm font-semibold transition-colors {current.density ===
          'compact' && current.cardSize === 'small'
          ? 'bg-accent text-(--color-on-accent)'
          : 'border border-edge text-ink-muted hover:text-ink'}"
        onclick={() => layoutPreferences.applyPreset('compact')}
      >
        {m.settings_layout_compact()}
      </button>
      <button
        class="rounded-full px-4 py-1.5 text-sm font-semibold transition-colors {current.density ===
          'comfortable' && current.cardSize === 'medium'
          ? 'bg-accent text-(--color-on-accent)'
          : 'border border-edge text-ink-muted hover:text-ink'}"
        onclick={() => layoutPreferences.applyPreset('comfortable')}
      >
        {m.settings_layout_comfortable()}
      </button>
    </div>
  </div>

  <label class="mt-5 block">
    <span class="mb-1.5 block text-sm font-medium">{m.settings_layout_density()}</span>
    <select
      class="w-full max-w-xs rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
      value={current.density}
      onchange={(event) =>
        layoutPreferences.update({
          density: (event.currentTarget as HTMLSelectElement).value as ViewDensity,
        })}
    >
      <option value="compact">{m.settings_layout_compact()}</option>
      <option value="comfortable">{m.settings_layout_comfortable()}</option>
    </select>
  </label>

  <label class="mt-5 block">
    <span class="mb-1.5 block text-sm font-medium">{m.settings_layout_card_size()}</span>
    <select
      class="w-full max-w-xs rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
      value={current.cardSize}
      onchange={(event) =>
        layoutPreferences.update({
          cardSize: (event.currentTarget as HTMLSelectElement).value as CardSize,
        })}
    >
      <option value="small">{m.settings_layout_card_small()}</option>
      <option value="medium">{m.settings_layout_card_medium()}</option>
      <option value="large">{m.settings_layout_card_large()}</option>
    </select>
  </label>

  <label class="mt-5 block">
    <span class="mb-1 flex max-w-md items-center justify-between text-sm">
      <span class="font-medium">{m.settings_layout_sidebar()}</span>
      <span class="text-xs text-ink-muted tabular-nums">{current.sidebarWidth}px</span>
    </span>
    <input
      class="h-1 w-full max-w-md accent-(--color-accent)"
      type="range"
      min="200"
      max="320"
      step="8"
      value={current.sidebarWidth}
      oninput={(event) =>
        layoutPreferences.update({
          sidebarWidth: Number((event.currentTarget as HTMLInputElement).value),
        })}
    />
  </label>

  <!-- The home page can hide a row itself; bringing one back has to happen
       somewhere else, because a hidden row has nothing left to click. -->
  <div class="mt-6 border-t border-edge pt-5">
    <div class="flex items-start justify-between gap-4">
      <div>
        <p class="text-sm font-medium">{m.settings_home_rows()}</p>
        <p class="mt-0.5 max-w-lg text-xs text-ink-muted">{m.settings_home_rows_hint()}</p>
      </div>
      <button
        class="shrink-0 rounded-full border border-edge px-3 py-1.5 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
        disabled={homeRows.isDefault}
        onclick={() => homeRows.reset()}
      >
        {m.settings_home_rows_reset()}
      </button>
    </div>

    <ul class="mt-3 max-w-md">
      {#each homeRows.order as id, position (id)}
        <li class="flex items-center gap-2 rounded-md px-1 py-1.5 hover:bg-base">
          <label class="flex min-w-0 flex-1 items-center gap-2">
            <input
              type="checkbox"
              class="h-4 w-4 shrink-0 accent-(--color-accent)"
              checked={!homeRows.isHidden(id)}
              onchange={() => homeRows.toggle(id)}
            />
            <span class="truncate text-sm {homeRows.isHidden(id) ? 'text-ink-muted' : ''}">
              {ROW_TITLE[id]()}
            </span>
          </label>
          <button
            class="rounded p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-30 disabled:hover:bg-transparent"
            disabled={position === 0}
            onclick={() => homeRows.move(id, -1)}
            aria-label={m.home_row_move_up()}
            title={m.home_row_move_up()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M12 8l6 6H6l6-6z" />
            </svg>
          </button>
          <button
            class="rounded p-1 text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-30 disabled:hover:bg-transparent"
            disabled={position === homeRows.order.length - 1}
            onclick={() => homeRows.move(id, 1)}
            aria-label={m.home_row_move_down()}
            title={m.home_row_move_down()}
          >
            <svg viewBox="0 0 24 24" class="h-4 w-4" fill="currentColor" aria-hidden="true">
              <path d="M12 16l-6-6h12l-6 6z" />
            </svg>
          </button>
        </li>
      {/each}
    </ul>
  </div>
</section>
