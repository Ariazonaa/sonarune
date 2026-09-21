# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[semantic versioning](https://semver.org/spec/v2.0.0.html), with the usual 0.x
caveat that a minor bump may still change behaviour.

## [Unreleased]

## [0.2.0] — 2026-09-21

### Added

- **In-app updates.** The installed app can ask the release page whether a
  newer version exists — once per start, and that check can be switched off in
  Settings → Updates, where there is also a button to check on demand. An
  update is downloaded and verified against the project's minisign key before
  the installer is opened, so a swapped download cannot be installed even
  though the installer itself is not code-signed. It never runs while a track
  is playing or loading, because the installer closes the app, and the last
  playback report is flushed before it does. The portable build says it cannot
  update itself instead of turning itself into an installed copy.
- Releases now carry `latest.json` and the installer's `.sig`, which is what
  the update check reads. 0.1.0 has neither, so the step from 0.1.0 to 0.2.0
  is a manual download; every step after it is not.

### Fixed

- The Jellyfin play session is now minted per attempt at a track instead of
  once per queue entry. Playing the same entry again — repeat-one, jumping
  back, a queue restored after a restart — reported a start under a session
  the server had already been told was stopped, which left it unable to tie
  the reports to the right playback and to the transcode job it had started
  for it. A reopen (seeking inside a transcode, switching the output device)
  still continues the running session, because it is the same playback.

## [0.1.0] — 2026-09-21

First public release. Windows, Jellyfin 10.11 or newer.

### Playback

- Rust audio engine built on Symphonia and rodio: FLAC, MP3, Vorbis and WAV,
  with a libopus-backed decoder for Ogg-Opus. The WebView never touches audio.
- True gapless — the next track is opened ahead of the boundary and appended
  sample-accurately — and an opt-in crossfade with gain ramps.
- 10-band equalizer, volume normalization, and click-free pause and resume
  through a sample-accurate fade stage.
- Seeking inside a live server transcode, and a sleep timer with fade-out.
- Output device is selectable and falls back automatically when a device
  disappears, resuming at the same position.

### Library and queue

- Albums, artists and genres with an A–Z scrubber, virtualized grids,
  favorites, home rows, recently added, multi-disc handling and play counts.
- The queue lives in Rust as the single source of truth: shuffle, repeat,
  album shuffle, one-step undo, deduplication, played-cleanup — and it survives
  a restart.
- Playlists with full editing, drag-and-drop reordering, duplicate detection,
  transfer between playlists, and saving the current queue as a playlist.
- Search with history, instant mix, and similar artists.

### Discovery

- A Discover hub with random, mixes, long-not-heard, decade and artist
  drill-downs.
- Dynamic smart views: server-side filters you can save and, when you want them
  frozen, materialize into a static playlist.
- Read-only Collections, and an Auto-DJ that keeps the queue going from a
  track, artist, genre, album or playlist seed.

### Now playing and visuals

- Synced lyrics at line and word level, as a panel or fullscreen, with a
  per-track offset when the file's timing is off.
- A now-playing backdrop derived from the cover art.
- A Butterchurn (MilkDrop) visualizer with 393 bundled presets, a preset
  browser, beat sync, sensitivity and quality controls, and a projector window
  for a second monitor. The online preset pack stays behind an explicit opt-in
  and a pinned checksum, because presets are executable code.

### Desktop

- Windows SMTC, so media keys and the system now-playing overlay work.
- System tray with a background mode, and a frameless mini player.
- Hidden or minimized, the WebView is suspended and memory drops to roughly
  115 MB while playback continues from the Rust side.
- ListenBrainz scrobbling, a command palette, and fully configurable keyboard
  shortcuts.

### Look

- AMOLED dark: a true-black ground, a short ladder of near blacks, hairlines
  rather than light edges. There is one theme, deliberately.
- The accent follows the playing cover by default and is kept readable against
  black; eight presets and a custom color are available instead.
- View density and layout presets, skeletons, empty states, an offline banner,
  and a stale-while-revalidate cache so navigation renders instantly.

### Security

- Self-signed servers are handled by per-server SHA-256 certificate pinning
  through a custom rustls verifier. There is no blanket "accept invalid
  certificates" switch.
- Auth tokens live in the OS credential store, never in the DOM and never in
  SQLite.
- A Content-Security-Policy restricts the WebView to the app's own origins.

### Known limitations

- The installer is not code-signed, so SmartScreen reports an unknown
  publisher.
- Windows only for now. Nothing in the architecture is Windows-specific, but
  no other platform has been built or tested.
- No offline downloads, casting, podcasts, multiple servers at once, or
  Subsonic/Navidrome backends. These are deliberate non-goals for 1.0.

[Unreleased]: https://github.com/Ariazonaa/jellysic/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Ariazonaa/jellysic/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ariazonaa/jellysic/releases/tag/v0.1.0
