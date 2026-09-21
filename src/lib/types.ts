// Mirrors of the Rust DTOs (src-tauri/src/api/types.rs, player.rs, commands.rs).

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

/** The six whitelisted fields the metadata editor can write. */
export interface MetadataEdits {
  name: string;
  albumArtists: string[];
  year: number | null;
  genres: string[];
  trackNumber: number | null;
  discNumber: number | null;
}

export interface ArtistDto {
  id: string;
  name: string;
  imageTag: string | null;
  imageBlurHash: string | null;
  overview: string | null;
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

export interface AutoDjReason {
  kind: "track" | "artist" | "genre" | "album" | "playlist";
  label: string;
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
  outputDevice: string | null;
}

export interface AudioDeviceSnapshot {
  devices: string[];
  /** Requested output currently routed through the system default. */
  fallbackDevice: string | null;
}

export interface AudioOutputFallbackEvent {
  requestedDevice: string;
}

export interface ExtrasSettings {
  /**
   * Never contains the stored token (it lives in the credential manager).
   * On save: a string stores it, "" clears it, null keeps the stored one.
   */
  listenbrainzToken: string | null;
  autoDjEnabled: boolean;
  autoDjSeedMode: "track" | "artist" | "genre" | "album" | "playlist";
  autoDjMaxTracks: number;
  autoDjRecentTracks: number;
  /** Read-only: a token is stored. */
  listenbrainzConfigured: boolean;
}

/** "Stop after this track / this album", armed on one queue entry. */
export type StopAfter = "off" | "track" | "album";

export interface PlayerState {
  status: PlaybackStatus;
  current: QueueTrack | null;
  sleepRemainingMs: number | null;
  stopAfter: StopAfter;
  /** The queue entry the stop is armed on (item id), so the row can say so. */
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
}

/** `player:waveform` — peaks (0–255) of a track, once its download finished. */
export interface WaveformEvent {
  itemId: string;
  peaks: number[];
}

export interface QueueSnapshot {
  tracks: QueueTrack[];
  index: number;
  /** Shuffle play order as indices into `tracks`; empty while shuffle is off.
   *  `queueMove` takes positions in this order (see `lib/queueOrder.ts`). */
  order: number[];
  canUndo: boolean;
  playedCount: number;
  duplicateCount: number;
}

export type CloseBehavior = "quit" | "tray";

export interface DesktopSettings {
  closeBehavior: CloseBehavior;
  startMinimized: boolean;
  coverCacheLimitMb: number;
  autoCheckUpdates: boolean;
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
  /** Playlist entry id — removal/reorder use this, not the item id. */
  entryId: string;
}

export interface PlaylistDetail {
  playlist: PlaylistDto;
  tracks: PlaylistTrackDto[];
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
}

/** First page of each favorites section, with its total (`get_favorites`). */
export interface FavoritesData {
  tracks: Page<TrackDto>;
  albums: Page<AlbumDto>;
  artists: Page<ArtistDto>;
}
