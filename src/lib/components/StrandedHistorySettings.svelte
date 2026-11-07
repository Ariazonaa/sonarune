<script lang="ts">
  // Plays of the signed-in account kept under another server's id
  // (`history::stranded`): a server restored into a fresh install at a new
  // address answers with a new id, and nothing tells the app the old one is
  // gone. Shown only when there are some, inside the history section.
  import { onMount } from "svelte";
  import { apiLibrary } from "#lib/api/library.js";
  import { errorCause } from "#lib/appErrors.js";
  import { m } from "#lib/paraglide/messages.js";
  import { getLocale } from "#lib/paraglide/runtime.js";
  import { confirm } from "#lib/state/confirm.svelte.js";
  import { toast } from "#lib/state/toast.svelte.js";
  import type { StrandedPlays } from "#lib/types.js";

  let entries = $state<StrandedPlays[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      entries = await apiLibrary.historyStranded();
    } catch (e) {
      error = errorCause(e);
    }
  }

  onMount(() => {
    void load();
  });

  function date(ms: number) {
    return new Intl.DateTimeFormat(getLocale(), { dateStyle: "medium" }).format(new Date(ms));
  }

  function playsLabel(entry: StrandedPlays) {
    const args = {
      count: entry.plays,
      from: date(entry.firstPlayedAt),
      to: date(entry.lastPlayedAt),
    };
    return entry.plays === 1
      ? m.settings_history_stranded_plays_one(args)
      : m.settings_history_stranded_plays_many(args);
  }

  async function act(entry: StrandedPlays, kind: "claim" | "forget") {
    const server = entry.address;
    const claim = kind === "claim";
    const ok = await confirm.ask(
      claim
        ? {
            title: m.settings_history_stranded_claim_title(),
            body: m.settings_history_stranded_claim_body({ server }),
            confirmLabel: m.settings_history_stranded_claim(),
          }
        : {
            title: m.settings_history_stranded_forget_title(),
            body: m.settings_history_stranded_forget_body({ server }),
            confirmLabel: m.settings_history_stranded_forget(),
            danger: true,
          },
    );
    if (!ok) return;
    busy = true;
    error = null;
    try {
      const count = claim
        ? await apiLibrary.historyClaimStranded(entry.serverId)
        : await apiLibrary.historyForgetStranded(entry.serverId);
      const args = { count };
      // None: the list was older than the store (another account signed in
      // meanwhile, say); the reload below shows what is there now.
      if (count > 0)
        toast.show(
        claim
          ? count === 1
            ? m.settings_history_stranded_claimed_one(args)
            : m.settings_history_stranded_claimed_many(args)
          : count === 1
            ? m.settings_history_stranded_forgotten_one(args)
            : m.settings_history_stranded_forgotten_many(args),
      );
    } catch (e) {
      error = errorCause(e);
    } finally {
      busy = false;
    }
    // Also after a failure: the list must show what is actually stored.
    await load();
  }
</script>

{#if entries.length > 0 || error}
  <div class="mt-5 border-t border-edge pt-4">
    <p class="text-sm font-medium">{m.settings_history_stranded()}</p>
    <p class="mt-0.5 max-w-lg text-xs text-ink-muted">{m.settings_history_stranded_hint()}</p>
    {#if entries.length > 0}
      <ul class="mt-3 space-y-3">
        {#each entries as entry (entry.serverId)}
          <li class="flex flex-wrap items-start justify-between gap-3 rounded-md bg-base px-3 py-2.5">
            <div class="min-w-0 flex-1">
              <p class="break-all text-sm font-medium">{entry.address}</p>
              <p class="mt-1 text-[11px] text-ink-muted tabular-nums">{playsLabel(entry)}</p>
              <p class="mt-0.5 text-[11px] text-ink-muted">
                {entry.lastSeen === null
                  ? m.settings_history_stranded_never_seen()
                  : m.settings_history_stranded_seen({ date: date(entry.lastSeen) })}
              </p>
            </div>
            <div class="flex shrink-0 gap-2">
              <button
                class="rounded-full border border-edge px-3 py-1 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
                disabled={busy}
                aria-label={m.settings_history_stranded_claim_aria({ server: entry.address })}
                onclick={() => void act(entry, "claim")}
              >
                {m.settings_history_stranded_claim()}
              </button>
              <button
                class="rounded-full border border-edge px-3 py-1 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
                disabled={busy}
                aria-label={m.settings_history_stranded_forget_aria({ server: entry.address })}
                onclick={() => void act(entry, "forget")}
              >
                {m.settings_history_stranded_forget()}
              </button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
    {#if error}
      <p class="mt-3 text-xs text-red-400">{m.error_generic({ message: error })}</p>
    {/if}
  </div>
{/if}
