// Mirrors of the Rust DTOs (src-tauri/src/api/types.rs, player.rs, commands.rs).

import type { AppErrorReport } from "#lib/appErrors.js";

export interface SessionInfo {
  serverUrl: string;
  username: string;
  userId: string;
  /** Whether this user may permanently delete media on the server. */
  canDelete: boolean;
  /** Whether this user (an admin) may edit item metadata on the server. */
  canEdit: boolean;
}

/** Current server values for the metadata editor form + diff preview. */
export interface EditableMetadata {
  id: string;
  name: string;
  albumArtists: string[];
  year: number | null;
  genres: string[];
  trackNumber: number | null;
  discNumber: number | null;
  /** Track (Audio) items show the track/disc fields; albums hide them. */
  isAudio: boolean;
}

/** The whitelisted fields the metadata editor changed. A field left out stays
 *  as it is on the server; `null` clears a number. */
export interface MetadataEdits {
  name?: string;
  albumArtists?: string[];
  year?: number | null;
  genres?: string[];
  trackNumber?: number | null;
  discNumber?: number | null;
}

/** Edits for many tracks at once. A field left out stays as it is on every
 *  one of them — see `BulkMetadataEdits` in src-tauri/src/api/types.rs. */
export interface BulkMetadataEdits {
  albumArtists?: string[];
  genres?: string[];
  /** Merge the genres into what each track already has instead of replacing. */
  addGenres?: boolean;
  year?: number;
  /** Empty the year; wins over `year`. */
  clearYear?: boolean;
}

export interface BulkMetadataResult {
  changed: number;
  failed: number;
  /** Never tried: the run stopped once the server could not be reached. */
  skipped: number;
  /** The first failure; the others are only counted. */
  error: AppErrorReport | null;
}

export interface ArtistDto {
  id: string;
  name: string;
  imageTag: string | null;
  imageBlurHash: string | null;
  overview: string | null;
  /** Asked for explicitly where it is shown: the Artists grid, the artist page. */
  isFavorite: boolean;
}

export interface ArtistDetail {
  artist: ArtistDto;
  albums: AlbumDto[];
  appearsOn: AlbumDto[];
  topSongs: TrackDto[];
}

export interface QuickConnectSession {
  secret: string;
  code: string;
}

/** Result of a `connect` attempt (mirrors the Rust `ConnectResult` enum). */
export type ConnectResult =
  | { status: "connected"; session: SessionInfo }
  | { status: "certUntrusted"; fingerprint: string };

/** A pinned server certificate (mirrors the Rust `store::TrustedCert`). */
export interface TrustedCert {
  serverUrl: string;
  fingerprint: string;
  /** Unix milliseconds; null for pins stored before dates were recorded. */
  pinnedAt: number | null;
}

/**
 * `session:ended` — the backend dropped a session that only a new sign-in
 * brings back (mirrors the Rust `commands::SessionEnded`). `rejected`: the
 * server refused the stored credentials, or there are none to renew a dead
 * token with (Quick Connect). `moved`: the stored address now redirects to
 * https, and `serverUrl` is the new one.
 */
export interface SessionEnded {
  reason: "rejected" | "moved";
  serverUrl: string;
  username: string;
}

/** Result of `check_server` (mirrors the Rust `commands::ServerCheck`). */
export interface ServerCheck {
  /** Whether the session reaches the server — the offline banner. */
  online: boolean;
  /** The certificate the server presents when it is not trusted. */
  untrustedCertificate: string | null;
}

/** A linkable artist reference (navigate to /artist/{id}). */
export interface ArtistRef {
  id: string;
  name: string;
}

export interface GenreRef {
  id: string;
  name: string;
}

export interface AlbumDto {
  id: string;
  name: string;
  artist: string;
  year: number | null;
  imageTag: string | null;
  imageBlurHash: string | null;
  isFavorite: boolean;
  trackCount: number | null;
  artists: ArtistRef[];
}

