import { errorCause } from "#lib/appErrors.js";
import { m } from "#lib/paraglide/messages.js";

/**
 * Word a `player:error` event for the banner.
 *
 * Rust sends a code with the data that goes with it — the track, the output
 * device, the cause — and keeps the English for the log
 * (`src-tauri/src/player/errors.rs`, whose `the_banner_codes_are_stable` test
 * pins the same shapes). A cause the UI has no words for is shown in the
 * English detail it came with; a code it does not know, or a payload of
 * another shape, still says what arrived rather than nothing.
 */
export function playerErrorMessage(payload: unknown): string {
  // The sentence itself, as builds before the codes sent it.
  if (typeof payload === "string") return payload;
  const event = record(payload);
  if (!event) return m.error_generic({ message: String(payload) });
  const cause = causeMessage(event.cause);
  const outputMissing = record(event.cause)?.kind === "noOutputDevice";
  const device = text(event.device);
  switch (event.code) {
    case "cannotPlay":
      return m.player_error_cannot_play({ track: text(event.track) ?? "", cause });
    case "outputUnavailable":
      return outputMissing
        ? m.player_error_no_output_device()
        : m.player_error_output_unavailable({ cause });
    case "outputFailed":
      if (outputMissing) return m.player_error_no_output_device();
      return device
        ? m.player_error_output_failed_device({ device, cause })
        : m.player_error_output_failed({ cause });
    case "noFallbackOutput":
      if (outputMissing) return m.player_error_no_output_device();
      return device
        ? m.player_error_no_fallback_device({ device, cause })
        : m.player_error_no_fallback({ cause });
    default:
      return m.error_generic({ message: cause || JSON.stringify(payload) });
  }
}

/** The cause in words: ours where there are any, else its detail. */
function causeMessage(value: unknown): string {
  const cause = record(value);
  if (!cause) return "";
  // A cause carries the code a command error would (`ErrorCause::code`).
  if (typeof cause.code === "string" && typeof cause.detail === "string") return errorCause(cause);
  // One from before the codes: its kind.
  switch (cause.kind) {
    case "notConnected":
      return m.player_error_cause_not_connected();
    case "network":
      return m.player_error_cause_network();
    case "server":
      if (typeof cause.status === "number") {
        return m.player_error_cause_server({ status: String(cause.status) });
      }
      break;
    case "outputDeviceMissing":
      return m.player_error_cause_device_missing();
  }
  return text(cause.detail) ?? "";
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
}

function text(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}
