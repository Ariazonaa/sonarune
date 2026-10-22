<script lang="ts">
  import { onMount } from "svelte";
  import { m } from "#lib/paraglide/messages.js";
  import { downloads } from "#lib/state/downloads.svelte.js";
  import type { DownloadTask } from "#lib/types.js";

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

  const view = $derived(downloads.panel);
</script>

{#snippet row(task: DownloadTask)}
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
          <!-- Scaled, not sized: a width transition lays out and repaints
               the page every frame it runs, twice a second per bar — half a
               core with the panel open on a big batch. A transform runs on
               the compositor. -->
          <div
            class="h-full w-full origin-left rounded-full bg-accent transition-transform"
            style:transform={`scaleX(${progress(task) / 100})`}
          ></div>
        {:else}
          <div class="h-full w-1/3 animate-pulse rounded-full bg-accent"></div>
        {/if}
      </div>
    {/if}
  </li>
{/snippet}

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

      <!-- The running downloads and the next ones, then those that failed or
           were cancelled — they wait for a retry —, then the last to complete
           (see `panelView`): a batch of thousands lists a few dozen rows, and
           a change redraws only the row of the task it is about. -->
      <ul class="overflow-y-auto p-2">
        {#each view.active as task (task.id)}
          {@render row(task)}
        {/each}
        {#if view.moreQueued > 0 || downloads.activeCount > 1}
          <li class="mb-1 flex items-center gap-2 px-3 py-1 text-xs last:mb-0">
            <span class="min-w-0 flex-1 truncate text-ink-muted">
              {#if view.moreQueued > 0}
                {(view.moreQueued === 1 ? m.download_more_queued_one : m.download_more_queued_many)({
                  count: view.moreQueued,
                })}
              {/if}
            </span>
            {#if downloads.activeCount > 1}
              <button class="shrink-0 rounded px-2 py-1 font-semibold hover:bg-ink/10" onclick={() => downloads.cancelAll()}>
                {m.download_cancel_all()}
              </button>
            {/if}
          </li>
        {/if}
        {#if downloads.stoppedCount > 0}
          <li class="mb-1 flex items-center gap-2 px-3 py-1 text-xs last:mb-0">
            <span class="min-w-0 flex-1 truncate font-semibold text-ink-muted">
              {(downloads.stoppedCount === 1 ? m.download_stopped_one : m.download_stopped_many)({
                count: downloads.stoppedCount,
              })}
            </span>
            <button class="shrink-0 rounded px-2 py-1 font-semibold hover:bg-ink/10" onclick={() => downloads.retryAll()}>
              {m.download_retry_all()}
            </button>
          </li>
        {/if}
        {#each view.stopped as task (task.id)}
          {@render row(task)}
        {/each}
        {#if view.moreStopped > 0}
          <li class="mb-1 px-3 py-1.5 text-xs text-ink-muted last:mb-0">
            {(view.moreStopped === 1 ? m.download_more_stopped_one : m.download_more_stopped_many)({
              count: view.moreStopped,
            })}
          </li>
        {/if}
        {#each view.completed as task (task.id)}
          {@render row(task)}
        {/each}
        {#if view.moreCompleted > 0}
          <li class="px-3 py-1.5 text-xs text-ink-muted">
            {(view.moreCompleted === 1 ? m.download_more_completed_one : m.download_more_completed_many)({
              count: view.moreCompleted,
            })}
          </li>
        {/if}
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