export interface TrackDto {
  id: string;
  name: string;
  artist: string;
  album: string;
  albumId: string | null;
  /** The album's artist as the track is tagged (Jellyfin's AlbumArtist). */
  albumArtist?: string | null;
  indexNumber: number | null;
  discNumber: number | null;
  durationMs: number;
  imageItemId: string | null;
  imageTag: string | null;
  imageBlurHash: string | null;
  normalizationGain: number | null;
  /** The album's gain, when the server knows one (Jellyfin after 10.11). */
  albumNormalizationGain: number | null;
  isFavorite: boolean;
  playCount: number;
  artists: ArtistRef[];
  genres: GenreRef[];
}

/** Technical audio details of a track's file (the "Info" panel). */
export interface TrackInfoDto {
  codec: string | null;
  bitrateKbps: number | null;
  sampleRateHz: number | null;
  bitDepth: number | null;
  channels: number | null;
  container: string | null;
  sizeBytes: number | null;
  path: string | null;
}

export interface Page<T> {
  items: T[];
  total: number;
}

export interface AlbumDetail {
  album: AlbumDto;
  tracks: TrackDto[];
}

export interface QueueTrack {
  itemId: string;
  name: string;
  artist: string;
  album: string;
  albumId: string | null;
  /** The album's artist as the track is tagged; null in queues from older
   *  builds. */
  albumArtist?: string | null;
  trackNumber: number | null;
  discNumber: number | null;
  durationMs: number;
  imageItemId: string | null;
  imageTag: string | null;
  imageBlurHash: string | null;
  streamUrl: string;
  /** Identity of this queue line (not the server's play session, which the
   *  player mints per playback attempt). Stable across moves, which is what
   *  row keys and "which entry did I click" resolve against. Empty for
   *  entries persisted by builds before it existed. */
  entryId: string;
  normalizationGain: number | null;
  albumNormalizationGain: number | null;
  artists: ArtistRef[];
  genres: GenreRef[];
  sourcePlaylistId: string | null;
  autoDjReason: AutoDjReason | null;
  isFavorite: boolean;
}

export type PlaybackStatus = "idle" | "loading" | "playing" | "paused";
export type RepeatMode = "off" | "all" | "one";
export type ShuffleMode = "off" | "tracks" | "albums";

/** What an Auto-DJ mix grows from (Rust `AutoDjSeedMode`): the setting, and
 *  the kind every appended track remembers. */
export type AutoDjSeedMode = "track" | "artist" | "genre" | "album" | "playlist" | "sonic";

export interface AutoDjReason {
  kind: AutoDjSeedMode;
  label: string;
  /** Seeded by Sonarune's own sonic similarity. `kind` is then "track",
   *  which is what older builds read it as. */
  bySound?: boolean;
}

export interface DspParams {
  eqEnabled: boolean;
  eqGainsDb: number[]; // 10 bands
  normalizationEnabled: boolean;
  /** Album mode: one gain for the whole album instead of one per track. */
  albumNormalization: boolean;
  preampDb: number;
}

export interface PlaybackSettings {
  crossfadeMode: "off" | "smart" | "always";
  crossfadeSeconds: number;
  /** Device id (`OutputDevice.id`); null = system default. Builds before
   *  0.6.1 stored the device name here, until the device opens again. */
  outputDevice: string | null;
  /** The chosen device's name as last seen, shown while it is away. */
  outputDeviceName: string | null;
  /** Land crossfades on the beat (needs both tracks analysed). */
  beatTransitions: boolean;
}

export interface MusicalKey {
  /** Pitch class of the tonic, 0 = C … 11 = B. */
  tonic: number;
  minor: boolean;
}

/** Tempo and key of a track, analysed locally while it played. */
export interface TrackAnalysisInfo {
  itemId: string;
  bpm: number | null;
  /** Camelot code ("8A"). */
  camelot: string | null;
  key: MusicalKey | null;
}

