<script lang="ts">
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import { portal } from "$lib/portal";
  import type { TrackInfoDto } from "$lib/types";

  let {
    itemId,
    name,
    playCount = 0,
    onclose,
  }: { itemId: string; name: string; playCount?: number; onclose: () => void } = $props();

  let info = $state<TrackInfoDto | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    const id = itemId;
    info = null;
    error = null;
    api
      .getTrackInfo(id)
      .then((i) => {
        if (id === itemId) info = i;
      })
      .catch((e) => {
        if (id === itemId) error = String(e);
      });
  });

  function formatSize(bytes: number): string {
    const mb = bytes / (1024 * 1024);
    return mb >= 1024 ? `${(mb / 1024).toFixed(2)} GB` : `${mb.toFixed(1)} MB`;
  }
  const channelsLabel = (n: number) =>
    n === 1 ? m.info_channels_mono() : n === 2 ? m.info_channels_stereo() : String(n);

  const rows = $derived.by(() => {
    const r: { label: string; value: string }[] = [];
    if (playCount > 0) r.push({ label: m.info_play_count(), value: String(playCount) });
    const i = info;
    if (i) {
      if (i.codec) r.push({ label: m.info_codec(), value: i.codec.toUpperCase() });
      if (i.bitrateKbps) r.push({ label: m.info_bitrate(), value: `${i.bitrateKbps} kbps` });
      if (i.sampleRateHz)
        r.push({ label: m.info_sample_rate(), value: `${(i.sampleRateHz / 1000).toFixed(1)} kHz` });
      if (i.bitDepth) r.push({ label: m.info_bit_depth(), value: `${i.bitDepth}-bit` });
      if (i.channels) r.push({ label: m.info_channels(), value: channelsLabel(i.channels) });
      if (i.container) r.push({ label: m.info_container(), value: i.container.toUpperCase() });
      if (i.sizeBytes) r.push({ label: m.info_size(), value: formatSize(i.sizeBytes) });
      if (i.path) r.push({ label: m.info_path(), value: i.path });
    }
    return r;
  });

  function onKeydown(e: KeyboardEvent) {
    // An overlay on top (a confirmation) already handled it.
    if (e.defaultPrevented) return;
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
    class="overlay w-full max-w-md rounded-overlay p-5"
    role="dialog"
    aria-modal="true"
    aria-label={m.track_info_title()}
  >
    <div class="mb-4 flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h2 class="text-sm font-bold tracking-tight">{m.track_info_title()}</h2>
        <p class="truncate text-xs text-ink-muted">{name}</p>
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

    {#if error}
      <p class="rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
        {m.error_generic({ message: error })}
      </p>
    {:else if rows.length === 0}
      <p class="text-sm text-ink-muted">…</p>
    {:else}
      <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
        {#each rows as row (row.label)}
          <dt class="font-medium text-ink-muted">{row.label}</dt>
          <dd class="min-w-0 truncate text-right" title={row.value}>{row.value}</dd>
        {/each}
      </dl>
    {/if}
  </div>
</div>
</div>
