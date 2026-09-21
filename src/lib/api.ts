import { invoke } from "@tauri-apps/api/core";
import type {
  AlbumDetail,
  AlbumDto,
  ArtistDetail,
  ArtistDto,
  AudioDeviceSnapshot,
  CacheInfo,
  ConnectResult,
  DesktopSettings,
  DownloadRequest,
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
  SessionInfo,
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
  listTrustedCertificates: () => invoke<TrustedCert[]>("list_trusted_certificates"),
  forgetTrustedCertificate: (serverUrl: string, fingerprint: string) =>
    invoke<void>("forget_trusted_certificate", { serverUrl, fingerprint }),
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
  getDownloads: () => invoke<DownloadTask[]>("get_downloads"),
  cancelDownload: (id: string) => invoke<void>("cancel_download", { id }),
  retryDownload: (id: string) => invoke<void>("retry_download", { id }),
  clearFinishedDownloads: () => invoke<void>("clear_finished_downloads"),
  openDownloadsFolder: () => invoke<void>("open_downloads_folder"),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  getArtists: (startIndex: number, limit: number) =>
    invoke<Page<ArtistDto>>("get_artists", { startIndex, limit }),
  getArtist: (artistId: string) => invoke<ArtistDetail>("get_artist", { artistId }),
  getSongs: (startIndex: number, limit: number) =>
    invoke<Page<TrackDto>>("get_songs", { startIndex, limit }),

  playAlbum: (albumId: string, startIndex = 0) =>
    invoke<void>("play_album", { albumId, startIndex }),
  enqueueAlbum: (albumId: string, next: boolean) =>
    invoke<void>("enqueue_album", { albumId, next }),
  enqueueTracks: (trackIds: string[], next: boolean) =>
    invoke<void>("enqueue_tracks", { trackIds, next }),
  search: (term: string) => invoke<SearchResults>("search", { term }),
  searchPage: (term: string, kind: SearchKind, startIndex: number, limit: number) =>
    invoke<SearchPage>("search_page", { term, kind, startIndex, limit }),
  queueRemove: (index: number, itemId: string) =>
    invoke<void>("queue_remove", { index, itemId }),
  queueJump: (index: number) => invoke<void>("queue_jump", { index }),
  queueMove: (from: number, to: number) => invoke<void>("queue_move", { from, to }),
  playerToggle: () => invoke<void>("player_toggle"),
  playerNext: () => invoke<void>("player_next"),
  playerPrev: () => invoke<void>("player_prev"),
  playerSeek: (positionMs: number) => invoke<void>("player_seek", { positionMs }),
  playerSetVolume: (volume: number) => invoke<void>("player_set_volume", { volume }),
  getPlayerState: () => invoke<PlayerState>("get_player_state"),
  getWaveform: (itemId: string) => invoke<number[] | null>("get_waveform", { itemId }),
  getQueue: () => invoke<QueueSnapshot>("get_queue"),
  getAudioSettings: () => invoke<DspParams>("get_audio_settings"),
  setAudioSettings: (params: DspParams) => invoke<void>("set_audio_settings", { params }),
  getPlaybackSettings: () => invoke<PlaybackSettings>("get_playback_settings"),
  setPlaybackSettings: (settings: PlaybackSettings) =>
    invoke<void>("set_playback_settings", { settings }),
  listAudioDevices: () => invoke<AudioDeviceSnapshot>("list_audio_devices"),
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
  checkLibraryNow: () => invoke<void>("check_library_now"),
  checkServer: () => invoke<boolean>("check_server"),
  getLyrics: (itemId: string) => invoke<LyricsDto>("get_lyrics", { itemId }),
  playInstantMix: (itemId: string) => invoke<void>("play_instant_mix", { itemId }),
  setSleepTimer: (minutes: number | null, endOfTrack: boolean, fadeSeconds = 30) =>
    invoke<void>("set_sleep_timer", { minutes, endOfTrack, fadeSeconds }),
  /** Arm "stop after this track / album" on a queue entry, or on what is
   *  playing when `itemId` is left out. `"off"` clears it. */
  playerSetStopAfter: (mode: import("./types").StopAfter, itemId: string | null = null) =>
    invoke<void>("player_set_stop_after", { mode, itemId }),
  getExtrasSettings: () => invoke<ExtrasSettings>("get_extras_settings"),
  setExtrasSettings: (settings: ExtrasSettings) =>
    invoke<void>("set_extras_settings", { settings }),
  exportSettings: () => invoke<string>("export_settings"),
  importSettings: (json: string) => invoke<string[]>("import_settings", { json }),

  /** Ask the release endpoint what the current version is (updater.rs). */
  checkForUpdate: () => invoke<UpdateInfo>("check_for_update"),
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