/** An output device (see audio_devices.rs). */
export interface OutputDevice {
  /** Stable and unique, where names are not (two devices can both be
   *  "Speakers"): what the output setting and the profiles store. */
  id: string;
  /** What the system's sound settings call it. */
  name: string;
}

export interface AudioDeviceSnapshot {
  devices: OutputDevice[];
  /** Requested output (its stored id) currently routed through the system
   *  default. */
  fallbackDevice: string | null;
  /** The device the output actually plays through right now — also while
   *  it is closed after a long pause: it opens there again. */
  activeDevice: OutputDevice | null;
}

/** Sound settings remembered per output device (see audio_profiles.rs). */
export interface AudioProfilesInfo {
  enabled: boolean;
  activeDevice: OutputDevice | null;
  /** Devices with a saved profile; deleted by id. */
  devices: OutputDevice[];
}

export interface AudioOutputFallbackEvent {
  /** Name of the configured device. */
  requestedDevice: string;
}

/** `player:error` (Rust `player/errors.rs`): a playback failure, worded by
 *  `lib/playerErrors.ts`. */
export interface PlayerErrorEvent {
  code: "cannotPlay" | "outputUnavailable" | "outputFailed" | "noFallbackOutput";
  /** Title of the track that did not play. */
  track: string | null;
  /** Name of the output device; null for the system default. */
  device: string | null;
  cause: PlayerErrorCause | null;
}

export interface PlayerErrorCause {
  kind: "notConnected" | "network" | "server" | "noOutputDevice" | "outputDeviceMissing" | "other";
  /** The code a command error would carry (`lib/appErrors.ts`); what the
   *  banner words the cause by. */
  code?: string;
  /** HTTP status, for `server`. */
  status?: number;
  /** The value the code's sentence names, if any. */
  arg?: string;
  /** The error as the log has it (English); shown where the UI has no
   *  words of its own for the cause. */
  detail: string;
}

export interface ExtrasSettings {
  /**
   * Never contains the stored token (it lives in the credential manager).
   * On save: a string stores it, "" clears it, null keeps the stored one.
   */
  listenbrainzToken: string | null;
  autoDjEnabled: boolean;
  autoDjSeedMode: AutoDjSeedMode;
  autoDjMaxTracks: number;
  autoDjRecentTracks: number;
  /** Keep the local play history (one row per listen, on this machine). */
  recordHistory: boolean;
  /** Read-only: a token is stored. */
  listenbrainzConfigured: boolean;
}

/** "Stop after this track / this album", armed on one queue entry. */
export type StopAfter = "off" | "track" | "album";

export interface PlayerState {
  status: PlaybackStatus;
  /** Only while loading: Pause was pressed (or it was paused before a
   *  reopen), so the source arrives paused instead of playing. */
  pendingPause: boolean;
  current: QueueTrack | null;
  sleepRemainingMs: number | null;
  stopAfter: StopAfter;
  /** The queue entry the music stops after, so the row can say so: the armed
   *  row for a track, the album's last row for an album. Its entry id, and
   *  its item id for an entry from a build before entry ids. */
  stopAfterEntry: string | null;
  stopAfterItem: string | null;
  index: number;
  queueLen: number;
  positionMs: number;
  durationMs: number;
  volume: number;
  shuffle: boolean;
  shuffleMode: ShuffleMode;
  repeat: RepeatMode;
  /** Downloaded part of the current track as `[start, end]` in ms (estimated
   *  from bytes); null when the stream length is unknown (live transcode). */
  bufferedMs: [number, number] | null;
  /** Playback speed, 1 = normal (0.5–2). */
  speed: number;
  /** Keep the pitch at other speeds; off, it follows the speed. */
  preservePitch: boolean;
}

/** `player:waveform` — peaks (0–255) of a track, once its download finished. */
export interface WaveformEvent {
  itemId: string;
  peaks: number[];
}

