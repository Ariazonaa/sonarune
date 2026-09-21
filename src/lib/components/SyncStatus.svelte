<script lang="ts">
  import { onMount } from "svelte";
  import { m } from "$lib/paraglide/messages";
  import { library } from "$lib/state/library.svelte";

  // Re-tick so the "synced X ago" label ages without a new sync event.
  let now = $state(Date.now());
  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 20_000);
    return () => clearInterval(t);
  });

  const label = $derived.by(() => {
    if (library.syncing) return m.sync_syncing();
    const at = library.lastSyncedAt;
    if (at === null) return null;
    const mins = Math.floor(Math.max(0, now - at) / 60_000);
    if (mins < 1) return m.sync_now();
    if (mins < 60) return m.sync_minutes({ count: mins });
    return m.sync_hours({ count: Math.floor(mins / 60) });
  });

  // Circular-arrows "sync" glyph.
  const SYNC_ICON =
    "M12 4V1L8 5l4 4V6c3.31 0 6 2.69 6 6 0 1.01-.25 1.97-.7 2.8l1.46 1.46A7.93 7.93 0 0 0 20 12c0-4.42-3.58-8-8-8zm0 14c-3.31 0-6-2.69-6-6 0-1.01.25-1.97.7-2.8L5.24 7.74A7.93 7.93 0 0 0 4 12c0 4.42 3.58 8 8 8v3l4-4-4-4v3z";
</script>

{#if label}
  <button
    class="mt-auto flex items-center gap-2 rounded-md px-3 py-2 text-xs font-medium text-ink-muted transition-colors hover:text-ink"
    onclick={() => library.syncNow()}
    title={m.sync_check_now()}
  >
    <svg
      viewBox="0 0 24 24"
      class="h-4 w-4 shrink-0 {library.syncing ? 'animate-spin' : ''}"
      fill="currentColor"
      aria-hidden="true"
    >
      <path d={SYNC_ICON} />
    </svg>
    <span class="truncate">{label}</span>
  </button>
{/if}
