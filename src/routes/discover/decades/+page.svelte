<script lang="ts">
  import { onMount } from "svelte";
  import { apiDiscovery, type DecadeDto } from "$lib/api/discovery";
  import { m } from "$lib/paraglide/messages";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import { player } from "$lib/state/player.svelte";
  import { swrGet, swrSet } from "$lib/swr";

  let decades = $state<DecadeDto[] | null>(swrGet<DecadeDto[]>("discover:all:decades") ?? null);
  let error = $state<string | null>(null);
  let generation = 0;
  async function load() {
    const seq = ++generation;
    try {
      const result = await apiDiscovery.getDecades();
      if (seq !== generation) return;
      decades = result;
      swrSet("discover:all:decades", result);
      error = null;
    } catch (cause) {
      if (seq === generation && !decades) error = String(cause);
    }
  }
  onMount(load);
</script>

<div class="p-6">
  <a href="/discover" class="mb-4 inline-flex items-center gap-1 text-sm font-semibold text-ink-muted hover:text-ink">
    <span aria-hidden="true">←</span> {m.discover_back()}
  </a>
  <h1 class="mb-5 text-2xl font-bold tracking-tight">{m.discover_decades()}</h1>
  {#if error}
    <p class="mb-4 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
      {m.error_generic({ message: error })}
    </p>
  {/if}
  {#if decades?.length}
    <div data-card-grid class="grid grid-cols-[repeat(auto-fill,minmax(11rem,1fr))] gap-4">
      {#each decades as decade (decade.startYear)}
        <article
          class="group relative flex h-36 flex-col justify-end overflow-hidden rounded-xl p-4 shadow-lg"
          style="background: linear-gradient(135deg, hsl({(decade.startYear * 7) % 360} 58% 43%), hsl({(decade.startYear * 7 + 55) % 360} 62% 25%));"
        >
          <a href="/discover/decade/{decade.startYear}" class="absolute inset-0" aria-label={decade.label}></a>
          <p class="relative pointer-events-none text-2xl font-black text-white drop-shadow">{decade.label}</p>
          <div class="relative mt-3 flex gap-2 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
            <button
              class="rounded-full bg-black/40 px-3 py-1 text-xs font-bold text-white backdrop-blur"
              onclick={() => player.run(apiDiscovery.playDecade(decade.startYear, false))}
            >{m.discover_play()}</button>
            <button
              class="rounded-full bg-black/40 px-3 py-1 text-xs font-bold text-white backdrop-blur"
              onclick={() => player.run(apiDiscovery.playDecade(decade.startYear, true))}
            >{m.discover_shuffle()}</button>
          </div>
        </article>
      {/each}
    </div>
  {:else if decades}
    <EmptyState
      icon="discover"
      title={m.discover_section_empty()}
      action={{ label: m.nav_discover(), href: "/discover" }}
    />
  {/if}
</div>
