import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EnergyState } from "./engine";
import type { PresetCatalogProgress, PresetEntry } from "./presets";

// The controller's own logic — switching, quarantine, preferences — against a
// stand-in engine (no WebGL, no AudioContext) and hand-made preset packs.
const fake = vi.hoisted(() => {
  const state = {
    contextLost: false,
    energy: null as ((state: EnergyState) => void) | null,
    /** Throws instead of loading, like butterchurn on a broken preset. */
    fail: null as ((preset: unknown) => Error | null) | null,
  };
  const engine = {
    get contextLost() {
      return state.contextLost;
    },
    loadPreset: vi.fn((preset: unknown) => {
      const error = state.fail?.(preset);
      if (error) throw error;
    }),
    setEnergyListener: (listener: ((state: EnergyState) => void) | null) => {
      state.energy = listener;
    },
    setSensitivity: vi.fn(),
    setQuality: vi.fn(),
    songTitle: vi.fn(),
    setSize: vi.fn(),
    capture: () => null,
    destroy: vi.fn(async () => {}),
  };
  return { state, engine };
});

vi.mock("./engine", () => ({ VisualizerEngine: { create: async () => fake.engine } }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./presets", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./presets")>()),
  loadInitialPresetPack: vi.fn(),
  loadAdditionalPresetPacks: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { m } from "#lib/paraglide/messages.js";
import { REMOTE_PRESETS_KEY, VisualizerController } from "./controller";
import { loadAdditionalPresetPacks, loadInitialPresetPack } from "./presets";

const QUARANTINED_KEY = "sonarune.vis.quarantined";
const FAVORITES_KEY = "sonarune.vis.favorites";
const BLOCKED_KEY = "sonarune.vis.blocked";
const CYCLE_KEY = "sonarune.vis.cycle";
const WEEKLY = "https://s3-us-east-2.amazonaws.com/butterchurn-presets/";

function entry(name: string, sourceUrl?: string): PresetEntry {
  return {
    name,
    preset: sourceUrl ? null : { baseVals: {}, name },
    sourceUrl,
    pack: "test",
    author: "Unknown",
    intensity: "medium",
    complexity: "light",
    priority: 0,
  };
}

/** The name a loaded preset was made for (see `entry`). */
const nameOf = (preset: unknown) => (preset as { name?: string }).name;

let deliverPack: (entries: PresetEntry[]) => void;

/** Let the async load chain (resolvePreset, #pickNext) run to its end. */
async function settle() {
  for (let i = 0; i < 20; i++) await Promise.resolve();
}

async function create(base: PresetEntry[]) {
  vi.mocked(loadInitialPresetPack).mockResolvedValue({ id: "base", label: "Base", entries: base });
  const callbacks = {
    onError: vi.fn(),
    onNotice: vi.fn(),
    onPreset: vi.fn(),
    isCancelled: () => false,
  };
  const controller = await VisualizerController.create(document.createElement("canvas"), callbacks);
  await settle();
  return { controller: controller!, ...callbacks };
}

