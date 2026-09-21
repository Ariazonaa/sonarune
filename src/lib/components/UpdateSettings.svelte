<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { m } from "$lib/paraglide/messages";
  import { player } from "$lib/state/player.svelte";
  import { updates } from "$lib/state/updates.svelte";
  import type { DesktopSettings } from "$lib/types";

  let {
    desktop = $bindable(),
    onsave,
  }: { desktop: DesktopSettings | null; onsave: () => void } = $props();

  // The running version, so the section says something before a check ran.
  // A check answers with it too, and that answer wins — it comes from the same
  // package metadata.
  // No check is fired here on purpose: switching the automatic check off has
  // to mean the app really does not ask on its own, opening Settings included.
  let version = $state<string | null>(null);
  onMount(() => {
    getVersion()
      .then((v) => (version = v))
      .catch(() => {});
  });

  // The installer closes the app, so it must not run mid-track. Rust refuses
  // it too (updater.rs) — this only keeps the button from lying.
  const busy = $derived(player.state.status === "playing" || player.state.status === "loading");

  const percent = $derived.by(() => {
    const progress = updates.progress;
    if (!progress?.total) return null;
    return Math.min(100, Math.round((progress.downloaded / progress.total) * 100));
  });

  /** Rust answers the refusals with a short code; everything else is a real
   *  failure and keeps its message. */
  const problem = $derived.by(() => {
    const error = updates.error;
    if (!error) return null;
    if (error.includes("update:playing")) return m.settings_update_playing();
    if (error.includes("update:portable")) return m.settings_update_portable();
    if (error.includes("update:none")) return m.settings_update_none();
    return m.error_generic({ message: error });
  });
</script>

<section class="mb-8 rounded-lg bg-card p-5">
  <h2 class="mb-4 text-sm font-semibold uppercase tracking-wider text-ink-muted">
    {m.settings_updates()}
  </h2>

  <div class="flex flex-wrap items-center justify-between gap-3">
    <p class="text-sm font-medium">
      {#if updates.info ?? version}
        {m.settings_update_current({
          version: updates.info?.currentVersion ?? version ?? "",
        })}
      {/if}
    </p>
    <button
      class="shrink-0 rounded-full border border-edge px-4 py-1.5 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
      disabled={updates.checking || updates.installing}
      onclick={() => void updates.check()}
    >
      {updates.checking ? m.settings_update_checking() : m.settings_update_check()}
    </button>
  </div>

  {#if updates.info}
    <div class="mt-4 rounded-md bg-base px-3 py-3">
      {#if updates.info.version}
        <p class="text-sm font-medium text-accent">
          {m.settings_update_available({ version: updates.info.version })}
        </p>
        {#if updates.info.notes}
          <p class="mt-3 text-[11px] uppercase tracking-wider text-ink-muted">
            {m.settings_update_notes()}
          </p>
          <p class="mt-1 max-h-40 overflow-y-auto whitespace-pre-wrap text-xs text-ink-muted">
            {updates.info.notes}
          </p>
        {/if}

        {#if updates.info.installable}
          <button
            class="mt-4 rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-40"
            disabled={busy || updates.installing}
            onclick={() => void updates.install()}
          >
            {updates.installing ? m.settings_update_installing() : m.settings_update_install()}
          </button>
          {#if busy}
            <p class="mt-2 text-xs text-ink-muted">{m.settings_update_playing()}</p>
          {:else}
            <p class="mt-2 max-w-lg text-xs text-ink-muted">{m.settings_update_install_hint()}</p>
          {/if}
          {#if updates.installing}
            <div class="mt-3 h-1.5 w-full overflow-hidden rounded-full bg-panel-2">
              <div
                class="h-full rounded-full bg-accent transition-[width] duration-200"
                style:width="{percent ?? 10}%"
              ></div>
            </div>
          {/if}
        {:else}
          <p class="mt-3 max-w-lg text-xs text-ink-muted">{m.settings_update_portable()}</p>
        {/if}
      {:else}
        <p class="text-sm">{m.settings_update_none()}</p>
      {/if}
    </div>
  {/if}

  {#if problem}
    <p class="mt-3 text-xs text-red-400">{problem}</p>
  {/if}

  {#if desktop}
    <label class="mt-5 flex items-center justify-between gap-4">
      <div>
        <p class="text-sm font-medium">{m.settings_update_auto()}</p>
        <p class="mt-0.5 max-w-md text-xs text-ink-muted">{m.settings_update_auto_hint()}</p>
      </div>
      <input
        type="checkbox"
        class="h-4 w-4 accent-(--color-accent)"
        bind:checked={desktop.autoCheckUpdates}
        onchange={onsave}
      />
    </label>
  {/if}
</section>
