<script lang="ts">
  import { fly } from "svelte/transition";
  import { m } from "$lib/paraglide/messages";
  import { toast, type ToastKind } from "$lib/state/toast.svelte";

  // 24x24 paths: check, info circle, warning triangle.
  const ICONS: Record<ToastKind, string> = {
    success: "M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z",
    info: "M11 7h2v2h-2zm0 4h2v6h-2zm1-9C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8z",
    error: "M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z",
  };
  const TONE: Record<ToastKind, string> = {
    success: "text-accent",
    info: "text-ink-muted",
    error: "text-red-400",
  };
</script>

<!-- Always mounted: a live region has to exist before text lands in it, or
     screen readers never announce the first toast. -->
<div
  class="pointer-events-none fixed bottom-28 left-1/2 z-50 flex -translate-x-1/2 flex-col items-center gap-2"
  role="status"
  aria-live="polite"
>
  {#each toast.toasts as t (t.id)}
    <button
      class="overlay pointer-events-auto flex items-center gap-2 rounded-full py-2 pr-4 pl-3 text-sm font-medium
        {t.kind === 'error' ? 'border border-red-500/40' : ''}"
      onclick={() => toast.dismiss(t.id)}
      title={m.dismiss()}
      transition:fly={{ y: 14, duration: 240 }}
    >
      <svg viewBox="0 0 24 24" class="h-4 w-4 shrink-0 {TONE[t.kind]}" fill="currentColor" aria-hidden="true">
        <path d={ICONS[t.kind]} />
      </svg>
      <span>{t.message}</span>
    </button>
  {/each}
</div>
