# Contributing

Issues and pull requests are welcome. This page is the long version of the
Contributing section in the [README](README.md).

## Setting up

You need **Rust** with the MSVC toolchain and the C++ build tools, **Node.js
22+**, and **cmake** on `PATH` — the `opus` crate builds a bundled libopus, and
without cmake the build stops there.

```powershell
npm install
npm run tauri dev
```

You do not need a Jellyfin server to work on most of the app. `npm run tauri
dev` against a real server is the fastest path if you have one; otherwise see
below.

## Working without a server

There is a stand-in Jellyfin in the repository. It speaks enough of the API for
the app to sign in, browse, play and visualize, and it generates its own demo
library — invented artists and albums, abstract cover art, short audio files.

```powershell
node scripts/devserver/server.mjs --verbose
```

Then sign the app in at `http://127.0.0.1:8096` with any username and password.
`--verbose` prints every request, which is the quickest way to see what a
feature actually asks the server for. Details in
[`scripts/devserver/README.md`](scripts/devserver/README.md).

## Before you open a pull request

**`npm run verify` has to pass.** It is the same set CI runs, fail-fast:

| Gate | What it means |
| --- | --- |
| `svelte-check` | zero errors **and** zero warnings |
| Vitest | frontend unit tests |
| `cargo fmt --check` | formatting, not negotiable |
| `clippy -D warnings` | warnings are errors here |
| `cargo test --lib` | Rust unit tests, including property tests on the queue |
| production build | catches what only breaks when bundled |

End-to-end tests drive the real built app through WebView2 and are not part of
that gate; they need a driver setup described in [`e2e/README.md`](e2e/README.md).
Run them when you touch anything the app does at startup, in the visualizer, or
around windows.

## Rules that are not style preferences

Break one of these and the patch gets sent back, however good the rest is.

**The WebView never touches audio or HTTP.** Every Jellyfin request — sign-in,
browsing, cover art, audio streams — goes through Rust and `reqwest`. That is
why plain HTTP and self-signed home servers work, and why tokens never reach
the DOM. Audio is decoded and played in Rust.

**No hardcoded user-facing strings.** Everything goes through
`messages/{en,de}.json` and Paraglide, aria-labels included. A new string needs
both languages.

**Colors come from the design tokens** in `src/app.css`, never as literals in a
component. The app is AMOLED dark: a true-black ground and a short ladder of
near blacks, separated by hairlines. Two gates enforce parts of this — no
`text-base`, which compiles to the background color here, and no `-white/`
utilities, which hardcode what the `ink` token carries.

**Certificates are pinned per server.** Do not reintroduce a blanket "accept
invalid certificates" flag. New HTTP clients go through `tls::apply`.
[`SECURITY.md`](SECURITY.md) explains what the rest of that model assumes,
and is where a vulnerability goes instead of into an issue.

## Commits and pull requests

Describe what the change actually does. Length is free, accuracy is not. If
something is a workaround, say so and say why — the next person will otherwise
spend an afternoon rediscovering it.

Small, focused pull requests get reviewed faster than large ones. If a change
touches the audio path, say how you tested it: which formats, gapless or
crossfade, and whether you watched the session appear on the Jellyfin
dashboard.

## Provenance

Jellysic is written clean-room: no code or assets are taken from Feishin, only
concepts. The licence now permits more than that, but the provenance stays as
it is. If you port an idea from another client, say so in the pull request.