export interface QueueSnapshot {
  /** Names this track list and order; a `QueuePosition` applies to the
   *  snapshot with the same revision only. */
  revision: number;
  tracks: QueueTrack[];
  index: number;
  /** Shuffle play order as indices into `tracks`; empty while shuffle is off.
   *  `queueMove` takes positions in this order (see `lib/queueOrder.ts`). */
  order: number[];
  canUndo: boolean;
  playedCount: number;
  duplicateCount: number;
}

/** `player:queue-index` — the queue moved on, its track list did not. Sent
 *  on a track change instead of the whole `QueueSnapshot`. */
export type QueuePosition = Pick<
  QueueSnapshot,
  "revision" | "index" | "canUndo" | "playedCount" | "duplicateCount"
>;

export type CloseBehavior = "quit" | "tray";

/** Which releases an update check considers (src-tauri/src/updater.rs):
 *  beta takes the newer of the stable release and the latest beta. */
export type UpdateChannel = "stable" | "beta";

export interface DesktopSettings {
  closeBehavior: CloseBehavior;
  startMinimized: boolean;
  coverCacheLimitMb: number;
  autoCheckUpdates: boolean;
  updateChannel: UpdateChannel;
}

/** What `check_for_update` answers (src-tauri/src/updater.rs). `version` is
 *  null when the release endpoint has nothing newer; `installable` is false
 *  for a portable copy, which can see an update but must not run the
 *  installer. */
export interface UpdateInfo {
  currentVersion: string;
  version: string | null;
  notes: string | null;
  date: string | null;
  installable: boolean;
}

export interface CacheInfo {
  sizeBytes: number;
  fileCount: number;
  limitMb: number;
}

export interface TrayLabels {
  show: string;
  hide: string;
  play: string;
  pause: string;
  previous: string;
  next: string;
  quit: string;
  nothingPlaying: string;
}

export type DownloadStatus = "queued" | "downloading" | "completed" | "failed" | "cancelled";

export interface DownloadRequest {
  itemId: string;
  name: string;
}

export interface DownloadTask extends DownloadRequest {
  id: string;
  status: DownloadStatus;
  receivedBytes: number;
  totalBytes: number | null;
  path: string | null;
  error: string | null;
  /** The list's revision at this task's last change: of two copies of a
   *  task, the one with the higher revision is the newer. */
  revision: number;
}

/** The whole download list (`download:state`, `get_downloads`). Changes to
 *  single tasks arrive as `download:changed`, carrying just those tasks. */
export interface DownloadSnapshot {
  /** Raised by every change: a list older than one applied is stale. */
  revision: number;
  tasks: DownloadTask[];
}

export type SearchKind = "artists" | "albums" | "tracks";

export interface SearchPage extends SearchResults {
  total: number;
}

export interface SearchResults {
  artists: ArtistDto[];
  albums: AlbumDto[];
  tracks: TrackDto[];
}

export interface LyricCueDto {
  startMs: number;
  /** Character offset into the line text where this cue begins. */
  position: number | null;
}

export interface LyricLineDto {
  startMs: number | null;
  text: string;
  cues: LyricCueDto[];
}

export interface LyricsDto {
  synced: boolean;
  offsetMs: number;
  lines: LyricLineDto[];
}

export interface GenreDto {
  id: string;
  name: string;
}

// --- Library slice: playlists, favorites, home ---

export interface PlaylistDto {
  id: string;
  name: string;
  trackCount: number;
  imageTag: string | null;
  imageBlurHash: string | null;
}

export interface PlaylistTrackDto {
  track: TrackDto;
  /** Playlist entry id — removal/reorder use this, not the item id. Not a row
   *  key: Jellyfin 10.11 makes it the track's id, so a track that is in the
   *  playlist twice has two entries with the same one (`lib/playlistEntries.ts`). */
  entryId: string;
}

export interface PlaylistDetail {
  playlist: PlaylistDto;
  tracks: PlaylistTrackDto[];
}

