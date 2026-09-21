# Testing & quality gate

The automated barrier is one command:

```bash
npm run verify
```

It runs every gate CI runs, fail-fast, in this order: `svelte-check` → frontend
unit tests → `cargo fmt --check` → `clippy --all-targets -D warnings` →
`cargo test --lib` → frontend production build. Rust gates need **cmake + cargo**
on PATH (the `opus` crate builds bundled libopus); the frontend gates need
`npm install` first.

CI runs the same gates on every push/PR via `.github/workflows/ci.yml` on a
**Windows** runner labelled `windows-latest`. The workflow installs Node, Rust (plain `rustup` in
PowerShell) and cmake itself and avoids bash — also inside composite actions,
because on the runner `bash` resolves to WSL's launcher, which fails there — and
the actions/cache backend, so the runner only needs: the
`windows-latest` label, the **MSVC C++ build tools + Windows SDK** (for the Rust
msvc linker), and outbound internet (the actions are fetched from github.com).
The workflow sets `CARGO_TARGET_DIR`, which overrides any local target-dir
redirect in `src-tauri/.cargo/config.toml` (see `config.toml.example`).

## Automated tests

**Rust (`cargo test --lib`, in `src-tauri/`)** — DSP, opus decoder, fades,
scrobble rules, null-sink gapless continuity, queue helpers (shuffle/undo/dedup
and index/permutation remap), the smart-view resolver (incl. which filters page
on the server, the added-since early stop and the page cache), the seek-bar
buffered range and waveform (peak folding, decoding a real WAV through the
playback decoder path, one analysis per track, cache before notify), the
stereo visualizer tap (mono/stereo/5.1 folding, frame layout, audio passes
through untouched, a feed that starts mid-frame keeps left left), cert
fingerprint/normalize/format validation, certificate pinning in the store
(replace on change, forget, legacy entries, pin only the presented certificate
after login), and download part-file naming. Queue-index invariants also have **property tests**
(`proptest`): a shuffle order is always a `0..len` permutation with the current
track first; `remap_move` is a bijection; removals keep the order a valid
permutation of the survivors and preserve their relative play order.

**Frontend (`npm run test`, Vitest + jsdom)** — pure logic and the Tauri bridge:
- `lib/visualizer/projector.test.ts` — settings normalization/clamping, overlay
  visibility timeouts, monitor selection fallback, burn-in transform.
- `lib/smartViews.test.ts` — filter sanitization and per-server/user persistence.
- `lib/eqPresets.test.ts` — custom EQ presets: gains clamped/snapped to the
  slider, same name (any case) replaces, the limit, broken storage entries
  dropped.
- `lib/designTokens.test.ts` — scans every source file (Vite glob): no
  `text-base`, which compiles to the background *color* here because of the
  `--color-base` token, and no `-white/` utilities, which hardcode what the
  `ink` token carries.
- `lib/state/shortcuts.test.ts` — event→binding normalization, reserved-key
  guarding, conflict detection (exercises a Svelte `.svelte.ts` rune module).
- `lib/api.test.ts` — mocks `@tauri-apps/api/core` `invoke` and asserts each
  wrapper's command name + camelCase payload (the Tauri command-mock pattern for
  frontend integration tests).
- `lib/components/Popover.test.ts` — the popover/portal contract: renders into
  `<body>`, clamps to the viewport, dismisses on outside press/scroll/Escape but
  not on its anchor, and leaves no node behind — including when it shares its
  `{#if}` block with a sibling (via `Popover.harness.svelte`).
- `lib/seek.test.ts`, `lib/motion.test.ts`, `lib/state/toast.test.ts` — seek-bar
  math, FLIP limits + unique row keys, toast kinds and durations.
- `lib/visualizer/pcm.test.ts`, `lib/visualizer/worklet.test.ts` — the stereo
  tap frames: decoding, per-channel resampling (left never leaks into right),
  and the real `static/visualizer-worklet.js` run under stubbed worklet
  globals (L/R to channels 0/1, silence when dry, latency skip keeps pairs).
- `lib/theme.test.ts` — the cover accent keeps ≥3:1 against both backgrounds
  for every hue and a readable text color on top; accent pick from a cover;
  the stored cover accent survives start-up until a cover is analysed;
  `<html>` is never painted.
- `lib/visualizer/presets.test.ts` — the online-preset opt-in: a remote preset
  is never downloaded without it, rotation leaves remote entries out until
  they are enabled.

## Live server contract (`#[ignore]` smoke test)

`api/mod.rs` mod `smoke` exercises the real 10.11 server contract for the
browse/playlist/artist/stats and discovery/smart-view/collection flows (creates
and deletes a temporary "delete me" playlist). For smart views it also asserts
that a server-paged page is exactly the full scan's slice (members, order,
total) and that scan pages served from the cached ids continue seamlessly:

