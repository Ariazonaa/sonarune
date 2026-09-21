<script lang="ts">
  import { m } from "$lib/paraglide/messages";
  import { layoutPreferences, type CardSize, type ViewDensity } from "$lib/state/layout.svelte";

  const current = $derived(layoutPreferences.active);
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
</section>
