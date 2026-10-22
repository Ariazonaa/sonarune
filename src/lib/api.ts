import { invoke } from "@tauri-apps/api/core";
import type {
  AlbumDetail,
  AlbumDto,
  ArtistDetail,
  ArtistDto,
  AudioDeviceSnapshot,
  AudioProfilesInfo,
  CacheInfo,
  ConnectResult,
  DesktopSettings,
  DownloadRequest,
  DownloadSnapshot,
  DownloadTask,
  DspParams,
  ExtrasSettings,
  GenreDto,
  LyricsDto,
  Page,
  PlaybackSettings,
  PlayerState,
  QueueSnapshot,
  RepeatMode,
  QuickConnectSession,
  SearchResults,
  SearchKind,
  SearchPage,
  ServerCheck,
  SessionInfo,
  TrackAnalysisInfo,
  TrackDto,
  TrackInfoDto,
  TrayLabels,
  TrustedCert,
  UpdateInfo,
} from "./types";

export const api = {
  restoreSession: () => invoke<SessionInfo | null>("restore_session"),
  connect: (input: {
    serverUrl: string;
    username: string;
    password: string;
    acceptInvalidCerts: boolean;
    trustFingerprint?: string;
  }) => invoke<ConnectResult>("connect", input),
  disconnect: () => invoke<void>("disconnect"),
  /** Where a damaged database was set aside at this start; null normally. */
  getDatabaseRecovery: () => invoke<string | null>("get_database_recovery"),
  listTrustedCertificates: () => invoke<TrustedCert[]>("list_trusted_certificates"),
  forgetTrustedCertificate: (serverUrl: string, fingerprint: string) =>
    invoke<void>("forget_trusted_certificate", { serverUrl, fingerprint }),
  /** Trust the certificate the running session's server now presents (after
   *  the user compared `fingerprint`, from `checkServer`) — no new sign-in. */
  trustSessionCertificate: (fingerprint: string) =>
    invoke<void>("trust_session_certificate", { fingerprint }),
  quickConnectStart: (serverUrl: string, acceptInvalidCerts: boolean) =>
    invoke<QuickConnectSession>("quick_connect_start", { serverUrl, acceptInvalidCerts }),
  quickConnectPoll: (serverUrl: string, acceptInvalidCerts: boolean, secret: string) =>
    invoke<SessionInfo | null>("quick_connect_poll", {
      serverUrl,
      acceptInvalidCerts,
      secret,
    }),

  getAlbums: (
    startIndex: number,
    limit: number,
    genreId: string | null = null,
    sort = "name",
    favoritesOnly = false,
    played = "all",
  ) =>
    invoke<Page<AlbumDto>>("get_albums", {
      startIndex,
      limit,
      genreId,
      sort,
      favoritesOnly,
      played,
    }),
  getAlbum: (albumId: string) => invoke<AlbumDetail>("get_album", { albumId }),
  getTrackInfo: (itemId: string) => invoke<TrackInfoDto>("get_track_info", { itemId }),
  queueDownloads: (requests: DownloadRequest[]) =>
    invoke<DownloadTask[]>("queue_downloads", { requests }),
  getDownloads: () => invoke<DownloadSnapshot>("get_downloads"),
  cancelDownload: (id: string) => invoke<void>("cancel_download", { id }),
  retryDownload: (id: string) => invoke<void>("retry_download", { id }),
  /** Retry every failed and cancelled download; how many were requeued. */
  retryFailedDownloads: () => invoke<number>("retry_failed_downloads"),
  cancelAllDownloads: () => invoke<void>("cancel_all_downloads"),
  clearFinishedDownloads: () => invoke<void>("clear_finished_downloads"),
  openDownloadsFolder: () => invoke<void>("open_downloads_folder"),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  getArtists: (startIndex: number, limit: number) =>
    invoke<Page<ArtistDto>>("get_artists", { startIndex, limit }),
  getArtist: (artistId: string) => invoke<ArtistDetail>("get_artist", { artistId }),
  getSongs: (startIndex: number, limit: number) =>
    invoke<Page<TrackDto>>("get_songs", { startIndex, limit }),
  /** Tracks by id, in the order asked for; ids the server no longer has are left out. */
  getTracks: (trackIds: string[]) => invoke<TrackDto[]>("get_tracks", { trackIds }),

  playAlbum: (albumId: string, startIndex = 0) =>
    invoke<void>("play_album", { albumId, startIndex }),
  /** A track in its album from that track on, or on its own without one. */
  playTrackInAlbum: (trackId: string, albumId: string | null) =>
    invoke<void>("play_track_in_album", { trackId, albumId }),
  enqueueAlbum: (albumId: string, next: boolean) =>
    invoke<void>("enqueue_album", { albumId, next }),
  enqueueTracks: (trackIds: string[], next: boolean) =>
    invoke<void>("enqueue_tracks", { trackIds, next }),
  search: (term: string) => invoke<SearchResults>("search", { term }),
  searchPage: (term: string, kind: SearchKind, startIndex: number, limit: number) =>
    invoke<SearchPage>("search_page", { term, kind, startIndex, limit }),
  /** Remove the entry at stored `index`, if that slot still holds it: by
   *  `entryId`, which tells two copies of a track apart; by `itemId` for an
   *  entry from a build before entry ids. */
  queueRemove: (index: number, itemId: string, entryId: string | null = null) =>
    invoke<void>("queue_remove", { index, itemId, entryId }),
  queueJump: (index: number) => invoke<void>("queue_jump", { index }),
  queueMove: (from: number, to: number) => invoke<void>("queue_move", { from, to }),
  playerToggle: () => invoke<void>("player_toggle"),
  playerNext: () => invoke<void>("player_next"),
  playerPrev: () => invoke<void>("player_prev"),
  playerSeek: (positionMs: number) => invoke<void>("player_seek", { positionMs }),
  playerSetVolume: (volume: number) => invoke<void>("player_set_volume", { volume }),
  getPlayerState: () => invoke<PlayerState>("get_player_state"),
  getWaveform: (itemId: string) => invoke<number[] | null>("get_waveform", { itemId }),
  getTrackAnalysis: (itemId: string) =>
    invoke<TrackAnalysisInfo | null>("get_track_analysis", { itemId }),
  getQueue: () => invoke<QueueSnapshot>("get_queue"),
  getAudioSettings: () => invoke<DspParams>("get_audio_settings"),
  setAudioSettings: (params: DspParams) => invoke<void>("set_audio_settings", { params }),
  getPlaybackSettings: () => invoke<PlaybackSettings>("get_playback_settings"),
  /** Resolves to the settings that apply: an output device the player
   *  cannot open is refused and the previous one kept. */
  setPlaybackSettings: (settings: PlaybackSettings) =>
    invoke<PlaybackSettings>("set_playback_settings", { settings }),
  listAudioDevices: () => invoke<AudioDeviceSnapshot>("list_audio_devices"),
  /** After a fallback: play through the configured device again. */
  restoreOutputDevice: () => invoke<AudioDeviceSnapshot>("restore_output_device"),
  getAudioProfiles: () => invoke<AudioProfilesInfo>("get_audio_profiles"),
  setAudioProfilesEnabled: (enabled: boolean) =>
    invoke<AudioProfilesInfo>("set_audio_profiles_enabled", { enabled }),
  /** `deviceId`: the profile's `OutputDevice.id`. */
  deleteAudioProfile: (deviceId: string) =>
    invoke<AudioProfilesInfo>("delete_audio_profile", { device: deviceId }),
  getDesktopSettings: () => invoke<DesktopSettings>("get_desktop_settings"),
  setDesktopSettings: (settings: DesktopSettings) =>
    invoke<DesktopSettings>("set_desktop_settings", { settings }),
  setTrayLabels: (labels: TrayLabels) => invoke<void>("set_tray_labels", { labels }),
  getCacheInfo: () => invoke<CacheInfo>("get_cache_info"),
  clearCoverCache: () => invoke<CacheInfo>("clear_cover_cache"),
  exportDiagnostics: () => invoke<string>("export_diagnostics"),
  /**
   * JSON text of one pinned Butterchurn Weekly preset (`<32 hex>.json`),
   * fetched and SHA-256-verified in Rust. Rejects with `preset:<code>…`
   * (see `commands_visualizer.rs`); only call it behind the online-preset
   * opt-in.
   */
  fetchWeeklyPreset: (file: string) => invoke<string>("fetch_weekly_preset", { file }),
  playerSetShuffleMode: (shuffleMode: import("./types").ShuffleMode) =>
    invoke<void>("player_set_shuffle_mode", { shuffleMode }),
  playerSetRepeat: (repeat: RepeatMode) => invoke<void>("player_set_repeat", { repeat }),
  queueClear: () => invoke<void>("queue_clear"),
  queueRemovePlayed: () => invoke<void>("queue_remove_played"),
  queueRemoveDuplicates: () => invoke<void>("queue_remove_duplicates"),
  queueUndo: () => invoke<void>("queue_undo"),
  /** Pokes the library watcher; resolves to how many library changes it has
   *  found since start (`library:changed` carries the same count). */
  checkLibraryNow: () => invoke<number>("check_library_now"),
  checkServer: () => invoke<ServerCheck>("check_server"),
  getLyrics: (itemId: string) => invoke<LyricsDto>("get_lyrics", { itemId }),
  playInstantMix: (itemId: string) => invoke<void>("play_instant_mix", { itemId }),
  playerSetSpeed: (speed: number, preservePitch: boolean) =>
    invoke<void>("player_set_speed", { speed, preservePitch }),
  setSleepTimer: (minutes: number | null, endOfTrack: boolean, fadeSeconds = 30) =>
    invoke<void>("set_sleep_timer", { minutes, endOfTrack, fadeSeconds }),
  /** Arm "stop after this track / album" on a queue entry (its `entryId`,
   *  with the `itemId` for an entry from a build before entry ids), or on
   *  what is playing when both are left out. `"off"` clears it. */
  playerSetStopAfter: (
    mode: import("./types").StopAfter,
    itemId: string | null = null,
    entryId: string | null = null,
  ) => invoke<void>("player_set_stop_after", { mode, itemId, entryId }),
  getExtrasSettings: () => invoke<ExtrasSettings>("get_extras_settings"),
  setExtrasSettings: (settings: ExtrasSettings) =>
    invoke<void>("set_extras_settings", { settings }),
  exportSettings: () => invoke<string>("export_settings"),
  importSettings: (json: string) => invoke<string[]>("import_settings", { json }),

  /** Ask the release endpoint what the current version is (updater.rs). */
  checkForUpdate: () => invoke<UpdateInfo>("check_for_update"),
  /** The last answer a check got (the background one or a check from
   *  Settings), without asking the endpoint; null before there is one. */
  getKnownUpdate: () => invoke<UpdateInfo | null>("get_known_update"),
  /**
   * Download the update, verify its signature and start the installer. On
   * Windows this never resolves: the app is closed by the installer. Rejects
   * with `update:playing` (a track is running), `update:portable` (not an
   * installed copy) or `update:none`.
   */
  installUpdate: () => invoke<void>("install_update"),

  getGenres: () => invoke<GenreDto[]>("get_genres"),
  /** Index of the first item whose name sorts at/after `letter`. */
  letterIndex: (kind: "albums" | "artists", letter: string, genreId: string | null = null) =>
    invoke<number>("letter_index", { kind, letter, genreId }),
  getSimilarArtists: (artistId: string, limit = 12) =>
    invoke<ArtistDto[]>("get_similar_artists", { artistId, limit }),
};

/**
 * URL for the authenticated cover-art protocol (see src-tauri/src/proto.rs).
 * Windows WebView2 exposes custom schemes as http://{scheme}.localhost/.
 * TODO(cross-platform): macOS/Linux use `jfimg://` directly.
 */
export function coverUrl(
  itemId: string | null,
  tag: string | null,
  size = 300,
): string | null {
  if (!itemId || !tag) return null;
  return `http://jfimg.localhost/${itemId}/${tag}/${size}`;
}

/** `m:ss`, or `h:mm:ss` from one hour on (queue/album totals, long mixes).
 *  Anything that isn't a positive, finite duration reads as 0:00. */
export function formatDuration(ms: number): string {
  const totalSeconds = Number.isFinite(ms) && ms > 0 ? Math.round(ms / 1000) : 0;
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = (totalSeconds % 60).toString().padStart(2, "0");
  return hours > 0
    ? `${hours}:${minutes.toString().padStart(2, "0")}:${seconds}`
    : `${minutes}:${seconds}`;
}
