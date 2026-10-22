import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import { library } from "#lib/state/library.svelte.js";
import { net } from "#lib/state/net.svelte.js";

const invokeMock = vi.mocked(invoke);
let reachable = true;

beforeEach(() => {
  reachable = true;
  invokeMock.mockReset();
  invokeMock.mockImplementation((async (command: string) =>
    command === "check_server"
      ? { online: reachable, untrustedCertificate: null }
      : undefined) as typeof invoke);
  net.reset();
});

describe("net", () => {
  it("tells an untrusted certificate apart from a server that is gone", async () => {
    const fingerprint = "AB:CD";
    invokeMock.mockResolvedValueOnce({ online: false, untrustedCertificate: fingerprint } as never);
    expect(await net.check()).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith("check_server");
    expect(net.online).toBe(false);
    // Its own banner: Retry alone never gets past a certificate.
    expect(net.untrustedCertificate).toBe(fingerprint);

    invokeMock.mockResolvedValueOnce({ online: false, untrustedCertificate: null } as never);
    await net.check();
    expect(net.untrustedCertificate).toBeNull();
  });

  it("refreshes the views when any check finds the server back", async () => {
    reachable = false;
    await net.check();
    expect(net.online).toBe(false);

    const before = library.revision;
    reachable = true;
    // A focus check or the one after a successful action — no Retry click.
    await net.check();
    expect(net.online).toBe(true);
    expect(library.revision).toBe(before + 1);
  });

  it("recovers on a Retry and refreshes the view", async () => {
    reachable = false;
    await net.check();
    const before = library.revision;
    reachable = true;
    await net.retry();
    expect(net.online).toBe(true);
    expect(net.untrustedCertificate).toBeNull();
    expect(library.revision).toBe(before + 1);
  });

  it("leaves the views alone while the server stays reachable", async () => {
    const before = library.revision;
    await net.check();
    await net.retry();
    expect(library.revision).toBe(before);
  });

  it("recovers once from a Retry that meets a check already running", async () => {
    reachable = false;
    await net.check();
    const before = library.revision;
    reachable = true;
    await Promise.all([net.check(), net.retry()]);
    expect(net.online).toBe(true);
    expect(library.revision).toBe(before + 1);
  });

  it("counts a check that cannot run as offline, and drops the certificate it showed", async () => {
    invokeMock.mockResolvedValueOnce({ online: false, untrustedCertificate: "AB" } as never);
    await net.check();
    invokeMock.mockRejectedValueOnce(new Error("ipc down"));
    expect(await net.check()).toBe(false);
    expect(net.online).toBe(false);
    expect(net.untrustedCertificate).toBeNull();
  });
});
