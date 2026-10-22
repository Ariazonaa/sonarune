import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import CertificateBanner from "#lib/components/CertificateBanner.svelte";

// A pinned self-signed server renewed its certificate: the session cannot
// reach it, and Retry never would. The banner leads to the fingerprint, and
// trusting it there re-pins without a new sign-in.
const invokeMock = vi.mocked(invoke);
const fingerprint = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";
let instance: Record<string, unknown> | null = null;

function render() {
  instance = mount(CertificateBanner, { target: document.body, props: { fingerprint } });
  flushSync();
}

const byTestId = (id: string) => document.querySelector<HTMLElement>(`[data-testid="${id}"]`);

async function settle() {
  for (let i = 0; i < 5; i++) await Promise.resolve();
  flushSync();
}

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "check_server") return { online: true, untrustedCertificate: null } as never;
    return undefined as never;
  });
});

afterEach(() => {
  if (instance) {
    unmount(instance);
    instance = null;
  }
  document.body.innerHTML = "";
});

describe("CertificateBanner", () => {
  it("shows the fingerprint for review, with the focus on Cancel", () => {
    render();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    byTestId("cert-banner")!.click();
    flushSync();

    const dialog = document.querySelector('[role="dialog"]');
    expect(dialog?.getAttribute("aria-modal")).toBe("true");
    expect(byTestId("cert-fingerprint")?.textContent?.trim()).toBe(fingerprint);
    const buttons = [...dialog!.querySelectorAll("button")];
    expect(document.activeElement).toBe(buttons[0]);
    expect(buttons[0]).not.toBe(byTestId("cert-trust"));
  });

  it("trusts exactly the reviewed certificate, then checks the server again", async () => {
    render();
    byTestId("cert-banner")!.click();
    flushSync();
    byTestId("cert-trust")!.click();
    await settle();

    expect(invokeMock).toHaveBeenCalledWith("trust_session_certificate", { fingerprint });
    expect(invokeMock).toHaveBeenLastCalledWith("check_server");
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  });

  it("keeps the dialog open with the reason when trusting fails", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "trust_session_certificate") {
        throw "the server presents a different certificate now";
      }
      return { online: false, untrustedCertificate: fingerprint } as never;
    });
    render();
    byTestId("cert-banner")!.click();
    flushSync();
    byTestId("cert-trust")!.click();
    await settle();

    expect(document.querySelector('[role="dialog"]')).not.toBeNull();
    expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
      "the server presents a different certificate now",
    );
  });

  it("closes on Escape without trusting anything", () => {
    render();
    byTestId("cert-banner")!.click();
    flushSync();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    flushSync();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(invokeMock).not.toHaveBeenCalledWith("trust_session_certificate", expect.anything());
  });
});
