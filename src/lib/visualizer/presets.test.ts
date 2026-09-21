import { beforeEach, describe, expect, it, vi } from "vitest";

// Remote presets are fetched and pinned in Rust (`fetch_weekly_preset`); mock
// the command bridge and check what the frontend does with its outcomes.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import { m } from "$lib/paraglide/messages";
import {
  blankPreset,
  PresetFetchError,
  PresetRotation,
  resolvePreset,
  unwrapPresets,
  type PresetEntry,
} from "./presets";

const invokeMock = vi.mocked(invoke);

const WEEKLY_ORIGIN = "https://s3-us-east-2.amazonaws.com/butterchurn-presets/";
const weeklyUrl = (digit: string) => `${WEEKLY_ORIGIN}${digit.repeat(32)}.json`;

// resolvePreset keeps a module-level cache: every download test gets its own file.
let fileCounter = 0;
function uniqueWeekly(): { file: string; url: string } {
  fileCounter += 1;
  const file = `${fileCounter.toString(16).padStart(32, "0")}.json`;
  return { file, url: `${WEEKLY_ORIGIN}${file}` };
}

function entry(name: string, sourceUrl?: string): PresetEntry {
  return {
    name,
    preset: sourceUrl ? null : { baseVals: {} },
    sourceUrl,
    pack: "test",
    author: "Unknown",
    intensity: "medium",
    complexity: "light",
    priority: 0,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe("unwrapPresets", () => {
  const RECORD = { "Geiss - Desert Rose": { baseVals: {} } };

  it("reads a UMD pack whose export is a function carrying getPresets", () => {
    // The real shape of butterchurn-presets/lib/*.min.js. `typeof` is
    // "function", and an object-only check made every pack resolve to {} in
    // production builds -- "0 of 0 presets", and a blank visualizer.
    const fn = () => undefined;
    (fn as unknown as { getPresets: () => unknown }).getPresets = () => RECORD;
    expect(unwrapPresets({ default: fn as never })).toEqual(RECORD);
  });

  it("reads getPresets off the module namespace", () => {
    expect(unwrapPresets({ getPresets: () => RECORD })).toEqual(RECORD);
  });

  it("reads an object default that carries getPresets", () => {
    expect(unwrapPresets({ default: { getPresets: () => RECORD } })).toEqual(RECORD);
  });

  it("reads a plain record default", () => {
    expect(unwrapPresets({ default: RECORD })).toEqual(RECORD);
  });

  it("yields nothing for a module that exports neither", () => {
    expect(unwrapPresets({})).toEqual({});
  });
});

describe("resolvePreset", () => {
  it("returns a local preset as-is", async () => {
    const local = entry("local");
    await expect(resolvePreset(local, { allowRemote: false })).resolves.toBe(local.preset);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("never downloads a remote preset without the opt-in", async () => {
    const remote = entry("remote", uniqueWeekly().url);
    const failure = resolvePreset(remote, { allowRemote: false });
    await expect(failure).rejects.toBeInstanceOf(PresetFetchError);
    // Not permanent, so the controller never quarantines the entry for it.
    await expect(failure).rejects.toMatchObject({ permanent: false });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("asks Rust for the pinned file once opted in, then serves it from the cache", async () => {
    const { file, url } = uniqueWeekly();
    invokeMock.mockResolvedValueOnce(JSON.stringify({ baseVals: { decay: 0.9 } }) as never);
    const remote = entry("remote", url);

    await expect(resolvePreset(remote, { allowRemote: true })).resolves.toEqual({
      baseVals: { decay: 0.9 },
    });
    expect(invokeMock).toHaveBeenCalledWith("fetch_weekly_preset", { file });

    await expect(resolvePreset(remote, { allowRemote: true })).resolves.toEqual({
      baseVals: { decay: 0.9 },
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("refuses anything but a Weekly file name without calling Rust", async () => {
    const urls = [
      "https://evil.example/butterchurn-presets/" + "a".repeat(32) + ".json",
      `${WEEKLY_ORIGIN}${"A".repeat(32)}.json`,
      `${WEEKLY_ORIGIN}../${"a".repeat(32)}.json`,
      `${WEEKLY_ORIGIN}${"a".repeat(32)}.json?x=1`,
    ];
    for (const url of urls) {
      await expect(resolvePreset(entry("odd", url), { allowRemote: true })).rejects.toMatchObject({
        name: "PresetFetchError",
        permanent: true,
        message: m.visualizer_preset_error_untrusted(),
      });
    }
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it.each([
    ["preset:changed", true, () => m.visualizer_preset_error_changed()],
    ["preset:not-listed", true, () => m.visualizer_preset_error_unlisted()],
    ["preset:invalid-name", true, () => m.visualizer_preset_error_unlisted()],
    ["preset:http:404", true, () => m.visualizer_preset_error_http({ status: 404 })],
    ["preset:http:503", false, () => m.visualizer_preset_error_http({ status: 503 })],
    ["preset:too-large", true, () => m.visualizer_preset_error_too_large()],
    ["preset:undecodable", true, () => m.visualizer_preset_error_encoding()],
    [
      "preset:network:operation timed out",
      false,
      () => m.visualizer_preset_error_download({ message: "operation timed out" }),
    ],
    [
      "network error: builder error",
      false,
      () => m.visualizer_preset_error_download({ message: "network error: builder error" }),
    ],
  ])("classifies the command error %s (permanent: %s)", async (wire, permanent, message) => {
    const { url } = uniqueWeekly();
    const remote = entry("remote", url);
    invokeMock.mockRejectedValueOnce(wire);
    const failure = resolvePreset(remote, { allowRemote: true });
    await expect(failure).rejects.toBeInstanceOf(PresetFetchError);
    await expect(failure).rejects.toMatchObject({ permanent, message: message() });

    // A failure is never cached: the next attempt asks again.
    invokeMock.mockResolvedValueOnce(JSON.stringify({ baseVals: {} }) as never);
    await expect(resolvePreset(remote, { allowRemote: true })).resolves.toEqual({ baseVals: {} });
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it.each([
    ["not json", () => m.visualizer_preset_error_json()],
    [JSON.stringify({ shapes: [] }), () => m.visualizer_preset_error_schema()],
    [JSON.stringify([1, 2]), () => m.visualizer_preset_error_schema()],
    ["null", () => m.visualizer_preset_error_schema()],
    [" ".repeat(2 * 1024 * 1024 + 1), () => m.visualizer_preset_error_too_large()],
  ])("still checks the verified text (%#)", async (body, message) => {
    const { url } = uniqueWeekly();
    invokeMock.mockResolvedValueOnce(body as never);
    await expect(resolvePreset(entry("remote", url), { allowRemote: true })).rejects.toMatchObject(
      { name: "PresetFetchError", permanent: true, message: message() },
    );
  });

  it("drops the outcome of a load aborted while Rust was still fetching", async () => {
    const { url } = uniqueWeekly();
    const remote = entry("remote", url);
    const controller = new AbortController();
    let finish!: (body: string) => void;
    invokeMock.mockReturnValueOnce(new Promise<string>((resolve) => (finish = resolve)) as never);

    const pending = resolvePreset(remote, { allowRemote: true, signal: controller.signal });
    controller.abort();
    finish(JSON.stringify({ baseVals: {} }));
    const reason = await pending.catch((error: unknown) => error);
    expect(reason).toBe(controller.signal.reason);
    expect(reason).not.toBeInstanceOf(PresetFetchError);

    // Nothing was cached for it.
    invokeMock.mockResolvedValueOnce(JSON.stringify({ baseVals: { zoom: 1 } }) as never);
    await expect(resolvePreset(remote, { allowRemote: true })).resolves.toEqual({
      baseVals: { zoom: 1 },
    });
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("does not turn a failure after an abort into a quarantine-worthy error", async () => {
    const { url } = uniqueWeekly();
    const controller = new AbortController();
    let fail!: (error: string) => void;
    invokeMock.mockReturnValueOnce(new Promise<string>((_, reject) => (fail = reject)) as never);

    const pending = resolvePreset(entry("remote", url), {
      allowRemote: true,
      signal: controller.signal,
    });
    controller.abort();
    fail("preset:changed");
    await expect(pending).rejects.toBe(controller.signal.reason);
  });

  it("does not start a download for an already aborted load", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      resolvePreset(entry("remote", uniqueWeekly().url), {
        allowRemote: true,
        signal: controller.signal,
      }),
    ).rejects.toBe(controller.signal.reason);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe("Weekly preset pins", () => {
  // The manifest Rust compiles in (scripts/pin-weekly-presets.mjs writes it).
  const manifestSources = import.meta.glob<string>("/src-tauri/src/weekly_presets.json", {
    query: "?raw",
    import: "default",
    eager: true,
  });

  it("pin every Weekly preset the installed package lists", async () => {
    const [manifestRaw] = Object.values(manifestSources);
    expect(manifestRaw).toBeTypeOf("string");
    const pins = JSON.parse(manifestRaw) as Record<string, string>;

    const module = (await import("butterchurn-presets-weekly")) as {
      default?: Record<string, unknown>;
    };
    const urls = Object.values(module.default ?? {}).filter(
      (value): value is string => typeof value === "string",
    );
    expect(urls.length).toBeGreaterThan(0);

    // A failure here means the package changed: re-run the pin script.
    const unpinned = urls.filter(
      (url) => !url.startsWith(WEEKLY_ORIGIN) || !(url.slice(WEEKLY_ORIGIN.length) in pins),
    );
    expect(unpinned).toEqual([]);
    for (const [file, hash] of Object.entries(pins)) {
      expect(file).toMatch(/^[a-f0-9]{32}\.json$/);
      expect(hash).toMatch(/^[a-f0-9]{64}$/);
    }
  });
});

describe("PresetRotation remote entries", () => {
  const picks = (rotation: PresetRotation, count: number) =>
    new Set(Array.from({ length: count }, () => rotation.next()?.name));

  it("leaves remote entries out until they are enabled", () => {
    const rotation = new PresetRotation([entry("local"), entry("remote", weeklyUrl("c"))]);
    expect(picks(rotation, 20)).toEqual(new Set(["local"]));

    rotation.setRemoteEnabled(true);
    expect(picks(rotation, 20).has("remote")).toBe(true);

    rotation.setRemoteEnabled(false);
    expect(picks(rotation, 20)).toEqual(new Set(["local"]));
  });
});

describe("PresetRotation.localFallback", () => {
  it("finds a local preset when favorites-only leaves only online favorites", () => {
    const rotation = new PresetRotation([entry("remote", weeklyUrl("d")), entry("local")]);
    rotation.setFavorites(new Set(["remote"]));
    rotation.setFavoritesOnly(true);
    expect(rotation.next()).toBeNull();
    expect(rotation.localFallback()?.name).toBe("local");
  });

  it("prefers unblocked entries, then blocked ones, never quarantined or remote", () => {
    const rotation = new PresetRotation([
      entry("remote", weeklyUrl("e")),
      entry("broken"),
      entry("disliked"),
      entry("fine"),
    ]);
    rotation.setQuarantined(new Set(["broken"]));
    rotation.setBlocked(new Set(["disliked"]));
    expect(rotation.localFallback()?.name).toBe("fine");

    rotation.setBlocked(new Set(["disliked", "fine"]));
    expect(rotation.next()).toBeNull();
    expect(rotation.localFallback()?.name).toBe("disliked");

    rotation.setQuarantined(new Set(["broken", "disliked", "fine"]));
    expect(rotation.localFallback()).toBeNull();
  });
});

describe("blankPreset", () => {
  it("is fresh, data-only and carries no equation or shader code", () => {
    expect(blankPreset()).toEqual({
      baseVals: {},
      shapes: [],
      waves: [],
      init_eqs_str: "",
      frame_eqs_str: "",
      pixel_eqs_str: "",
      warp: "",
      comp: "",
    });
    expect(blankPreset()).not.toBe(blankPreset());
  });
});
