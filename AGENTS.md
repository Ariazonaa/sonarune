# AGENTS.md

Instructions for AI coding agents working on Sonarune — Claude Code, Codex,
GitHub Copilot, Cursor, Gemini CLI or any other — and for the people steering
them. This file is the single source: `CLAUDE.md` and `GEMINI.md` only import
it, `.github/copilot-instructions.md` only points here. Change the rules here,
nowhere else.

## Working rules

**Reporting**
- Report what actually happened. Say which checks ran and what they showed,
  and say plainly what was *not* verified ("built, but not tested in the
  running app"). Never present a check as passed that did not run.
- Ask only when a decision genuinely belongs to the maintainer — scope,
  product behaviour, anything irreversible or public that these rules do not
  already cover. Otherwise pick the sensible default and name it in the report.
- Commit messages say what was done, accurately; length is free. AI-assisted
  commits carry the agent's `Co-Authored-By` trailer (see the AI disclosure in
  the README).

**Verifying**
- Run the gates the change touches before committing: Rust → `cargo fmt
  --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib`;
  frontend → `npm run check` (0 errors, 0 warnings), `npm run test`; a
  production build when bundling, the CSP, assets or the installer change.
  The full `npm run verify` runs before anything is published or a pull
  request is opened.
- Where a change can be seen, also exercise it in the running app — against a
  real Jellyfin 10.11 server or the stand-in in `scripts/devserver/`.
- Get an independent second opinion (another agent or model) before landing
  changes to the audio hot path (`player/`), TLS and certificate pinning
  (`tls.rs`), the updater and installer, data migrations (`legacy.rs`, the
  store) or anything else security-relevant.

**Scope and safety**
- Dependencies may be added or updated when the task needs them. Keep the
  lockfiles committed, stay GPL-3.0-compatible (permissive licences preferred),
  check for known advisories, and name every dependency change in the report.
- Tags and releases are cut only when the maintainer asks.
- Planning and bugs live in GitHub issues.
- Never commit secrets, tokens, personal data or machine-specific paths.

## Project

Sonarune is a Windows-first Tauri 2 desktop music client for Jellyfin (server 10.11+), built as a faster/stabler replacement for Feishin. Svelte 5 (runes) + Tailwind v4 frontend, all real work in Rust.

## Commands

```powershell
npm run tauri dev        # run the app (Vite on port 1420 + cargo run; rebuilds on change)
npm run check            # svelte-check (0 errors, 0 warnings is the bar)
npm run test             # frontend unit tests (Vitest + jsdom)
npm run build            # frontend production build (also a good type smoke test)
npm run verify           # the full quality gate: check + test + fmt + clippy + cargo test + build
cargo clippy -- -D warnings   # in src-tauri/ — warnings are errors here
cargo check              # in src-tauri/
npm run tauri build      # release build + NSIS installer
```

`npm run verify` (`scripts/verify.mjs`) is the automated quality gate — the same
checks CI runs (`.github/workflows/ci.yml`, Windows), fail-fast. It needs cmake +
cargo on PATH for the Rust gates. The gate list and the manual live-test matrix
against Jellyfin 10.11.x live in `docs/testing.md`. Frontend tests sit next to
their module as `*.test.ts` (Vitest); mock Tauri commands by mocking
`@tauri-apps/api/core` `invoke` (see `src/lib/api.test.ts`).

`cargo test --lib` runs the unit tests (DSP, opus decoder, fades, scrobble rules, null-sink gapless continuity, queue shuffle/undo/dedup permutations, smart-view resolver, cert fingerprint/normalize, download part-file naming). There's also an ignored live smoke test (`api/mod.rs` mod `smoke`) that exercises the real server contract for the browse/playlist/artist/stats **and discovery/smart-view/collection** flows — run it against a real 10.11 server with `SONARUNE_TEST_URL=… SONARUNE_TEST_USER=… SONARUNE_TEST_PASS=… cargo test --lib smoke -- --ignored --nocapture` (it creates and deletes a temporary "delete me" playlist).