const storedNames = (key: string): string[] => JSON.parse(localStorage.getItem(key) ?? "[]");

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  fake.state.contextLost = false;
  fake.state.energy = null;
  fake.state.fail = null;
  fake.engine.loadPreset.mockClear();
  vi.mocked(invoke).mockReset();
  vi.mocked(loadAdditionalPresetPacks).mockImplementation(async (onPack) => {
    let loaded = 1;
    deliverPack = (entries) => {
      loaded += 1;
      const progress: PresetCatalogProgress = { loadedPacks: loaded, totalPacks: 7, failedPacks: [] };
      onPack({ id: `pack${loaded}`, label: `Pack ${loaded}`, entries }, progress);
    };
    return { loadedPacks: 1, totalPacks: 7, failedPacks: [] };
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("a lost graphics context", () => {
  it("never quarantines the presets that fail because of it", async () => {
    const { controller, onNotice, onPreset } = await create([entry("a"), entry("b"), entry("c")]);
    expect(onPreset).toHaveBeenCalledTimes(1);

    // A GPU reset: butterchurn's shader setup now throws for every preset.
    fake.state.contextLost = true;
    fake.state.fail = () => new TypeError("parameter 1 is not of type 'WebGLShader'");
    controller.nextPreset();
    controller.onTrackChange("next track");
    controller.selectPreset("b");
    await vi.advanceTimersByTimeAsync(120_000); // auto mode's max dwell, many times over
    fake.state.energy?.({ level: "intense", rmsDb: -10, breakDetected: true, beatDetected: true });
    await settle();

    expect(controller.quarantinedNames()).toEqual([]);
    expect(localStorage.getItem(QUARANTINED_KEY)).toBeNull();
    expect(onNotice).not.toHaveBeenCalled();
    expect(fake.engine.loadPreset).toHaveBeenCalledTimes(1);
  });

  it("is not blamed on the preset even when the load fails before the loss is reported", async () => {
    const { controller, onNotice } = await create([entry("a"), entry("b")]);
    // The context reports the loss as the load fails, before its event fired.
    fake.state.fail = () => {
      fake.state.contextLost = true;
      return new TypeError("parameter 1 is not of type 'WebGLShader'");
    };
    controller.nextPreset();
    await settle();
    expect(controller.quarantinedNames()).toEqual([]);
    expect(onNotice).not.toHaveBeenCalled();
  });
});

describe("a preset that fails to load", () => {
  it("is a notice, not an error, and the next preset takes over", async () => {
    fake.state.fail = (preset) => (nameOf(preset) === "broken" ? new Error("bad shader") : null);
    const { controller, onError, onNotice, onPreset } = await create([entry("broken")]);
    expect(onPreset).not.toHaveBeenCalled();

    deliverPack([entry("fine")]);
    await settle();
    expect(onNotice).toHaveBeenCalledWith(
      m.visualizer_error_preset_skipped({ name: "broken", message: "bad shader" }),
    );
    expect(onError).not.toHaveBeenCalled();
    expect(controller.quarantinedNames()).toEqual(["broken"]);
    expect(onPreset).toHaveBeenLastCalledWith("fine");
  });

  it("joins what another window quarantined instead of overwriting it", async () => {
    localStorage.setItem(QUARANTINED_KEY, JSON.stringify(["broken elsewhere"]));
    fake.state.fail = (preset) => (nameOf(preset) === "broken here" ? new Error("bad") : null);
    const { controller } = await create([entry("fine")]);
    // The projector quarantines one in the meantime.
    localStorage.setItem(QUARANTINED_KEY, JSON.stringify(["broken elsewhere", "broken there"]));

    controller.selectPreset("fine");
    deliverPack([entry("broken here")]);
    controller.selectPreset("broken here");
    await settle();
    expect(storedNames(QUARANTINED_KEY).sort()).toEqual(
      ["broken elsewhere", "broken here", "broken there"],
    );

    // A reset in the main window clears the stored list: it stays cleared.
    localStorage.removeItem(QUARANTINED_KEY);
    deliverPack([entry("broken too")]);
    fake.state.fail = (preset) => (nameOf(preset) === "broken too" ? new Error("bad") : null);
    controller.selectPreset("broken too");
    await settle();
    expect(storedNames(QUARANTINED_KEY)).toEqual(["broken too"]);
  });
});

describe("the first preset", () => {
  it("comes from a later pack when favorites-only finds none in the base pack", async () => {
    localStorage.setItem(FAVORITES_KEY, JSON.stringify(["extra favorite"]));
    localStorage.setItem("sonarune.vis.favoritesOnly", "true");
    const { onPreset } = await create([entry("base a"), entry("base b")]);
    expect(onPreset).not.toHaveBeenCalled();

    deliverPack([entry("extra favorite"), entry("extra other")]);
    await settle();
    expect(onPreset).toHaveBeenCalledWith("extra favorite");
  });

  it("is not picked again once one is on screen", async () => {
    const { onPreset } = await create([entry("base a")]);
    expect(onPreset).toHaveBeenCalledTimes(1);
    deliverPack([entry("extra")]);
    await settle();
    expect(onPreset).toHaveBeenCalledTimes(1);
  });
});

describe("switching", () => {
  it("does not abort a preset download for an automatic switch", async () => {
    localStorage.setItem(REMOTE_PRESETS_KEY, "true");
    const file = `${"a".repeat(32)}.json`;
    let finish!: (body: string) => void;
    vi.mocked(invoke).mockImplementation(
      () => new Promise((resolve) => (finish = resolve as (body: string) => void)),
    );
    const { controller, onPreset } = await create([entry("local")]);
    deliverPack([entry("weekly", WEEKLY + file)]);
    onPreset.mockClear();

    // 45 s on one preset: auto mode wants a switch, waiting for a beat …
    await vi.advanceTimersByTimeAsync(45_000);
    // … when the user picks an online preset, which takes a while to download.
    controller.selectPreset("weekly");
    await settle();
    expect(invoke).toHaveBeenCalledWith("fetch_weekly_preset", { file });
    fake.state.energy?.({ level: "medium", rmsDb: -20, breakDetected: true, beatDetected: true });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(onPreset).not.toHaveBeenCalled();

    finish(JSON.stringify({ baseVals: {}, name: "weekly" }));
    await settle();
    expect(onPreset).toHaveBeenCalledWith("weekly");
    expect(onPreset).toHaveBeenCalledTimes(1);
  });

  it("gives a preset picked by hand a full fixed cycle", async () => {
    localStorage.setItem(CYCLE_KEY, "10");
    const { controller, onPreset } = await create([entry("a"), entry("b"), entry("c")]);
    expect(onPreset).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(9_500);
    controller.selectPreset("b");
    await settle();
    expect(onPreset).toHaveBeenLastCalledWith("b");

    // Where a free-running interval would have fired half a second later.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(onPreset).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(9_000);
    await settle();
    expect(onPreset).toHaveBeenCalledTimes(3);
  });

  it("keeps switching on a fixed cycle when a switch found nothing", async () => {
    localStorage.setItem(CYCLE_KEY, "10");
    localStorage.setItem(BLOCKED_KEY, JSON.stringify(["a", "b"]));
    const { controller, onPreset } = await create([entry("a"), entry("b")]);
    expect(onPreset).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(10_000);
    // The user unblocks one: the next tick still comes.
    localStorage.setItem(BLOCKED_KEY, JSON.stringify(["a"]));
    controller.syncStoredPreferences(BLOCKED_KEY);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(onPreset).toHaveBeenCalledWith("b");
  });
});

describe("preferences changed in another window", () => {
  it("reach an open projector", async () => {
    const { controller, onPreset } = await create([entry("strobe"), entry("calm")]);
    // The main window's preset browser hides one and picks a 30 s cycle.
    localStorage.setItem(BLOCKED_KEY, JSON.stringify(["strobe"]));
    localStorage.setItem(CYCLE_KEY, "30");
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    controller.syncStoredPreferences(BLOCKED_KEY);
    controller.syncStoredPreferences(CYCLE_KEY);
    controller.syncStoredPreferences("sonarune.vis.projector.v1"); // not a preset key
    expect(setItem).not.toHaveBeenCalled(); // taking them over writes nothing back
    setItem.mockRestore();
    expect(controller.blockedNames()).toEqual(["strobe"]);
    expect(controller.cycle).toBe(30);

    onPreset.mockClear();
    for (let i = 0; i < 5; i++) {
      controller.nextPreset();
      await settle();
    }
    expect(onPreset).toHaveBeenCalledTimes(5);
    expect(onPreset.mock.calls.every(([name]) => name === "calm")).toBe(true);
  });

  it("are all re-read when storage was cleared", async () => {
    localStorage.setItem(BLOCKED_KEY, JSON.stringify(["a"]));
    const { controller } = await create([entry("a"), entry("b")]);
    expect(controller.blockedNames()).toEqual(["a"]);
    localStorage.clear();
    controller.syncStoredPreferences(null);
    expect(controller.blockedNames()).toEqual([]);
    expect(controller.cycle).toBe("auto");
  });
});

describe("importPreferences", () => {
  it("leaves the favorites alone when the backup is rejected halfway", async () => {
    localStorage.setItem(FAVORITES_KEY, JSON.stringify(["mine"]));
    const { controller } = await create([entry("a")]);
    const backup = JSON.stringify({
      format: "sonarune-visualizer-preferences",
      version: 1,
      favorites: ["theirs"],
      blocked: "not a list",
    });
    expect(() => controller.importPreferences(backup)).toThrow(m.visualizer_backup_error_entries());
    expect(controller.favoriteNames()).toEqual(["mine"]);

    // The next star saves the user's own favorites, not the rejected ones.
    controller.toggleFavorite("a");
    expect(storedNames(FAVORITES_KEY)).toEqual(["mine", "a"]);
  });
});
