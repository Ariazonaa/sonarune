import { m } from "$lib/paraglide/messages";

/**
 * Turn a rejection from `check_for_update` / `install_update` into a sentence.
 *
 * Rust answers with `update:<code>` (`src-tauri/src/updater.rs`, whose
 * `error_codes_are_stable` test pins the same strings) and keeps the technical
 * detail in the log: the diagnostic export has it, the panel does not need it.
 * Anything that is not one of those codes is unexpected and is shown as it
 * arrived rather than swallowed.
 */
export function updateErrorMessage(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  switch (/^update:([a-z-]+)$/.exec(text)?.[1]) {
    case "playing":
      return m.settings_update_playing();
    case "portable":
      return m.settings_update_portable();
    case "none":
      return m.settings_update_none();
    case "no-release":
      return m.settings_update_error_no_release();
    case "offline":
      return m.settings_update_error_offline();
    case "download":
      return m.settings_update_error_download();
    case "signature":
      return m.settings_update_error_signature();
    case "install":
      return m.settings_update_error_install();
    case "unavailable":
      return m.settings_update_error_unavailable();
    default:
      return m.error_generic({ message: text });
  }
}