/** What a copy or move between playlists did (`transfer_playlist_entries`). */
export interface PlaylistTransfer {
  /** Tracks the target gained — fewer than selected when it already held some;
   *  null when unknown. */
  added: number | null;
  /** A move whose copy went through but whose removal from the source failed. */
  removeError: AppErrorReport | null;
}

/** What cleaning up a playlist's duplicates did (`remove_playlist_duplicates`). */
export interface PlaylistCleanup {
  /** Later entries of repeated tracks that went. */
  removed: number;
  /** Entries the user cannot see. Cleaning up rewrites the playlist with what
   *  the user may see, so while there are any nothing is changed. */
  hidden: number;
}

export interface HomeData {
  recentlyPlayed: AlbumDto[];
  recentlyAdded: AlbumDto[];
  mostPlayed: AlbumDto[];
  forgottenFavorites: AlbumDto[];
}

export interface StatsData {
  recentlyPlayed: TrackDto[];
  mostPlayedTracks: TrackDto[];
  mostPlayedAlbums: AlbumDto[];
  totals: LibraryTotals;
  /** Added up from the most played tracks — Jellyfin cannot sort artists by
   *  plays, so the page says where the number comes from. */
  topArtists: ArtistPlays[];
  decades: DecadeAlbums[];
}

export interface LibraryTotals {
  tracks: number;
  albums: number;
  artists: number;
  playedTracks: number;
}

export interface ArtistPlays {
  id: string | null;
  name: string;
  plays: number;
}

/** The background library analysis for sonic similarity. */
export interface SonicScanStatus {
  enabled: boolean;
  running: boolean;
  /** Tracks the sound analysis is done with (a few too short or quiet to
   *  have a sound vector included), out of `total`. */
  analysed: number;
  /** Tracks of the library the analysis covers (all but very long ones);
   *  null until a listing of this library is known. */
  total: number | null;
  current: string | null;
  lastError: string | null;
}

/** What the play history remembers of a track. */
export interface HistoryTrack {
  itemId: string;
  name: string;
  artist: string;
  album: string;
  albumId: string | null;
  /** The album's artist as the track was tagged; null in rows from before
   *  it was kept. */
  albumArtist?: string | null;
  artists: ArtistRef[];
  durationMs: number;
  imageItemId: string | null;
  imageTag: string | null;
  imageBlurHash: string | null;
}

/** One listen, by the ListenBrainz rule. */
export interface HistoryEntry {
  /** Cursor for the next page (older rows have smaller ids). */
  id: number;
  /** When the listen ended, epoch ms. */
  playedAt: number;
  listenedMs: number;
  track: HistoryTrack;
}

export interface HistoryYears {
  /** Years with plays, newest first (local time). */
  years: number[];
  /** First recorded play, epoch ms; null before the first one. */
  recordedSince: number | null;
}

export interface TrackPlays {
  track: HistoryTrack;
  plays: number;
}

export interface AlbumPlays {
  albumId: string | null;
  name: string;
  /** The album artist its tracks are tagged with, or else the artist most of
   *  its played tracks name; empty when none is credited or for a
   *  compilation (`variousArtists`). */
  artist: string;
  /** No artist is named by most of its played tracks: a compilation. */
  variousArtists: boolean;
  imageItemId: string | null;
  imageTag: string | null;
  imageBlurHash: string | null;
  plays: number;
}

export interface YearReview {
  year: number;
  plays: number;
  listenedMs: number;
  tracks: number;
  artists: number;
  topTracks: TrackPlays[];
  topArtists: ArtistPlays[];
  topAlbums: AlbumPlays[];
  /** Plays per month, January first. */
  months: number[];
  firstPlayedAt: number | null;
  lastPlayedAt: number | null;
}

export interface DecadeAlbums {
  startYear: number;
  /** English ("1990s"), not for display: see `DecadeDto.label`. */
  label: string;
  albums: number;
}

/** First page of each favorites section, with its total (`get_favorites`). */
export interface FavoritesData {
  tracks: Page<TrackDto>;
  albums: Page<AlbumDto>;
  artists: Page<ArtistDto>;
}