The frontend build output MUST stay in the project's `build/` dir: `frontendDist` in `tauri.conf.json` is `../build`, and Tauri embeds it into the binary at build time. Pointing `frontendDist` at an absolute path outside the project silently breaks that embedding — the *installed* app then shows an empty page / directory listing (dev is unaffected, it loads from `devUrl`). The `ENOTEMPTY` the adapter's rimraf occasionally throws on slow or networked file systems is handled by a retry-capable pre-clean (`scripts/clean-build.mjs`, run first in `npm run build`), not by moving the output. If your checkout sits on slow storage, redirect the **Cargo target** instead — see `src-tauri/.cargo/config.toml.example`.

Discord Rich Presence was removed; the module is archived under `archive/discord-rpc/` with re-integration notes.

The app mark (a bold S whose lower stroke ends in a tilted note head) is **generated**: `scripts/make-logo.mjs` describes it once as geometry and rasterizes every size natively from a distance field -- the whole `src-tauri/icons/` set including a hand-written `.ico`/`.icns`, `static/favicon.png`, `docs/logo.{png,svg}` and `src/lib/components/BrandMark.svelte` (used by the sidebar and the sign-in screen). Change the mark or `ACCENT` there, never the outputs, then re-run it **and** `scripts/make-installer-art.mjs`, which derives the setup art from `icons/icon.png`.

The NSIS installer is branded (custom header/sidebar art, `installerIcon`, de/en language selector, `currentUser` install) via `bundle.windows.nsis` in `tauri.conf.json`. The header/sidebar bitmaps in `src-tauri/installer/*.bmp` are **generated** (24-bit BMP3, the format NSIS requires) from the app logo by `scripts/make-installer-art.mjs` (`node scripts/make-installer-art.mjs`, needs the `pngjs` devDep). Re-run it if the logo/brand changes; it also drops PNG previews in the temp dir.

**Environment gotchas:**
- Building needs **cmake** on PATH (the `opus` crate compiles bundled libopus). It's in the user PATH already (VS Build Tools CMake); if a build complains, prepend `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`.
- **CI installs the latest `stable` Rust on every run; a pinned local toolchain does not follow.** A lint introduced in a newer clippy therefore fails CI while the local `cargo clippy --all-targets -- -D warnings` stays green — check the CI log's `rustc` version before assuming a flaky run.
- If port 1420 is stuck after a killed dev run, kill the owning process (`Get-NetTCPConnection -LocalPort 1420`).
- To leave the dev app running beyond a tool-call timeout, spawn it detached: `Start-Process cmd -ArgumentList "/c","npm run tauri dev"`.
- Don't run `npm run verify`/`npm run check` while `tauri dev` is up: both compile Paraglide into `src/lib/paraglide` and the two runs trip over each other (`ENOTEMPTY`, then a 404 on `messages.js` in the dev app). `npm run i18n:compile` repairs it. Its `--strategy` must match `vite.config.js`, or the dev app falls back to English.

Releases are cut by `.github/workflows/release.yml`, not by hand: push a `v*`
tag whose version matches all three manifests (package.json, tauri.conf.json,
Cargo.toml — the workflow refuses the build otherwise) and a **draft** release
appears with the NSIS installer, a portable zip and `SHA256SUMS.txt`; review it
and publish by hand. A manual run builds the same way and creates nothing,
which is how to check the release path without cutting a release. Keep
`CHANGELOG.md` current in the same commit that bumps the version.

**Moving a tag that already has a release runs the workflow again**, and its
last step calls `gh release create` without asking whether that release exists.
On 22.09.2026 a force-pushed `v0.5.0` therefore produced a second, draft
release next to the published one — harmless, invisible to others, and easy to
miss. The published release and its assets were untouched. If a tag has to
move, delete the draft afterwards, and do not re-run a failed one: a successful
re-run only makes another draft.

For **winget**, `node scripts/winget-manifest.mjs` writes the three manifests
of a *published* release into `winget/manifests/` — download URL and date from
the GitHub API, checksum from `SHA256SUMS.txt` and re-computed against the
downloaded installer, the ARP entries read out of `tauri.conf.json` so they
cannot drift from what the installer actually registers. Submitting is by
hand: fork microsoft/winget-pkgs, copy `winget/manifests/*` over its
`manifests/`, open a pull request. Every version needs its own.

