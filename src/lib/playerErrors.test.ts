import { describe, expect, it } from "vitest";
import { m } from "#lib/paraglide/messages.js";
import { setLocale } from "#lib/paraglide/runtime.js";
import { playerErrorMessage } from "./playerErrors";
import type { PlayerErrorCause, PlayerErrorEvent } from "./types";

/** The shapes `the_banner_codes_are_stable` pins on the Rust side. */
function event(
  code: PlayerErrorEvent["code"],
  cause: PlayerErrorCause | null,
  data: Partial<Pick<PlayerErrorEvent, "track" | "device">> = {},
): PlayerErrorEvent {
  return { code, track: null, device: null, cause, ...data };
}

const decoder: PlayerErrorCause = { kind: "other", detail: "decode failed: unsupported codec" };
const noDevice: PlayerErrorCause = { kind: "noOutputDevice", detail: "no default output device" };

describe("playerErrorMessage", () => {
  it("words each code with its data", () => {
    expect(playerErrorMessage(event("cannotPlay", decoder, { track: "Intro" }))).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: decoder.detail }),
    );
    expect(playerErrorMessage(event("outputUnavailable", decoder))).toBe(
      m.player_error_output_unavailable({ cause: decoder.detail }),
    );
    expect(playerErrorMessage(event("outputFailed", decoder, { device: "USB DAC" }))).toBe(
      m.player_error_output_failed_device({ device: "USB DAC", cause: decoder.detail }),
    );
    // No device named: the system default.
    expect(playerErrorMessage(event("outputFailed", decoder))).toBe(
      m.player_error_output_failed({ cause: decoder.detail }),
    );
    expect(playerErrorMessage(event("noFallbackOutput", decoder, { device: "Headphones" }))).toBe(
      m.player_error_no_fallback_device({ device: "Headphones", cause: decoder.detail }),
    );
    expect(playerErrorMessage(event("noFallbackOutput", decoder))).toBe(
      m.player_error_no_fallback({ cause: decoder.detail }),
    );
  });

  it("puts the causes it knows into words of its own", () => {
    const cannotPlay = (cause: PlayerErrorCause) =>
      playerErrorMessage(event("cannotPlay", cause, { track: "Intro" }));
    expect(cannotPlay({ kind: "notConnected", detail: "not connected to a server" })).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: m.player_error_cause_not_connected() }),
    );
    expect(cannotPlay({ kind: "network", detail: "network error: timed out" })).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: m.player_error_cause_network() }),
    );
    expect(cannotPlay({ kind: "server", status: 503, detail: "server returned 503: busy" })).toBe(
      m.player_error_cannot_play({
        track: "Intro",
        cause: m.player_error_cause_server({ status: "503" }),
      }),
    );
    const missing: PlayerErrorCause = {
      kind: "outputDeviceMissing",
      detail: "the selected output device is not connected",
    };
    expect(playerErrorMessage(event("outputFailed", missing, { device: "USB DAC" }))).toBe(
      m.player_error_output_failed_device({
        device: "USB DAC",
        cause: m.player_error_cause_device_missing(),
      }),
    );
  });

  it("says plainly when there is no output device at all", () => {
    for (const code of ["outputUnavailable", "outputFailed", "noFallbackOutput"] as const) {
      expect(playerErrorMessage(event(code, noDevice, { device: "USB DAC" }))).toBe(
        m.player_error_no_output_device(),
      );
    }
  });

  it("shows the detail of a cause it has no words for", () => {
    // A kind from a newer build, or a server status that did not come.
    const unknown = { kind: "somethingNew", detail: "it broke" } as unknown as PlayerErrorCause;
    expect(playerErrorMessage(event("cannotPlay", unknown, { track: "Intro" }))).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: "it broke" }),
    );
    const bare = { kind: "server", detail: "server returned 500" } as PlayerErrorCause;
    expect(playerErrorMessage(event("cannotPlay", bare, { track: "Intro" }))).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: "server returned 500" }),
    );
  });

  it("falls back to the generic sentence for a code it does not know", () => {
    const later = { ...event("cannotPlay", decoder), code: "outputBusy" };
    expect(playerErrorMessage(later)).toBe(m.error_generic({ message: decoder.detail }));
    // Nothing to go by but the payload itself.
    const bare = { code: "outputBusy", track: null, device: null, cause: null };
    expect(playerErrorMessage(bare)).toBe(m.error_generic({ message: JSON.stringify(bare) }));
    expect(playerErrorMessage(null)).toBe(m.error_generic({ message: "null" }));
  });

  it("words a cause by the code a command error would carry", () => {
    // What `ErrorCause::of` sends since the codes (`appErrors.ts` words it).
    const crashed: PlayerErrorCause = { kind: "other", code: "decoderCrashed", detail: "the decoder crashed" };
    expect(playerErrorMessage(event("cannotPlay", crashed, { track: "Intro" }))).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: m.error_decoder_crashed() }),
    );
    const cpal: PlayerErrorCause = {
      kind: "other",
      code: "defaultOutputFailed",
      detail: "cannot open default output: The device is busy",
    };
    expect(playerErrorMessage(event("outputFailed", cpal))).toBe(
      m.player_error_output_failed({ cause: m.error_default_output_failed() }),
    );
    const busy: PlayerErrorCause = { kind: "server", code: "server", status: 503, detail: "server returned 503" };
    expect(playerErrorMessage(event("cannotPlay", busy, { track: "Intro" }))).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: m.player_error_cause_server({ status: "503" }) }),
    );
    // A code the UI has no words for still shows its detail.
    const odd: PlayerErrorCause = { kind: "other", code: "audio", detail: "cpal said no" };
    expect(playerErrorMessage(event("cannotPlay", odd, { track: "Intro" }))).toBe(
      m.player_error_cannot_play({ track: "Intro", cause: "cpal said no" }),
    );
  });

  it("shows a sentence sent as text as it arrived", () => {
    expect(playerErrorMessage("Audio output failed: busy")).toBe("Audio output failed: busy");
  });

  it("speaks the user's language", () => {
    setLocale("de", { reload: false });
    try {
      const signedOut: PlayerErrorCause = { kind: "notConnected", detail: "not connected to a server" };
      expect(playerErrorMessage(event("cannotPlay", signedOut, { track: "Intro" }))).toBe(
        "„Intro“ kann nicht abgespielt werden: nicht mit einem Server verbunden",
      );
      expect(playerErrorMessage(event("outputFailed", noDevice))).toBe("Kein Ausgabegerät verfügbar.");
    } finally {
      setLocale("en", { reload: false });
    }
  });
});

describe("the banner's messages", () => {
  const catalogs = import.meta.glob("/messages/*.json", { import: "default", eager: true }) as Record<
    string,
    Record<string, string>
  >;
  const placeholders = (message: string) => [...message.matchAll(/\{(\w+)\}/g)].map((p) => p[1]).sort();

  it("are in both languages, with the same placeholders", () => {
    const en = catalogs["/messages/en.json"];
    const de = catalogs["/messages/de.json"];
    const keys = Object.keys(en).filter((key) => key.startsWith("player_error_") || key.startsWith("error_"));
    expect(keys.length).toBeGreaterThan(0);
    for (const key of keys) {
      expect(de[key], key).toBeTypeOf("string");
      expect(placeholders(de[key]), key).toEqual(placeholders(en[key]));
    }
  });
});
