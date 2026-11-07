import { flushSync, mount, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import StrandedHistorySettings from "#lib/components/StrandedHistorySettings.svelte";
import { confirm } from "#lib/state/confirm.svelte.js";
import { toast } from "#lib/state/toast.svelte.js";
import type { StrandedPlays } from "#lib/types.js";

// Plays of the account under the id of a server that no longer answers —
// a server restored into a fresh install at a new address. Settings lists
// them, and they can be taken over or deleted, each after a confirmation.
const invokeMock = vi.mocked(invoke);
let instance: Record<string, unknown> | null = null;
let stored: StrandedPlays[] = [];

const old: StrandedPlays = {
  serverId: "old-id",
  plays: 2,
  firstPlayedAt: Date.UTC(2026, 1, 1, 12),
  lastPlayedAt: Date.UTC(2026, 1, 3, 12),
  address: "http://10.0.0.5:8096",
  lastSeen: Date.UTC(2026, 1, 3, 12),
};

async function settle() {
  for (let i = 0; i < 20; i++) await Promise.resolve();
  flushSync();
}

async function render() {
  instance = mount(StrandedHistorySettings, { target: document.body });
  await settle();
}

function button(label: string) {
  return document.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
}

beforeEach(() => {
  stored = [old];
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "history_stranded") return stored as never;
    if (command === "history_claim_stranded" || command === "history_forget_stranded") {
      const { serverId } = args as { serverId: string };
      const gone = stored.filter((entry) => entry.serverId === serverId);
      stored = stored.filter((entry) => entry.serverId !== serverId);
      return gone.reduce((sum, entry) => sum + entry.plays, 0) as never;
    }
    return undefined as never;
  });
});

afterEach(() => {
  confirm.answer(false);
  if (instance) {
    unmount(instance);
    instance = null;
  }
  document.body.innerHTML = "";
});

describe("StrandedHistorySettings", () => {
  it("shows nothing while there are no such plays", async () => {
    stored = [];
    await render();
    expect(document.body.textContent?.trim()).toBe("");
  });

  it("lists the plays by the address they were played under", async () => {
    await render();
    expect(document.body.textContent).toContain(old.address);
    expect(document.querySelectorAll("li")).toHaveLength(1);
  });

  it("takes them over only once confirmed, then lists what is left", async () => {
    await render();
    button(`Take over the plays under ${old.address}`)!.click();
    await settle();
    expect(confirm.request?.title).toBe("Take over these plays?");
    expect(invokeMock).not.toHaveBeenCalledWith("history_claim_stranded", expect.anything());
    confirm.answer(true);
    await settle();
    expect(invokeMock).toHaveBeenCalledWith("history_claim_stranded", { serverId: "old-id" });
    expect(invokeMock).toHaveBeenLastCalledWith("history_stranded");
    expect(document.querySelectorAll("li")).toHaveLength(0);
  });

  it("says how many went, and nothing when none were left", async () => {
    const shown = vi.spyOn(toast, "show");
    stored = [{ ...old, plays: 1 }];
    await render();
    button(`Delete the plays under ${old.address}`)!.click();
    await settle();
    confirm.answer(true);
    await settle();
    expect(shown).toHaveBeenLastCalledWith("1 play deleted");
    // Shown before another account signed in: the store has none of them.
    stored = [old];
    unmount(instance!);
    await render();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "history_stranded") return [] as never;
      return 0 as never;
    });
    shown.mockClear();
    button(`Take over the plays under ${old.address}`)!.click();
    await settle();
    confirm.answer(true);
    await settle();
    expect(shown).not.toHaveBeenCalled();
    expect(document.querySelectorAll("li")).toHaveLength(0);
    shown.mockRestore();
  });

  it("deletes nothing when the confirmation is cancelled", async () => {
    await render();
    button(`Delete the plays under ${old.address}`)!.click();
    await settle();
    expect(confirm.request?.title).toBe("Delete these plays?");
    confirm.answer(false);
    await settle();
    expect(invokeMock).not.toHaveBeenCalledWith("history_forget_stranded", expect.anything());
    expect(document.querySelectorAll("li")).toHaveLength(1);
  });

  it("shows why a step failed and keeps the list as stored", async () => {
    await render();
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "history_forget_stranded") throw "database is locked";
      if (command === "history_stranded") return stored as never;
      return undefined as never;
    });
    button(`Delete the plays under ${old.address}`)!.click();
    await settle();
    confirm.answer(true);
    await settle();
    expect(document.body.textContent).toContain("database is locked");
    expect(document.querySelectorAll("li")).toHaveLength(1);
  });
});
