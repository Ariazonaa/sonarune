# Features

Everything Jellysic does, grouped and foldable. The README has the short
version, [`usage.md`](usage.md) walks through the app in order, and
[`architecture.md`](architecture.md) explains how it works underneath; this
page is the list you search when you want to know whether one specific thing
is in there.

Open a section to read it. Nothing below is planned or partial — it is what
the current build does.

<details>
<summary><b>Playback</b> — decoding, gapless, crossfade, EQ, devices</summary>

- **Decoders**: FLAC, MP3, Vorbis and WAV through Symphonia, Ogg-Opus through a
  libopus-backed decoder of our own. All of it runs in Rust; the WebView never
  touches audio.
- **True gapless**: the next track is opened about 20 seconds before the
  boundary and appended to the same output sample-accurately, so an album that
  was mastered without gaps plays without one.
- **Smart crossfade** (opt-in): overlaps two tracks with gain ramps instead,
  and leaves a continuous album alone.
- **10-band equalizer** with presets and your own saved presets, **volume
  normalization**, and a clamp against clipping. Changes apply to the running
  track, no restart.
- **Click-free pause and resume** through a sample-accurate fade stage — the
  same one that fades a sleep timer out.
- **Seeking inside a live transcode**: a server transcode restarts at the new
  position, a direct-play file is positioned before it reaches the audio graph.
  Native seeks wait until the file is fully downloaded, because a decoder
  waiting on an undownloaded range would stall the audio callback.
- **Output device** is selectable, and survives hotplug: if the chosen device
  disappears, playback falls back to the default one and resumes at the same
  position.
- **Sleep timer**, by minutes or at the end of the current track, with an
  optional fade-out.
- **Playback reporting**: Jellyfin sees the session, the position (about every
  10 s) and the stop, each with a play-session id, so the server can correlate
  and kill its own transcode jobs.

</details>

<details>
<summary><b>Library</b> — albums, artists, genres, favorites, metadata</summary>

- **Albums, artists, genres**, each with virtualized grids and lists that stay
  smooth on a large library, and an **A–Z scrubber** that jumps by first
  letter.
- **Home rows** — recently added, recently played, most played, and the rest of
  what the server offers.
- **Multi-disc albums**, play counts, release years, durations.
- **Favorites** for tracks, albums and artists, with their own pages.
- **Recently added** as its own route.
- **Search 2.0**: paged results per kind, search history, and the same context
  menus as everywhere else.
- **Instant mix** and **similar artists** from any track, album or artist.
- **Tag and metadata editing** for what Jellyfin lets a client change.
- **Deleting** items on the server, when the account is allowed to.
- **Library watch**: the server is polled for changes (and on window focus), so
  music added while the app is open shows up without a manual refresh.

</details>

<details>
<summary><b>Queue</b> — the single source of truth, and its safety net</summary>

The queue lives in Rust and is the only thing that decides what plays next; the
UI shows it, it never computes it.

- **Shuffle**, **album shuffle** and **repeat** (off / all / one).
- **Reorder by dragging**, remove, jump — all keyed by identity, so a stale UI
  index cannot move or drop the wrong row.
- **One-step undo** for queue edits, invalidated by any other queue change.
- **Clean up**: remove played tracks, remove duplicates, clear.
- **Survives restarts**: the queue and its position are persisted to SQLite and
  come back paused after a restart.
- **Save the queue as a playlist** in one action.

</details>

<details>
<summary><b>Playlists</b> — create, reorder, dedup, transfer</summary>

- Create, rename, duplicate and delete playlists.
- Reorder entries by drag-and-drop, remove entries, remove duplicates.
- **Transfer entries between playlists.**
- Add anything to a playlist from its context menu, including multi-selections.
- Reorders and removals use the playlist's *entry id* rather than the track id,
  so a playlist that contains the same track twice behaves.

</details>

<details>
<summary><b>Discovery and Auto-DJ</b> — finding something to play</summary>

- **Discover hub** with random albums, mixes, long-not-heard, artists.
- **By decade**: decade rows and a page per decade.
- **Smart views**: build a query from server-side filters (genre, year, rating,
  added-since and so on), save it locally, and materialize it into a static
  playlist when you want it to stop moving.
- **Collections** — Jellyfin BoxSets, read-only.
- **Auto-DJ**: pick a seed (track, artist, genre, album or playlist) and when
  the queue is about to run dry, Jellysic appends a fresh instant mix. It
  resumes playback if the queue ran out before the mix arrived, and holds off
  while a sleep timer is about to end the session.

</details>

<details>
<summary><b>Now playing</b> — lyrics, waveform, ambient backdrop</summary>

- **Synced lyrics**, line- and word-level, as a side panel or fullscreen, with
  auto-scroll and click-a-line-to-seek.
- **Per-track lyrics offset**, shared by every lyrics view and remembered.
- **Waveform seek bar**: the whole track's peaks, analysed once the file has
  been downloaded, with the buffered range, hover times and keyboard control.
- **Ambient backdrop** from two colors extracted from the cover.
- **Song info** panel with the technical details of what is actually playing.

</details>

<details>
<summary><b>Visualizer</b> — MilkDrop presets driven by the real audio</summary>

