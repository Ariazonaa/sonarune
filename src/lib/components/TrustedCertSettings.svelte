<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { m } from "$lib/paraglide/messages";
  import { getLocale } from "$lib/paraglide/runtime";
  import { confirm } from "$lib/state/confirm.svelte";
  import { session } from "$lib/state/session.svelte";
  import type { TrustedCert } from "$lib/types";

  let certs = $state<TrustedCert[] | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      certs = await api.listTrustedCertificates();
    } catch (e) {
      error = String(e);
    }
  }

  onMount(() => {
    void load();
  });

  function isActive(cert: TrustedCert) {
    return session.info?.serverUrl === cert.serverUrl;
  }

  function pinnedLabel(cert: TrustedCert) {
    if (cert.pinnedAt === null) return m.settings_trusted_certs_pinned_unknown();
    const date = new Intl.DateTimeFormat(getLocale(), {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(cert.pinnedAt));
    return m.settings_trusted_certs_pinned_on({ date });
  }

  async function forget(cert: TrustedCert) {
    const active = isActive(cert);
    const ok = await confirm.ask({
      title: m.settings_trusted_certs_forget_title(),
      body: active
        ? m.settings_trusted_certs_forget_active_body({ server: cert.serverUrl })
        : m.settings_trusted_certs_forget_body({ server: cert.serverUrl }),
      confirmLabel: active
        ? m.settings_trusted_certs_forget_active_confirm()
        : m.settings_trusted_certs_forget(),
      danger: true,
    });
    if (!ok) return;
    busy = true;
    error = null;
    try {
      await api.forgetTrustedCertificate(cert.serverUrl, cert.fingerprint);
      if (active) {
        // The running session was built trusting this certificate and would
        // keep doing so until the next start — and then fail on it, with no
        // way back to the confirmation prompt. Sign out so the next sign-in
        // asks for the certificate again.
        await session.disconnect();
        return;
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
    // Also after a failure: the list must show what is actually pinned.
    await load();
  }
</script>

<section class="mb-8 rounded-lg bg-card p-5">
  <h2 class="mb-1 text-sm font-semibold uppercase tracking-wider text-ink-muted">
    {m.settings_trusted_certs()}
  </h2>
  <p class="mb-4 max-w-lg text-xs text-ink-muted">{m.settings_trusted_certs_hint()}</p>

  {#if certs && certs.length === 0}
    <p class="text-sm text-ink-muted">{m.settings_trusted_certs_empty()}</p>
  {:else if certs}
    <ul class="space-y-3">
      {#each certs as cert (`${cert.serverUrl}\n${cert.fingerprint}`)}
        <li class="flex flex-wrap items-start justify-between gap-3 rounded-md bg-base px-3 py-2.5">
          <div class="min-w-0 flex-1">
            <p class="flex flex-wrap items-center gap-2 text-sm font-medium">
              <span class="break-all">{cert.serverUrl}</span>
              {#if isActive(cert)}
                <span
                  class="rounded-full bg-accent/15 px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-accent"
                >
                  {m.settings_trusted_certs_active()}
                </span>
              {/if}
            </p>
            <p class="mt-2 text-[10px] font-medium uppercase tracking-wider text-ink-muted">
              {m.setup_cert_fingerprint()}
            </p>
            <p class="mt-0.5 break-all font-mono text-[11px] leading-relaxed text-ink">
              {cert.fingerprint}
            </p>
            <p class="mt-1 text-[11px] text-ink-muted">{pinnedLabel(cert)}</p>
          </div>
          <button
            class="shrink-0 rounded-full border border-edge px-3 py-1 text-xs font-semibold text-ink-muted transition-colors hover:border-ink-muted hover:text-ink disabled:opacity-40"
            disabled={busy}
            aria-label={m.settings_trusted_certs_forget_aria({ server: cert.serverUrl })}
            onclick={() => void forget(cert)}
          >
            {m.settings_trusted_certs_forget()}
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if error}
    <p class="mt-3 text-xs text-red-400">{m.error_generic({ message: error })}</p>
  {/if}
</section>
