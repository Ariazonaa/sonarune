# Changelog

Notable changes per release. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[semantic versioning](https://semver.org/spec/v2.0.0.html), with the usual 0.x
caveat that a minor bump may still change behaviour.

## [Unreleased]

## [0.7.0] — 2026-10-23

0.6.1 was never released; everything it would have brought is part of 0.7.0.

### Added

- **Play history.** Every track you listen to is recorded on this computer
  (by the ListenBrainz rule: half the track or four minutes). The *History*
  page shows it by day and plays a track again on click; entries can be
  removed and recording switched off. A **year in review** is built from it.
- **Sonic similarity, without a server plugin.** Sonarune analyses how your
  tracks sound (Settings → Sonic similarity, off by default) and offers a
  **sonic radio** from any track, a **sonic path** from one track to another,
  and an Auto-DJ mode that continues **by sound**.
- **A beta channel** (Settings → Updates): betas as soon as they are out, and
  the stable release after them. Back on stable, the beta stays installed until
  a stable release is newer.

### Changed

- **Each track decodes on its own thread**, 1.5 s ahead of the speakers: a
  slow connection silences only its own track, Next, Stop and a device switch
  act at once, and a seek in a track still downloading goes straight there.
- **Music at another sample rate than the device sounds better**: CD rips on a
  48 kHz device are resampled with a band-limited filter, so the treble is no
  longer dulled. **Gapless albums join without a trace** at any sample rate.
- **Seeking back in a long variable-bitrate MP3** (a two-hour mix) jumps
  straight to the target and lands on the exact sample.
- **A limiter** catches peaks that normalization, preamp or equalizer push past
  full scale, instead of clipping them.
- **The audio device is released after five minutes without sound**, so
  Windows can sleep while Sonarune waits in the tray. Play carries on where the
  track was paused.
- **Sonarune remembers where the track stood** after quitting, a restart or an
  update.
- **Output devices are told apart by their Windows id**, so two devices with
  the same name keep their own selection and sound profile. *System default*
  follows the Windows default device.
- **A server is recognised by its Jellyfin id**, not by its address: after a
  move to https or a new address, the history and sonic similarity carry on.
- **Long playlists open at once**, big download batches stay light, and the
  download panel can *Cancel all*.
- **Errors are shown in your language.**
- Certificates are checked by Windows' own verifier, and the Windows proxy
  bypass list is honoured. Signing out also removes the ListenBrainz token.

### Fixed

- **Sonarune starts on a Windows without the Visual C++ Redistributable**;
  0.6.0 quit there without a word, as on a fresh installation.
- **The visualizer did not start in 0.6.0.** Drag and drop did nothing on
  Windows, and clicking an artist name reloaded the app.
- Playback in the background survives a revoked sign-in.
- Jellyfin 10.11: genres were missing on music-only servers, and a playlist
  with a track twice did not open.
- The sleep timer's fade-out was lost at a track change, a broken stream could
  freeze playback, and 5.1 tracks on a stereo device could switch channels.
- Shuffle order survives a restart; *Stop after this album* waits for its last
  track.
- Many smaller fixes from a full code review.

### Security

- **Uninstalling with "Delete the application data" also removes the saved
  sign-in** (server token, password, ListenBrainz token) from the Windows
  Credential Manager. Updates and a plain uninstall leave it.

## [0.6.0] — 2026-10-18

### Upgrading from 0.5.x

**Install 0.6.0 by hand once**: download the installer below and run it over
your Jellysic. The built-in updater of 0.5.x cannot take this release,
because the key that signs updates has changed: 0.5.x may offer the update and
then refuse it. Everything else carries over (see "Jellysic is now Sonarune"
below), and from 0.6.0 on updates arrive through the app again.

### Added

- **Sound per output device.** With the new switch in Settings → Playback,
  equalizer, normalization, preamp and volume are remembered for every output
  device and come back on their own when audio plays through that device
  again — plugging in headphones no longer means retuning the speakers'
  curve. It follows the device that actually plays, including "system
  default" and a fallback after a device was unplugged.
- **Crossfade on the beat.** A new switch next to the crossfade settings
  starts the next track so that its first beat lands on a beat of the
  current one, with an overlap of whole bars. When the keys clash, the
  overlap stays at one bar; when the tempos are too far apart (more than 4 %,
  half and double time count as matching), it is the usual crossfade. Tempo,
  beat grid and key are worked out on your computer from the same decode the
  seek-bar waveform already uses, and kept, so a track is analysed once.
  Tempo and key (with its Camelot code) also show in a track's info.
- **Playback speed.** A new button in the player bar sets the speed from
  0.5× to 2×, with presets and a slider. The pitch stays where it is
  (time-stretching with Signalsmith Stretch), or, with "Keep pitch" off,
  follows the speed like a record. At 1× the stage is bypassed and the sound
  is exactly what it was. Seeking, crossfades, crossfades on the beat, the
  sleep timer and ListenBrainz all keep working at other speeds. The speed is
  remembered and part of the settings export.

### Changed

- **Jellysic is now Sonarune.** New name, new mark (an S whose tail is a note
  head), and the name goes all the way down: the program folder, the exe, the
  app identifier (`io.sonarune.app`), the database, the keyring entries and
  every stored preference. An installed 0.5.x, upgraded by hand (see above),
  keeps its sign-in, settings, queue, cover cache and window position: the app moves its
  data over from the old locations on first start, and the installer removes
  the old installation and carries its Start-menu and desktop shortcuts over.
  A taskbar pin has to be set again. Visualizer backups exported under the old
  name still import.

### Fixed

- A sleep timer that ran out while playback was paused no longer swallows the
  next press of Play: the timer switches itself off, and Play plays. Before,
  the first press only cleared the timer, and the player bar still showed it
  as waiting for the end of the track.

## [0.5.1] — 2026-10-15

### Security

- Three dependencies updated for published advisories: `serialize-javascript`
  7.1.2 (GHSA-gfhx-hw2g-v5hg), `source-map-js` 1.2.2 (CVE-2026-93749) and
  `brace-expansion` 2.1.7 / 1.1.21. All three are build- and test-time tools;
  nothing else changed in the app.

## [0.5.0] — 2026-09-22

### Changed

- **The installer is built by a pipeline that runs no third-party code.** The
  one action that was not GitHub's own is gone — it put the Microsoft compiler
  on the PATH, which turned out to be something nothing needed: cargo finds the
  linker and cmake finds Visual Studio through the registry. Proven before it
  was removed, uncached, on every runner this project builds on. What signs and
  publishes your installer is now GitHub's checkout and GitHub's node setup,
  and nothing else.

### Fixed

- **Saving in the metadata editor did nothing.** It asks before it writes, and
  that prompt was the one dialog in the app that stayed where it was declared
  instead of moving to the end of the document like every other overlay. At the
  same stacking level the editor — which does move — was painted over it, so
  the prompt sat behind the editor's dark backdrop: invisible, and the click
  meant for it landed on that backdrop. Pressing Save looked like it did
  nothing at all, because nothing is exactly what happened. Any confirmation
  asked from inside another dialog would have gone the same way; the metadata
  editor was the only place that did.
- A successful metadata write could still be reported as an error: the list
  behind the dialog was reloaded inside the same guarded block as the write,
  so a list that failed to reload turned a finished save into an error message
  and kept the dialog open.

## [0.4.0] — 2026-09-21

### Added

- **The stats page is a dashboard now.** Four numbers about the library —
  tracks, albums, artists, and how much of it has been played at least once —
  and two charts: the most played artists, and albums by decade. Both bars are
  walkable: a row leads to that artist or that decade. The artist numbers are
  added up from your most played tracks, because the server counts plays per
  track and not per artist, and the chart says so rather than pretending
  otherwise.
- **Metadata for many tracks at once.** Select rows in the song list,
  right-click, and set album artist, genres or the year for all of them —
  genres can be added to what each track already has instead of replacing it.
  Only what you fill in is written: everything left empty stays as it is on
  each track, and titles and track numbers are not offered at all, because
  they are not something several tracks share. A track the server refuses is
  counted and the rest still go through.

### Changed

- The Opus decoder now builds on libopus 1.6.1 instead of 1.3. The `opus`
  crate moved to a maintained `-sys` crate in August; the one it used before
  had been sitting untouched since 2021 and carries an "unmaintained" advisory
  (RUSTSEC-2026-0150). Nothing in the app had to change, and the binary grew
  by 80 KB.
- An update check now says in the log what it found — the version offered and
  the one it was compared against. The panel only ever shows the answer, and a
  diagnostic export should be able to answer the next question by itself.
- Groundwork for installing Sonarune with `winget`: the three manifests that
  the Windows package repository wants are generated from a published release —
  download URL, release date and the checksum, the last of these re-computed
  against the installer as it is actually served — instead of being typed out
  by hand. Nothing is listed there yet; this is the part that had to exist
  first.

## [0.3.0] — 2026-09-21

### Added

- **Volume normalization has an album mode.** Settings → Playback now offers
  off, per track or per album. Per album plays every track of an album at one
  gain, so a quiet piece stays quieter than a loud one — what the album was
  mixed for. Jellyfin only sends an album value after 10.11; without one the
  gain is the median of the album's tracks in the queue, which is the same
  constant offset even if the absolute level can sit a little off. A change
  takes effect from the next track.
- **Stop after this track, or after this album.** Right-click a queue entry:
  playback ends where you said it would, without a fade — the track finishes
  the way it was recorded and then it is quiet. The armed row is marked, and
  the gapless hand-off is held back for that one boundary so the track really
  does end. An album stop knows where its album ends, even when the queue
  continues with another one.
- **Every track list has the same right-click menu.** Album, playlist,
  favorites and search results had a few hover buttons and no menu at all;
  they now offer what the song list has offered all along — play, play next,
  add to the queue, add to a playlist, instant mix, go to album or artist,
  info, edit, download, delete — plus what only that list can do, such as
  removing an entry from the playlist it sits in or taking a track out of the
  favorites.
- **The playlists are in the sidebar**, and tracks and albums can be dragged
  onto them. Drag a row from the song list, an album's track list or the
  favorites — or a whole album from a grid — and drop it on a playlist to add
  it. A drag from outside the window is not accepted; only the app's own
  payload lights a playlist up.
- **The home page can be arranged.** Each row can be moved or hidden from its
  own heading; Settings → View lists them all, which is where a hidden one
  comes back from. A row the server has nothing for still stays out — that is
  not a setting, that is an empty row.
- **Jump to the playing track.** `J`, the command palette, or the pill that
  appears in the queue while the current entry is scrolled out of sight. The
  queue also opens on the playing track now, and follows along to the next one
  while it is on screen — but never while you are reading somewhere else in
  the list.

### Changed

- The update panel now shows what actually changed. `latest.json` carries the
  release's changelog section instead of a sentence pointing at a file the
  dialog cannot open.
- A failed update check says what went wrong in words: the releases page was
  unreachable, it holds no release for this platform, the download broke off,
  or — the one worth its own sentence — the download did not match the
  project's signature and was discarded. The technical detail stays in the log
  and in the diagnostic export.

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

[Unreleased]: https://github.com/Ariazonaa/sonarune/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/Ariazonaa/sonarune/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/Ariazonaa/sonarune/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/Ariazonaa/sonarune/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/Ariazonaa/sonarune/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Ariazonaa/sonarune/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Ariazonaa/sonarune/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Ariazonaa/sonarune/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ariazonaa/sonarune/releases/tag/v0.1.0