- **Butterchurn** (MilkDrop-style WebGL presets): 393 presets in seven bundled
  packs, with a preset browser.
- The optional **Butterchurn Weekly** pack (about 550 more presets) is an
  explicit opt-in, because those presets are code downloaded from a third-party
  server. They are fetched in Rust and only handed to the WebView when their
  SHA-256 matches a pinned manifest.
- **Beat-sync**, sensitivity and render-quality controls, and screenshots.
- A **projector window** for a second monitor, with overlays.
- The audio it reacts to is the player's own PCM, shipped over IPC into a muted
  audio graph — the visuals match what you hear, and there is no second audio
  path.
- A **spectrum bar** in the player chrome, from the same feed.

</details>

<details>
<summary><b>Desktop</b> — media keys, tray, mini player, updates</summary>

- **Windows SMTC**: the OS now-playing overlay shows the track and cover, and
  the media keys work.
- **System tray** with a background mode: close to tray or quit (your choice),
  start minimized, and play/pause/next/previous plus the current title in the
  tray menu.
- **Mini player**, a small frameless window.
- **Background is cheap**: hidden or minimized, the WebView is suspended and
  the music keeps playing from Rust — about 115 MB instead of 442 MB, with the
  server still getting its progress reports.
- **One instance**: a second launch hands over to the running window instead of
  starting a second player.
- **In-app updates**: Jellysic can ask the release page whether a newer version
  exists (once per start, and that can be switched off) and install it from
  Settings. The download is verified against the project's signing key before
  the installer is opened, and an update never runs while a track is playing —
  the installer closes the app. The portable build says so instead of
  installing anything.
- **Downloads**: export original files, two at a time, with progress, cancel
  and retry. This is an export, not an offline cache.

</details>

<details>
<summary><b>Keyboard and menus</b> — palette, shortcuts, context menus</summary>

- **Command palette** (`Ctrl+K` or `Ctrl+P`): jump anywhere, trigger anything.
- **Configurable shortcuts** with conflict and reserved-key detection.
  Defaults: `Space` play/pause, `Ctrl+←`/`Ctrl+→` or `P`/`N` previous/next,
  `←`/`→` seek, `↑`/`↓` volume, `M` mute, `L`/`S`/`R` favorite/shuffle/repeat.
- A **shortcut help** overlay listing the current bindings.
- **Context menus with multi-select** on every list and grid.

</details>

<details>
<summary><b>Look and feel</b> — one dark theme, done properly</summary>

- **AMOLED dark**, one theme: a true-black ground, a short ladder of near
  blacks, hairlines instead of light edges. There is no light mode.
- **The accent follows the cover** by default, walked up in lightness until it
  stays readable against black, with a fixed accent as the fallback for grey
  artwork — or pick one of eight presets or your own color.
- **View density and layout presets**: compact or comfortable lists, grid card
  size, sidebar width, remembered per view.
- **Skeletons, empty states and an offline banner**, plus a
  stale-while-revalidate cache, so navigation is instant and the UI never
  pretends to know something it does not.
- **German and English**, switchable in Settings.

</details>

<details>
<summary><b>Settings and maintenance</b></summary>

- **Playback**: gapless, crossfade, EQ and presets, normalization, output
  device, sleep timer.
- **Desktop**: close behavior, start minimized, cover-cache limit, automatic
  update check.
- **Extras**: Auto-DJ seed and length, ListenBrainz token.
- **Cover cache** with a size limit and a "clear" button; eviction runs in the
  background.
- **Diagnostic bundle**: app, system, server and audio information plus
  redacted logs — URLs, credentials, absolute paths and long ids are dropped
  before they are written.
- **Export and import settings** as JSON. Tokens are never part of it.
- **Trusted certificates**: list and forget the certificates you pinned.

</details>

<details>
<summary><b>Security and privacy</b></summary>

- **The WebView never touches the server.** Every request — sign-in, browsing,
  cover art, the audio stream — goes through Rust, so tokens never reach the
  DOM.
- **Per-server certificate pinning** (SHA-256, a custom rustls verifier) for
  self-signed home servers: the OS trust store first, and an untrusted
  certificate only after you confirmed its fingerprint. There is no blanket
  "accept invalid certificates".
- **Tokens live in the OS credential store**, not in the database and not in an
  export — the ListenBrainz token included.
- **A Content-Security-Policy** limits the WebView to the app's own resources,
  the IPC channel and the cover-art protocol.
- **Remote visualizer presets are treated as code**: opt-in, downloaded in
  Rust, checked against a pinned hash.
- **Updates are signed** and verified before they are installed.
- Jellysic talks to your Jellyfin server, and — only if you configure it —
  ListenBrainz and the release page. There is no telemetry.

</details>

<details>
<summary><b>What Jellysic does not do</b></summary>

Deliberate non-goals, not gaps waiting to be filled:

- **Offline downloads.** Files can be exported; there is no offline library.
- **Casting** (Chromecast, UPnP) and **SyncPlay**.
- **Podcasts and audiobooks** — it is a music client.
- **Multiple servers at once**, and no Subsonic or Navidrome backends.
- **Video.** Jellysic plays music.

</details>
