<script lang="ts">
  import { onMount } from "svelte";
  import { m } from "$lib/paraglide/messages";
  import { downloads } from "$lib/state/downloads.svelte";
  import type { DownloadTask } from "$lib/types";

  onMount(() => {
    downloads.init();
    return () => downloads.destroy();
  });

  function formatBytes(value: number): string {
    const units = ["B", "KB", "MB", "GB"];
    let amount = value;
    let unit = 0;
    while (amount >= 1024 && unit < units.length - 1) {
      amount /= 1024;
      unit++;
    }
    return `${amount.toLocaleString(undefined, { maximumFractionDigits: unit === 0 ? 0 : 1 })} ${units[unit]}`;
  }

  function progress(task: DownloadTask): number {
    if (!task.totalBytes || task.totalBytes <= 0) return 0;
    return Math.min(100, (task.receivedBytes / task.totalBytes) * 100);
  }

  function status(task: DownloadTask): string {
    if (task.status === "queued") return m.download_status_queued();
    if (task.status === "downloading") {
      return task.totalBytes
        ? m.download_status_progress({ received: formatBytes(task.receivedBytes), total: formatBytes(task.totalBytes) })
        : m.download_status_progress_unknown({ received: formatBytes(task.receivedBytes) });
    }
    if (task.status === "completed") return m.download_status_completed();
    if (task.status === "cancelled") return m.download_status_cancelled();
    return m.download_status_failed();
  }
</script>

{#if downloads.tasks.length > 0}
  {#if downloads.open}
    <section
      class="overlay fixed bottom-24 right-4 z-40 flex max-h-[28rem] w-[min(26rem,calc(100vw-2rem))] flex-col overflow-hidden rounded-overlay"
      aria-label={m.downloads_title()}
    >
      <header class="flex items-center gap-2 border-b border-ink/10 px-4 py-3">
        <div class="min-w-0 flex-1">
          <h2 class="text-sm font-bold">{m.downloads_title()}</h2>
          <p class="text-xs text-ink-muted">
            {downloads.activeCount > 0
              ? m.downloads_active({ count: downloads.activeCount })
              : m.downloads_finished()}
          </p>
        </div>
        <button class="rounded-md px-2 py-1 text-xs font-semibold hover:bg-ink/10" onclick={() => downloads.openFolder()}>
          {m.download_open_folder()}
        </button>
        {#if downloads.finishedCount > 0}
          <button class="rounded-md px-2 py-1 text-xs font-semibold hover:bg-ink/10" onclick={() => downloads.clearFinished()}>
            {m.download_clear_finished()}
          </button>
        {/if}
        <button
          class="grid h-7 w-7 place-items-center rounded-md text-ink-muted hover:bg-ink/10 hover:text-ink"
          aria-label={m.download_hide()}
          onclick={() => (downloads.open = false)}
        >
          −
        </button>
      </header>

      {#if downloads.error}
        <button class="bg-red-950/60 px-4 py-2 text-left text-xs text-red-300" onclick={() => (downloads.error = null)}>
          {downloads.error}
        </button>
      {/if}

      <ul class="overflow-y-auto p-2">
        {#each [...downloads.tasks].reverse() as task (task.id)}
          <li class="mb-1 rounded-lg bg-card px-3 py-2 last:mb-0">
            <div class="flex items-start gap-3">
              <div class="min-w-0 flex-1">
                <p class="truncate text-sm font-semibold" title={task.name}>{task.name}</p>
                <p class="truncate text-xs text-ink-muted" title={task.error ?? undefined}>
                  {task.error ?? status(task)}
                </p>
              </div>
              {#if task.status === "queued" || task.status === "downloading"}
                <button class="rounded px-2 py-1 text-xs font-semibold hover:bg-ink/10" onclick={() => downloads.cancel(task.id)}>
                  {m.download_cancel()}
                </button>
              {:else if task.status === "failed" || task.status === "cancelled"}
                <button class="rounded px-2 py-1 text-xs font-semibold hover:bg-ink/10" onclick={() => downloads.retry(task.id)}>
                  {m.download_retry()}
                </button>
              {/if}
            </div>
            {#if task.status === "downloading"}
              <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-ink/10">
                {#if task.totalBytes}
                  <div class="h-full rounded-full bg-accent transition-[width]" style:width={`${progress(task)}%`}></div>
                {:else}
                  <div class="h-full w-1/3 animate-pulse rounded-full bg-accent"></div>
                {/if}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  {:else}
    <button
      class="fixed bottom-24 right-4 z-40 rounded-full border border-ink/10 bg-panel-2 px-4 py-2 text-sm font-bold shadow-xl hover:bg-card"
      onclick={() => (downloads.open = true)}
    >
      {m.downloads_title()}{#if downloads.activeCount > 0} · {downloads.activeCount}{/if}
    </button>
  {/if}
{/if}
