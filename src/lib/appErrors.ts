import { m } from "#lib/paraglide/messages.js";

/**
 * A command's rejection as Rust sends it: `ErrorReport` in
 * `src-tauri/src/error.rs`, whose tests pin the same shapes. The code is what
 * the UI words; the detail is the English the error was raised with, which
 * is what every build before the codes sent instead, and what is shown for a
 * code there are no words for.
 */
export interface AppErrorReport {
  code: string;
  /** The HTTP status of a server error. */
  status?: number;
  /** The one value the sentence names (an address, a sample rate). */
  arg?: string;
  /** Why, in the words of whatever failed (English): the server's message,
   *  a network error's cause, the I/O error after a known prefix. */
  reason?: string;
  detail: string;
}

/** Any rejection as a report. A plain string (as builds before the codes
 *  sent, or a test mock) or an exception thrown in the WebView itself has no
 *  code: its text is the detail. */
export function errorReport(error: unknown): AppErrorReport {
  if (typeof error === "object" && error !== null && !(error instanceof Error)) {
    const value = error as Record<string, unknown>;
    if (typeof value.code === "string" && typeof value.detail === "string") {
      return {
        code: value.code,
        status: typeof value.status === "number" ? value.status : undefined,
        arg: typeof value.arg === "string" ? value.arg : undefined,
        reason: typeof value.reason === "string" ? value.reason : undefined,
        detail: value.detail,
      };
    }
    // Some other object: what it holds says more than "[object Object]".
    try {
      return { code: "", detail: JSON.stringify(error) };
    } catch {
      // A cycle; fall through.
    }
  }
  return { code: "", detail: error instanceof Error ? error.message : String(error) };
}

/** The English text of an error, as it arrived before the codes: for the
 *  areas that parse their own codes out of it (`update:…`, `preset:…`,
 *  `sonic:…`) and for the log. */
export function errorDetail(error: unknown): string {
  return errorReport(error).detail;
}

/** Whether the server said the item is not there (or not visible to this
 *  account): a page then drops its cached copy. */
export function isNotFound(error: unknown): boolean {
  return errorReport(error).status === 404;
}

const CAUSES: Record<string, (report: AppErrorReport) => string> = {
  network: () => m.player_error_cause_network(),
  notConnected: () => m.player_error_cause_not_connected(),
  database: () => m.error_database(),
  credentialStore: () => m.error_credential_store(),
  playerGone: () => m.error_player_gone(),
  decoderCrashed: () => m.error_decoder_crashed(),
  noOutputDevice: () => m.error_no_output_device(),
  outputDeviceMissing: () => m.player_error_cause_device_missing(),
  outputDeviceUnusable: () => m.error_output_device_unusable(),
  defaultOutputFailed: () => m.error_default_output_failed(),
  outputOpenFailed: () => m.error_output_open_failed(),
  outputListFailed: () => m.error_output_list_failed(),
  threadFailed: () => m.error_thread_failed(),
  sampleRate: ({ arg }) => m.error_sample_rate({ rate: arg ?? "?" }),
  undecodable: () => m.error_undecodable(),
  streamBuffer: () => m.error_stream_buffer(),
  openCancelled: () => m.error_open_cancelled(),
  foreignQueueEntry: () => m.error_foreign_queue_entry(),
  streamRequest: () => m.error_stream_request(),
  albumEmpty: () => m.error_album_empty(),
  trackMissing: () => m.error_track_missing(),
  mixEmpty: () => m.error_mix_empty(),
  collectionEmpty: () => m.error_collection_empty(),
  playlistEmpty: () => m.error_playlist_empty(),
  selectionEmpty: () => m.error_selection_empty(),
  smartViewEmpty: () => m.error_smart_view_empty(),
  sonicSameTrack: () => m.error_sonic_same_track(),
  wrongCredentials: () => m.error_wrong_credentials(),
  serverRedirect: ({ arg }) => m.error_server_redirect({ address: arg ?? "" }),
  serverAddress: () => m.error_server_address(),
  invalidFingerprint: () => m.error_invalid_fingerprint(),
  otherAccount: () => m.error_other_account(),
  certificateChanged: () => m.error_certificate_changed(),
  unreachable: () => m.error_unreachable(),
  noServerId: () => m.error_no_server_id(),
  tlsSetup: () => m.error_tls_setup(),
  badResponse: () => m.error_bad_response(),
  responseTooLarge: () => m.error_response_too_large(),
  playlistEntryMissing: () => m.error_playlist_entry_missing(),
  noMediaSource: () => m.error_no_media_source(),
  titleEmpty: () => m.error_title_empty(),
  yearRange: () => m.error_year_range(),
  playlistNameEmpty: () => m.error_playlist_name_empty(),
  nothingSelected: () => m.error_nothing_selected(),
  samePlaylist: () => m.error_same_playlist(),
  settingsUnsaved: () => m.error_settings_unsaved(),
  settingsExport: () => m.error_settings_export(),
  notSettingsExport: () => m.error_not_settings_export(),
  downloadsFolder: () => m.error_downloads_folder(),
  outsideDownloads: () => m.error_outside_downloads(),
  fileMissing: () => m.error_file_missing(),
  fileBrowser: () => m.error_file_browser(),
  downloadMissing: () => m.error_download_missing(),
  diagnosticsFailed: () => m.error_diagnostics_failed(),
  coverCache: () => m.error_cover_cache(),
  taskFailed: () => m.error_task_failed(),
};

/** The codes there are words for (the contract test checks them against
 *  Rust's). */
export const WORDED_CODES: readonly string[] = [...Object.keys(CAUSES), "server"];

/**
 * What went wrong, as a clause in the user's language — the part after
 * "Something went wrong:" (`error_generic`), and the cause of a player error.
 * A code there are no words for (`other`, `audio`, an area's own code, a new
 * code from a newer backend) shows its detail.
 */
export function errorCause(error: unknown): string {
  const report = errorReport(error);
  const cause = clause(report);
  if (cause === null) return report.detail;
  // Where the reason is the point — what the server refused, why the
  // connection failed, which I/O error stopped an export — it follows.
  return report.reason && WITH_REASON.has(report.code)
    ? m.error_with_reason({ cause, reason: report.reason })
    : cause;
}

/** The codes whose reason is worth reading. Not the output and decoder
 *  codes: their reason is cpal's or a decoder's text, which the log has. */
const WITH_REASON: ReadonlySet<string> = new Set([
  "network",
  "server",
  "database",
  "credentialStore",
  "tlsSetup",
  "streamBuffer",
  "settingsUnsaved",
  "settingsExport",
  "downloadsFolder",
  "diagnosticsFailed",
  "coverCache",
  "fileBrowser",
  "taskFailed",
]);

function clause(report: AppErrorReport): string | null {
  // A server error of a kind the UI has words of its own for (a wrong
  // password, a redirect) has its own code; any other one says its status.
  if (report.code === "server") {
    return report.status === undefined ? null : m.player_error_cause_server({ status: String(report.status) });
  }
  const cause = Object.hasOwn(CAUSES, report.code) ? CAUSES[report.code] : undefined;
  return cause ? cause(report) : null;
}

/** A command's rejection as a sentence, for the error banner and wherever an
 *  error is shown on its own. */
export function errorMessage(error: unknown): string {
  return m.error_generic({ message: errorCause(error) });
}
