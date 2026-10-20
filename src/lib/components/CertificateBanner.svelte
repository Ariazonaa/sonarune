<script lang="ts">
  import type { Attachment } from "svelte/attachments";
  import { api } from "#lib/api.js";
  import { m } from "#lib/paraglide/messages.js";
  import { portal } from "#lib/portal.js";
  import { net } from "#lib/state/net.svelte.js";

  /**
   * The banner for a server that presents a certificate the session does not
   * trust — a pinned self-signed one that was renewed, or something on the
   * network presenting its own. Retry can never get past that, so instead of
   * the offline banner this one leads to the fingerprint: trusted, it is
   * pinned for the running session (`trust_session_certificate`), without a
   * new sign-in and with the queue intact.
   */
  let { fingerprint }: { fingerprint: string } = $props();

  /** The fingerprint under review. Held while the dialog is open: a check
   *  arriving meanwhile must not swap what the user is comparing. */
  let reviewing = $state<string | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  // Initial focus on Cancel, never on Trust (see ConfirmDialog for why it
  // sits after `portal` on the portaled element).
  const focusCancel: Attachment<HTMLElement> = (node) => {
    node.querySelector("button")?.focus();
  };

  function review() {
    reviewing = fingerprint;
    error = null;
  }

  function close() {
    if (!busy) reviewing = null;
  }

  async function trust() {
    if (!reviewing || busy) return;
    busy = true;
    error = null;
    try {
      await api.trustSessionCertificate(reviewing);
      reviewing = null;
      // Back on a trusted certificate: the banner goes, the view refetches.
      await net.retry();
    } catch (e) {
      error = String(e);
      // The server may present yet another certificate by now.
      void net.check();
    } finally {
      busy = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (reviewing && event.key === "Escape") {
      event.preventDefault();
      close();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<button
  class="flex w-full items-center justify-center gap-3 bg-amber-500/90 px-4 py-1.5 text-xs font-semibold text-black"
  data-testid="cert-banner"
  onclick={review}
>
  {m.cert_changed_banner()}
  <span class="rounded-full bg-black/20 px-2 py-0.5">{m.cert_changed_review()}</span>
</button>

{#if reviewing}
  <!-- The wrapper stays put; the overlay moves to <body> (see portal.ts). -->
  <div class="contents">
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div
      {@attach portal}
      {@attach focusCancel}
      class="fixed inset-0 z-[60] flex items-center justify-center bg-black/75 p-4"
      onclick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        class="overlay w-full max-w-md rounded-overlay p-5"
        role="dialog"
        aria-modal="true"
        aria-label={m.cert_changed_title()}
      >
        <h2 class="text-md font-bold tracking-tight">{m.cert_changed_title()}</h2>
        <p class="mt-2 text-sm leading-relaxed text-ink-muted">{m.cert_changed_body()}</p>
        <p class="mt-3 text-[10px] font-medium uppercase tracking-wider text-ink-muted">
          {m.setup_cert_fingerprint()}
        </p>
        <p
          class="mt-1 break-all rounded bg-base px-2 py-1.5 font-mono text-[11px] leading-relaxed text-ink"
          data-testid="cert-fingerprint"
        >
          {reviewing}
        </p>
        {#if error}
          <p class="mt-3 text-xs text-red-400">{m.error_generic({ message: error })}</p>
        {/if}
        <div class="mt-5 flex justify-end gap-2">
          <button
            class="rounded-full px-4 py-1.5 text-sm font-semibold text-ink-muted transition-colors hover:bg-panel-2 hover:text-ink disabled:opacity-50"
            disabled={busy}
            onclick={close}
          >
            {m.cancel()}
          </button>
          <button
            class="rounded-full bg-accent px-4 py-1.5 text-sm font-bold text-(--color-on-accent) transition-colors hover:bg-accent-hover disabled:opacity-50"
            disabled={busy}
            data-testid="cert-trust"
            onclick={trust}
          >
            {busy ? m.cert_changed_trusting() : m.cert_changed_trust()}
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}
