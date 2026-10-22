import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { invoke } from "@tauri-apps/api/core";
import { updates } from "#lib/state/updates.svelte.js";
import type { UpdateInfo } from "#lib/types.js";

let known: UpdateInfo | null = null;
vi.mocked(invoke).mockImplementation((async (command: string) =>
  command === "get_known_update" ? known : undefined) as typeof invoke);

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function windowComesBack() {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
  document.dispatchEvent(new Event("visibilitychange"));
}

const newer = (version: string): UpdateInfo => ({
  currentVersion: "0.6.1",
  version,
  notes: null,
  date: null,
  installable: true,
});

describe("update announcement", () => {
  it("reads back an announcement the hidden window missed", async () => {
    // Started minimized: the background check had nobody to tell.
    await updates.init();
    await settle();
    expect(updates.info).toBeNull();

    known = newer("0.6.2");
    windowComesBack();
    await settle();
    expect(updates.info?.version).toBe("0.6.2");
  });

  it("forgets the other channel's answer after a switch", async () => {
    // A check on beta.
    vi.mocked(invoke).mockImplementationOnce((async () => newer("0.7.0-beta.1")) as typeof invoke);
    await updates.check();
    expect(updates.info?.version).toBe("0.7.0-beta.1");

    // Switched back to stable: until the next check there is no answer.
    known = null;
    updates.forget();
    expect(updates.info).toBeNull();
    windowComesBack();
    await settle();
    expect(updates.info).toBeNull();
  });
});
