<script lang="ts">
  import { onDestroy } from "svelte";
  import { api } from "$lib/api";
  import BrandMark from "$lib/components/BrandMark.svelte";
  import { session } from "$lib/state/session.svelte";
  import { m } from "$lib/paraglide/messages";
  import type { QuickConnectSession } from "$lib/types";

  let serverUrl = $state("");
  let username = $state("");
  let password = $state("");
  let acceptInvalidCerts = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  // Set when the server presents an untrusted certificate the user must confirm.
  let pendingFingerprint = $state<string | null>(null);

  // Quick Connect: show a code, poll until it is approved on another device.
  let quickConnect = $state<QuickConnectSession | null>(null);
  let pollTimer: ReturnType<typeof setInterval> | undefined;

  async function connectWith(trustFingerprint?: string) {
    busy = true;
    error = null;
    try {
      const result = await session.connect({
        serverUrl,
        username,
        password,
        acceptInvalidCerts,
        trustFingerprint,
      });
      // Server presented a self-signed/untrusted cert: surface the fingerprint
      // for the user to verify before we pin it.
      pendingFingerprint =
        result.status === "certUntrusted" ? result.fingerprint : null;
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (quickConnect) return; // Enter while the code screen is up
    await connectWith();
  }

  let pollInFlight = false;
  /** Transient network blips must not kill a still-valid code. */
  const MAX_CONSECUTIVE_POLL_FAILURES = 5;

  async function startQuickConnect() {
    if (!serverUrl.trim() || quickConnect) return;
    busy = true;
    error = null;
    // Snapshot: edits to the form while the code screen is up must not
    // change what the poll talks to.
    const url = serverUrl.trim();
    const certs = acceptInvalidCerts;
    let failures = 0;
    try {
      quickConnect = await api.quickConnectStart(url, certs);
      pollTimer = setInterval(async () => {
        // A slow poll must not overlap the next tick (the secret is consumed
        // by the first successful authentication).
        if (!quickConnect || pollInFlight) return;
        pollInFlight = true;
        try {
          const info = await api.quickConnectPoll(url, certs, quickConnect.secret);
          failures = 0;
          if (info) {
            stopQuickConnect();
            session.info = info;
          }
        } catch (e) {
          failures += 1;
          if (failures >= MAX_CONSECUTIVE_POLL_FAILURES) {
            stopQuickConnect();
            error = String(e);
          }
        } finally {
          pollInFlight = false;
        }
      }, 2000);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function stopQuickConnect() {
    clearInterval(pollTimer);
    pollTimer = undefined;
    quickConnect = null;
  }

  onDestroy(stopQuickConnect);
</script>

<div class="flex h-full items-center justify-center">
  <form class="surface w-96 rounded-panel bg-panel p-8" onsubmit={submit} data-testid="setup-screen">
    <div class="mb-1 flex items-center gap-2">
      <div class="flex h-8 w-8 items-center justify-center rounded-full bg-accent">
        <BrandMark class="h-4.5 w-4.5 text-(--color-on-accent)" />
      </div>
      <h1 class="text-xl font-extrabold tracking-tight">{m.app_name()}</h1>
    </div>
    <h2 class="mt-4 text-lg font-medium">{m.setup_title()}</h2>
    <p class="mb-6 text-sm text-ink-muted">{m.setup_subtitle()}</p>

    <label class="mb-3 block">
      <span class="mb-1 block text-xs font-medium text-ink-muted">{m.setup_server_url()}</span>
      <input
        type="url"
        required
        bind:value={serverUrl}
        placeholder={m.setup_server_placeholder()}
        data-testid="setup-server"
        class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
      />
    </label>

    {#if quickConnect}
      <div class="mb-5 rounded-lg bg-card p-5 text-center">
        <p class="text-4xl font-black tracking-[0.3em]">{quickConnect.code}</p>
        <p class="mt-3 text-xs text-ink-muted">{m.setup_quick_connect_hint()}</p>
        <div class="mt-3 flex items-center justify-center gap-1 text-ink-muted">
          <span class="h-1.5 w-1.5 animate-pulse rounded-full bg-accent"></span>
          <span class="h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:200ms]"></span>
          <span class="h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:400ms]"></span>
        </div>
      </div>
      <button
        type="button"
        class="w-full rounded-full border border-edge py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink"
        onclick={stopQuickConnect}
      >
        {m.setup_back()}
      </button>
    {:else}
      <label class="mb-3 block">
        <span class="mb-1 block text-xs font-medium text-ink-muted">{m.setup_username()}</span>
        <input
          type="text"
          required
          bind:value={username}
          data-testid="setup-username"
          class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
        />
      </label>
      <label class="mb-3 block">
        <span class="mb-1 block text-xs font-medium text-ink-muted">{m.setup_password()}</span>
        <input
          type="password"
          bind:value={password}
          data-testid="setup-password"
          class="w-full rounded-md border border-edge bg-base px-3 py-2 text-sm outline-none focus:border-accent"
        />
      </label>
      <label class="mb-5 flex items-center gap-2 text-sm text-ink-muted">
        <input type="checkbox" bind:checked={acceptInvalidCerts} class="accent-(--color-accent)" />
        {m.setup_accept_invalid_certs()}
      </label>
    {/if}

    {#if error}
      <p class="my-4 rounded-md border border-red-900 bg-red-950/50 px-3 py-2 text-sm text-red-300">
        {error}
      </p>
    {/if}

    {#if pendingFingerprint}
      <div class="my-4 rounded-lg border border-amber-700/50 bg-amber-950/30 p-4">
        <p class="text-sm font-bold text-amber-200">{m.setup_cert_untrusted_title()}</p>
        <p class="mt-1 text-xs text-ink-muted">{m.setup_cert_untrusted_body()}</p>
        <p class="mt-3 text-[10px] font-medium uppercase tracking-wider text-ink-muted">
          {m.setup_cert_fingerprint()}
        </p>
        <p class="mt-1 break-all rounded bg-base px-2 py-1.5 font-mono text-[11px] leading-relaxed text-ink">
          {pendingFingerprint}
        </p>
        <div class="mt-4 flex gap-2">
          <button
            type="button"
            disabled={busy}
            class="flex-1 rounded-full bg-accent py-2 text-sm font-bold text-(--color-on-accent) transition-all hover:bg-accent-hover disabled:opacity-50"
            onclick={() => connectWith(pendingFingerprint ?? undefined)}
          >
            {busy ? m.setup_connecting() : m.setup_cert_trust()}
          </button>
          <button
            type="button"
            disabled={busy}
            class="flex-1 rounded-full border border-edge py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-50"
            onclick={() => (pendingFingerprint = null)}
          >
            {m.setup_back()}
          </button>
        </div>
      </div>
    {:else if !quickConnect}
      <button
        type="submit"
        disabled={busy}
        data-testid="setup-connect"
        class="w-full rounded-full bg-accent py-2.5 text-sm font-bold text-(--color-on-accent) transition-all hover:scale-[1.02] hover:bg-accent-hover disabled:opacity-50"
      >
        {busy ? m.setup_connecting() : m.setup_connect()}
      </button>
      <button
        type="button"
        disabled={busy || !serverUrl.trim()}
        class="mt-2 w-full rounded-full border border-edge py-2 text-sm font-bold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-50"
        onclick={startQuickConnect}
      >
        {m.setup_use_quick_connect()}
      </button>
    {/if}
  </form>
</div>