The installed app **updates itself** (`src-tauri/src/updater.rs`, Settings →
Updates). `plugins.updater` in `tauri.conf.json` carries the endpoint
(`releases/latest/download/latest.json`) and the **public** minisign key; the
release workflow signs the installer with the private key from the
`TAURI_SIGNING_PRIVATE_KEY` secret and writes `latest.json` next to it.
Signing is switched on by merging `src-tauri/tauri.updater.conf.json` into that
one build, so a local `npm run tauri build` still works without any key. The
check and the install are our own commands rather than the plugin's JS API, so
they go through the normal ACL generation and no secondary window can reach
them. Three rules live in the code and should stay there: never install while
the player is playing or loading (it is checked again after the download),
flush the player through `flush_before_exit` before the installer kills the
process, and refuse in a portable copy — recognised by the absence of the NSIS
`uninstall.exe` next to the exe — instead of silently turning it into an
installed one. `requireSignedVersion` stays off: it needs the version in the
signature's trusted comment, and CLI 2.11.4 writes only a timestamp and the
file name there, so enabling it would reject every update. Revisit when the
CLI records it.

## Ground rules (from the project decisions — do not violate)

- **The WebView never touches audio or HTTP.** All server traffic (auth, browsing, covers, streams) goes through Rust/`reqwest` (tokens never in the DOM). Audio is decoded and played in Rust (Symphonia via rodio). Self-signed home servers are handled by **per-server SHA-256 certificate pinning** (`src-tauri/src/tls.rs`): a custom rustls `ServerCertVerifier` validates against the OS trust store first and accepts an untrusted leaf only when its fingerprint was user-confirmed (stored per `server_url` in SQLite). `connect` returns `ConnectResult::CertUntrusted { fingerprint }` for the login prompt; the legacy `accept_invalid_certs` flag now means "auto-trust this server on first use" (also migrates pre-pinning installs). That flag is only ever **kept while the server has a pin** (`Store::settle_auto_trust`, applied after every sign-in, Quick Connect, re-login and restore): a login that goes through with nothing pinned proved a public certificate, and a stored flag would let a later silent re-login pin whatever an attacker presents. A confirmed fingerprint is accepted in memory for that one login attempt and pinned only after the login succeeds, only if the server actually presented it, and it **replaces** the server's previous pin (`Store::pin_fingerprint`); the session client is then rebuilt with exactly the stored pins. Settings → Trusted certificates lists and forgets pins (`list_trusted_certificates` / `forget_trusted_certificate`); forgetting the active server's pin signs out. Settings → Account signs out (`disconnect`: token revoked, credentials and queue dropped; settings and pins stay). The OS trust store is loaded once per process; when it has no anchors at all (a Linux/macOS system without a CA bundle) the verifier accepts pinned leaves only instead of failing to build. **Do not** reintroduce blanket `danger_accept_invalid_certs` — route new HTTP clients through `tls::apply`.
- **Clean-room vs. Feishin**: never copy Feishin code or assets — concepts only. Sonarune is GPL-3.0-or-later, so this is no longer a licensing necessity, but the project was built clean-room and stays that way; lifting code now would misrepresent its provenance.
- Jellyfin auth uses **only** the `Authorization: MediaBrowser ..., Token="..."` scheme (legacy `X-Emby-*` headers are being removed server-side). `userId` goes in query params, not paths.
- All user-facing strings live in `messages/{en,de}.json` (Paraglide; `import { m } from "#lib/paraglide/messages.js"`). No hardcoded UI strings, including aria-labels.
- No Windows-only shortcuts in architecture — Linux/macOS stay possible (e.g. souvlaki also does MPRIS).
- **The app is AMOLED dark, one theme, no light mode.** UI colors only via the design tokens in `src/app.css` (`@theme` block), never hardcoded hex in components. The surface ladder is `base` (`#000`, window ground) → `panel` (chrome) → `card` (panes) → `panel-2` (hover/raised), all **opaque** — a translucent fill over black reads grey. `edge` is the hairline. Never use `white`-alpha utility classes (`bg-white/10`, `ring-white/10`, `accent-white`) for highlights/hovers — use the `ink` token (`bg-ink/10`, `accent-ink`, `text-(--color-base)`); `src/lib/designTokens.test.ts` fails the gate on any `-white/` utility. Opaque `text-white` on a colored surface (a red button, a generated hue gradient) is fine.
- **Never write `text-base`.** Because of the `--color-base` token, Tailwind compiles it to `color: var(--color-base)` — text in the background color, and no font size. 16px is `text-md` (own token); the base color as text is `text-(--color-base)`. `src/lib/designTokens.test.ts` fails the gate on any `text-base`.
- **Two surface classes carry the look** (`app.css`, unlayered so a `shadow-*` utility can't drop them): `.surface` is the chrome hairline (sidebar, `main`, player bar, queue, lyrics), `.overlay` is the floating one (fill + backdrop blur + hairline + depth) for dialogs, popovers, the palette, toasts and the download panel — it brings its own background, so don't add `bg-panel-2` next to it. Radii are capped by the panel they sit in: `rounded-panel` (12px) for chrome, `rounded-overlay` (10px) for overlays, nothing rounder inside. The `short:` variant (viewport ≤ 56rem high) hides secondary rows in low windows — the app allows 600px.
- **Floating UI declared inside a page goes to `<body>`.** `.overlay` is the only surface left with a `backdrop-filter`, which makes it the only containing block for `position: fixed` — anything fixed rendered inside one would land offset and scrolled away. Use `Popover.svelte` for menus/popovers and the `portal` attachment (`lib/portal.ts`) for modals, always on an element *inside* a wrapper that stays in place (never on a block's top-level node); that also keeps the overlays themselves out of each other.
- **Remote visualizer presets are code.** Butterchurn compiles preset equations with `new Function`, in a window that can invoke every command. The online "Butterchurn Weekly" entries stay behind the explicit, persisted opt-in (`sonarune.vis.remotePresets`, default off, `controller.ts` `#loadEntry` is the single gate) — never list or load a remote preset without it. They are downloaded in Rust (`fetch_weekly_preset`, `commands_visualizer.rs`: fixed S3 base, no redirects, size cap, gzip) and handed to the WebView only when their SHA-256 matches the pinned manifest `src-tauri/src/weekly_presets.json`; after updating `butterchurn-presets-weekly` (or on a "changed on the server" error) regenerate it with `node scripts/pin-weekly-presets.mjs` and review the diff.
- **Persisted names are user data.** The app `identifier` (it names the data directories and the WebView2 profile), the database file, the keyring service and the `localStorage` keys (`sonarune.*` / `sonarune:*`) decide where an install's state lives. Renaming one without a migration silently signs users out and resets their settings. `src-tauri/src/legacy.rs` and the pre-paint script in `app.html` carry the Jellysic → Sonarune move; follow that pattern (move only while the new location is empty, release builds under the real identifier only) and add a test.
- **A Content-Security-Policy is set** (`tauri.conf.json` `security.csp`, plus `devCsp` with the Vite HMR socket): scripts only from the app (`'unsafe-eval'` stays for Butterchurn), images from the app, `data:`/`blob:` and `jfimg`, connections only to IPC and `jfimg`. A new origin, asset type or worker needs a matching CSP change — and a wrong CSP shows up only in the *installed* build (blank window or silently missing images/visuals), so check a release build after touching it.

## Architecture

Two worlds connected by Tauri commands (UI → Rust) and events (Rust → UI):

**Rust (`src-tauri/src/`)**
- `api/` — thin typed Jellyfin client (`JellyfinClient`): auth, items/albums/search, images, playback reporting, stream URL building. Jellyfin wire DTOs are PascalCase (`api/types.rs`), everything sent to the frontend is re-mapped to camelCase DTOs.
- `player/` — the heart. A dedicated OS thread (`sonarune-player`, `player/mod.rs`) owns the rodio output, the **queue (single source of truth)**, playback reporting, and Windows SMTC (souvlaki, needs the main window HWND). It receives `PlayerCommand`s over an mpsc channel; a 400 ms `recv_timeout` tick drives position events, throttled progress reports (~10 s), prefetch scheduling, and end-of-track bookkeeping. Sources are opened **asynchronously** on short-lived threads (`player/source.rs`) and delivered back as `SourceReady` commands; a `generation` counter invalidates stale opens — never block the command loop. **Gapless**: the next track is opened ~20 s before the boundary and appended to the same `rodio::Player` ~3 s before it; `sink.len()` dropping to 1 signals the transition (rodio's `get_pos` tracks the current source, so position resets automatically). **Crossfade** (opt-in via `PlaybackSettings`) instead overlaps the tracks on a second `rodio::Player` with gain ramps from `player/fade.rs` (a sample-accurate `FadeSource` wrapper steered via a shared handle — also used for click-free pause/resume). Seeking an unseekable live transcode — and any reopen at a position, e.g. after an output-device switch — goes through `restart_at`: a transcode restarts via `startTimeTicks`, a direct-play file (Jellyfin ignores `startTimeTicks` there) is positioned on the open thread before it reaches the audio graph, and `position_offset_ms` tracks media time. Native seeks run only once the whole file is downloaded (`DownloadProgress::is_complete`): rodio executes them on the audio callback, where a decoder waiting for an undownloaded range stalls all audio — every other seek reopens. Queue edits bump only `prefetch_generation`; bumping `generation` there would also drop the current track's own open. Output device is user-selectable; `open_output` installs a cpal error callback that falls back to the default device (debounced) and resumes at the current position. Decoders: Symphonia via `rodio::Decoder` (flac/mp3/vorbis/wav, gapless flag on) or our libopus-backed `player/opus.rs` for Ogg-Opus (probe-based, Symphonia fallback). Source chain: decoder → `player/dsp.rs` (normalization, 10-band EQ, clamp; live via revision counter) → `player/fade.rs` → `player/tap.rs`.
- Lyrics: `GET /Audio/{id}/Lyrics` mapped to a camelCase `LyricsDto` (line + word-cue timestamps in ms); rendered by `src/lib/components/LyricsView.svelte` (panel + fullscreen variants, auto-scroll, click-to-seek) fed by `src/lib/state/lyrics.svelte.ts` (LRU cache, plus the per-track manual sync offset shared by every lyrics view and persisted via `lib/lyricsOffset.ts`) and `player.estimatedPositionMs()` (extrapolates between 400 ms state events). `src/lib/ambient.ts` extracts two cover colors for the now-playing backdrop — this needs the `Access-Control-Allow-Origin: *` header on jfimg responses (canvas taint).
- Session sharing: `SharedSession = Arc<RwLock<Option<Arc<JellyfinClient>>>>` is shared between commands and the player worker (worker uses `blocking_read`; it's a plain OS thread).
- `store.rs` — SQLite key/value store (settings, stable DeviceId, volume, persisted queue snapshot). Tokens go in the Windows Credential Manager (keyring), **not** SQLite — that includes the ListenBrainz token (`commands::listenbrainz_token()`; export/import strip it, `get_extras_settings` only returns a `listenbrainzConfigured` flag, never the token).
- `proto.rs` — `jfimg://` protocol: authenticated, disk-cached cover art for the WebView (`http://jfimg.localhost/{itemId}/{tag}/{size}` on Windows). SMTC artwork reuses this disk cache via `file://` URLs.
- `player/tap.rs` — visualizer PCM bridge. A `TapSource` wrapper (outermost in the source chain) folds any channel layout to **stereo** (mono duplicated, surround channels mixed into both sides) and ships 2048-frame interleaved L/R frames over a Tauri IPC channel (`visualizer_subscribe` command, binary `InvokeResponseBody::Raw`; header: sample rate, channel count — decoded in `src/lib/visualizer/pcm.ts`). It runs on the audio callback thread, so sends go through a bounded queue to a forwarder thread (frames drop rather than block). Near-zero overhead when no visualizer is open (an atomic load and a sample counter, which keeps L/R aligned when a feed starts mid-stream). Frontend side: `src/lib/visualizer/` feeds a **muted** AudioWorklet graph (`static/visualizer-worklet.js`, stereo ring buffer with ~100 ms latency clamp) whose L/R reach Butterchurn's separate left/right analysers (MilkDrop WebGL presets) on the `/visualizer` route; energy/beat detection and the `SpectrumBar` FFT run on the mid signal.
- `commands.rs` — invoke surface (session, queue, settings). Queue mutations go through identity-checked commands (e.g. remove takes `(index, itemId)` to survive stale UI indices); the queue panel shows the play order (`QueueSnapshot.order` while shuffling, `lib/queueOrder.ts`), so `queue_move` takes play-order positions (under shuffle it reorders only the shuffle order) while jump/remove take stored indices; shuffle/repeat/album-shuffle modes, played/duplicate cleanup, and a single-step queue undo (snapshot invalidated by any foreign queue edit) all live in the player worker. `commands_library.rs` (playlists — reorder/remove/transfer by `entryId`, not item id —, favorites, home rows, collections/BoxSets read-only) and `commands_discover.rs` (genres, A-Z scrubber via `nameLessThan` counts, similar artists, the Discover hub, decade rows, and dynamic **smart-view** queries — `SmartFilter` → server filters, materializable to a static playlist; `query_smart_view` lets the server page when it can evaluate the whole filter, otherwise it scans once — `addedSince` stops the scan early — and serves later pages from a short-lived id cache on the client) are separate modules; **all** server commands wrap `with_retry` (401 → auto-relogin → retry).
- Home rows quirk: playback updates Played/PlayCount/DatePlayed only on **tracks**, never albums — `home_albums` queries tracks for the played-based rows and maps them to their albums.
- Playback reporting: the **play session belongs to one attempt at a track, not to the queue line**. `spawn_open` mints it, `source.rs` appends it to the `/Audio/{id}/universal` URL next to `startTimeTicks`, and the `/Sessions/Playing*` reports name the same one, so the server can correlate and kill transcode jobs. A reopen (seek in a transcode, device switch) continues the same session; a Play or Prefetch starts a new one — the server treats a session it saw stopped as a finished playback, so replaying an entry must not reuse it. The queue line's own `entry_id` is a separate, stable id that never leaves the app: the queue UI keys rows by it and tells two copies of the same track apart (it used to be the play session id, which is why old persisted queues are read through a serde alias).
- Extras (`player/scrobble.rs`, Auto-DJ in `player/mod.rs`): ListenBrainz playing-now on `report_start`, listen on `report_stopped` when the ≥4 min/≥50 % rule passes; Auto-DJ (track/artist/genre/album/playlist seed modes) appends an instant mix when the last queued track enters its final 30 s (latched per seed, released on failure, resumes playback if the queue ended before the mix arrived, suppressed while an end-of-track sleep is armed). Sleep timer supports an optional fade-out (`player/fade.rs`).
- `download.rs` — original-file export (not offline cache): streams to a collision-safe `*.part` file and atomically renames on success; two concurrent slots (`Semaphore`), progress events, cancel (`AtomicBool` + `Notify`), and retry with an `attempt` generation (the attempt is part of the `.part` name so a retry cannot collide with a just-cancelled run's file). Stale `.part` files older than 24 h are swept on startup.
- `cache.rs` — `CoverCache`: managed disk cache for `jfimg` cover art with a user-configurable MB limit and coalesced background LRU eviction (mtime order, evicts to 80 % headroom). `clear()` only removes regular files directly in the cache dir.
- Desktop/background: `tray.rs` (system tray + background mode — show/hide, current title, prev/play-pause/next; close-to-tray vs quit and start-minimized honor `DesktopSettings`; `tauri-plugin-single-instance`, registered first in `lib.rs`, hands a second launch over to the running window), `desktop.rs` (`DesktopSettings` in `store`: close behavior, start minimized, cache limit), `audio_devices.rs` (output device list + runtime fallback-device state for hotplug), `diagnostics.rs` (redacted 500-line log ring feeding the diagnostic export — URLs, credentials, absolute paths, and long hex ids are dropped wholesale; quoted dynamic values become `[value]`).

**Svelte (`src/`)**
- SvelteKit SPA (adapter-static, `ssr = false`), routes under `src/routes/`.
- `lib/state/*.svelte.ts` — runes-based stores. `player.svelte.ts` mirrors Rust state by listening to `player:state`, `player:queue`, `player:error` events; UI never computes playback state itself. Use `player.run(promise)` for fire-and-forget commands so rejections surface as the error banner.
- `lib/api.ts` — typed `invoke()` wrappers; arg names are camelCase (Tauri maps to snake_case Rust params).
- `lib/types.ts` must stay in sync with the Rust DTOs (there is no codegen).
- Async fetches in routes need stale-response guards (capture the request id/param and compare before assigning state — see album page `$effect` and search `requestId` pattern). `lib/swr.ts` is the shared stale-while-revalidate cache: detail/overview routes render the last snapshot instantly, then background-refetch.
- Discovery/library routes: `/discover` (hub) with `/discover/{random,mixes,long-not-heard,decades,decade/[year],artists}` drill-downs, `/smart` (dynamic smart views, saved locally per server/user via `lib/smartViews.ts`), `/collections` + `/collection/[id]` (read-only BoxSets), `/recently-added`. Rust DTOs for these are in `lib/api/discovery.ts`.
- Global shortcuts (`lib/shortcuts.ts`, installed by the root layout for the signed-in shell) are **user-configurable**: bindings live in `lib/state/shortcuts.svelte.ts` (`shortcutSettings`, persisted, with conflict/reserved-key checks) and are edited in `ShortcutSettings.svelte`; `shortcuts.ts` resolves a `KeyboardEvent` → action via the store. Defaults: Space play/pause, Ctrl+K/Ctrl+P palette, Ctrl+←/→ prev/next, plus N/P, ←/→ seek, ↑/↓ volume, M/L/S/R. Handlers bail on `event.defaultPrevented` — row widgets that handle keys themselves just `preventDefault()`.
- View density / layout presets: `lib/state/layout.svelte.ts` + `LayoutSettings.svelte` (compact/comfortable lists, grid card size, sidebar width; persisted per view).
- Theming: `lib/theme.ts` + a pre-paint script in `app.html` (kept in sync by hand) set the accent vars from `localStorage["sonarune.theme"]` — there is no mode and no `data-theme`. `accentMode: "cover"` is the **default**: the accent is derived from the playing cover (`ambient.ts` → `setCoverHue`, lightness walked up to ≥3:1 against `--color-base`, last result cached under `sonarune.coverAccent.v2` for the pre-paint script) and fires `ACCENT_CHANGED` for canvas drawers; the stored fixed accent stays the fallback for grey artwork. `applyTheme` never paints `<html>` (frameless transparent window). `--cover-a`/`--cover-b` tint `.app-root::before`, which the opaque panels reduce to a thin colored seam around the chrome.
- Player UI building blocks: `SeekBar` (buffered range from `PlayerState.bufferedMs`, a whole-track waveform once `player/waveform.rs` has analysed the fully downloaded temp file — `player:waveform` event / `get_waveform` —, hover time, keyboard), `PlaybackProgress`, `TransportControls` — shared by the player bar and now playing. `Toaster` has success/info/error kinds (`toast.show(msg, { kind })`); page empty states use `EmptyState`; reorderable lists animate with `animate:listFlip` (`lib/motion.ts`).
- Extra frameless `WebviewWindow`s: the mini player on `/mini` and the visualizer projector on `/projector` (`lib/visualizer/projector.ts`, opened from the visualizer route onto a chosen monitor). The root layout renders those routes bare. **Command access is per window** (Tauri ACL): `build.rs` reads `generate_handler!` in `lib.rs`, declares every command as permissioned and generates the `main-window` set (all commands) into `src-tauri/permissions/main-window.toml` (git-ignored). `capabilities/main.json`, `mini.json` and `projector.json` each grant exactly one window; the mini and projector windows only get the hand-picked sets in `permissions/windows.toml`. A new command is available to the main window automatically — if `/mini` or `/projector` (or anything they import, like `player.init()` or the lyrics store) calls it, add `allow-<command-with-dashes>` to that window's set, or the call fails at runtime with "not allowed". A new window needs its own capability file. `acl_tests` in `lib.rs` checks the sets against the invoke handler.

## Manual check in the app

The gates prove the code builds and the tests pass, not that playback works.
For anything that touches playback, the session or the queue, run the app
against a real Jellyfin 10.11 server (or `scripts/devserver/`) and exercise
the flow: play an album, check that the Jellyfin dashboard shows the session,
restart the app to confirm the queue comes back. The red banner in the UI
surfaces player errors.
