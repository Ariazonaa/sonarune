import { describe, expect, it } from "vitest";
import { m } from "$lib/paraglide/messages";
import { updateErrorMessage } from "./updateErrors";

describe("updateErrorMessage", () => {
  // The same list `error_codes_are_stable` pins on the Rust side. A code that
  // loses its case here silently falls through to the raw text, which is the
  // failure this table exists to catch.
  it.each([
    ["update:playing", () => m.settings_update_playing()],
    ["update:portable", () => m.settings_update_portable()],
    ["update:none", () => m.settings_update_none()],
    ["update:no-release", () => m.settings_update_error_no_release()],
    ["update:offline", () => m.settings_update_error_offline()],
    ["update:download", () => m.settings_update_error_download()],
    ["update:signature", () => m.settings_update_error_signature()],
    ["update:install", () => m.settings_update_error_install()],
    ["update:unavailable", () => m.settings_update_error_unavailable()],
  ])("%s reads as its own sentence", (code, expected) => {
    expect(updateErrorMessage(code)).toBe(expected());
    // Commands reject with a string, but an Error must map the same way.
    expect(updateErrorMessage(new Error(code))).toBe(expected());
  });

  it("shows anything unexpected as it arrived", () => {
    for (const text of ["network error: builder error", "update:", "update:Offline", ""]) {
      expect(updateErrorMessage(text)).toBe(m.error_generic({ message: text }));
    }
  });
});