```bash
JELLYSIC_TEST_URL=… JELLYSIC_TEST_USER=… JELLYSIC_TEST_PASS=… \
  cargo test --lib smoke -- --ignored --nocapture
```

## End-to-end (WebDriver) — the real app

`npm run test:e2e` drives the built `jellysic.exe` through WebView2
(`wdio` → `tauri-driver` → `msedgedriver`). Covers the **boot** smoke (no
server) and the **visualizer mount/teardown** flow; the latter reaches the
signed-in shell via a restored session or, on a clean machine, by signing in
with `JELLYSIC_TEST_*`. Prerequisites (tauri-driver, a WebView2-matching
msedgedriver), env knobs, and the covered-vs-manual split live in
[`e2e/README.md`](../e2e/README.md). The **dialog focus trap** flow is covered
instead as a fast Vitest component test
(`src/lib/components/ConfirmDialog.test.ts`).

## Manual live-test matrix (Jellyfin 10.11.x)

Not automatable — verify before a release. Run the app (`npm run tauri dev`)
against a real server and confirm:

| Area | What to check |
| --- | --- |
| Playback | Play an album; the Jellyfin dashboard shows the session; gapless + crossfade transitions; seek in a live transcode. |
| Queue restore | Restart the app → the last queue returns (paused). |
| Mini player | `/mini` window opens, controls mirror the main window. *(Separate window — not E2E-reachable.)* |
| Visualizer projector | `/projector` opens on the chosen monitor, overlays + preset-lock work, closes cleanly (tap teardown). *(In-window visualizer teardown is E2E-covered; the projector window stays manual.)* |
| Drag & drop | Reorder a playlist and the queue; order persists. Drag a track row, a multi-selection and an album card onto a playlist in the sidebar — it lights up, the tracks arrive, and a file dragged in from outside does not light it at all. *(HTML5 DnD not reliably reproducible via WebDriver.)* |
| Dialogs / focus | Delete/confirm dialogs trap focus; no Enter-to-confirm on destructive actions. *(Now automated: `ConfirmDialog.test.ts`.)* |
| TLS (self-signed) | Connect to a self-signed server → fingerprint prompt → trust → reconnect; a changed cert is rejected. Confirm with a **wrong password** → nothing is pinned (Settings → Trusted certificates stays empty). Confirm a **changed** cert → the list shows only the new one. **Forget** a pin of another server → entry gone; of the connected server → confirm dialog, sign-out, and the next sign-in prompts again. First sign-in with "trust automatically" ticked goes through without a prompt; afterwards a changed cert still prompts. Signing in with the box ticked to a server with a *valid* certificate must not keep the box stored. No self-signed server at hand: `node scripts/tls-test-proxy.mjs https://your.server` puts self-signed certificates in front of it (`https://localhost:8921`, a second "server" on `:8923`; switch certificates with `http://127.0.0.1:8922/use?port=8921&cert=B`). Sign out via Settings → Account. |
| Smart views | A filter without play count / "added since" / open year range pages via the server (one request per "load more"). With "added since" the view is fast even on a large library; "load more" on a play-count filter does not rescan. |
| Audio device hotplug | Unplug/replug the selected output device → fallback toast, no unsolicited switch-back. |
| Ambient / visualizer backdrop | Now-playing backdrop renders (cover colors / cover / Butterchurn); controls stay readable in light mode — also for a track without cover. The whole view fits a 600px-high window (art shrinks, "up next" hides), with and without lyrics; the lyrics sync row stays visible. |
| Menus & dialogs | In a long, scrolled list: the context menu opens at the pointer; song info, metadata editor and playlist dialogs sit centered over the whole window. |
| Seek bar | Buffered range grows while loading (none for a live transcode); the waveform replaces the line once the track is fully downloaded (not for a live transcode, not after seeking past the downloaded part); hover time, drag, keyboard seek once per key. No `jellysic-*` files left in the temp folder afterwards. |
| Accent from cover | Light + dark with a bright, a dark and a grey cover; spectrum bars recolor. Window corners stay transparent after opening settings. |
| Stop after / normalization | Arm "stop after this track" and "stop after this album" from a queue row: the marked row is where the music ends, without a fade, and with the next track already appended (queue edited after the hand-off) it pauses at the start of the next one instead. Album mode for normalization: an album with a quiet and a loud track keeps that difference, per-track mode levels it out. |
| Play sessions | With repeat-one, and after jumping back to a track that already played: the Jellyfin dashboard shows one live session per attempt and no stale one, and a seek inside a transcode does not start a second. |
| Updates | Only checkable from an **installed** build against a published release, and not in the first minutes after publishing — GitHub serves the `releases/latest` redirect from a cache, so a check in that window still gets the release before it and says "up to date" (seen with 0.3.0: about two to three minutes). After that: Settings → Updates finds the newer version, the install refuses while a track plays, and after pausing it downloads, closes the app, installs and comes back on the new version with the queue restored. With the automatic check off, nothing is requested at startup. In the portable build the section says it cannot update itself. |
