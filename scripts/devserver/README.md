# Dev server — a stand-in Jellyfin

A small Node server that speaks just enough of the Jellyfin API for the app to
sign in, browse, play and visualize. It exists so development, demos and the
README screenshots do not need a real server or anyone's real music.

```sh
node scripts/devserver/server.mjs             # http://127.0.0.1:8096
node scripts/devserver/server.mjs --verbose   # log every request
```

`--verbose` prints one line per request. That is how you watch playback
reporting arrive: `/Sessions/Playing/Progress` lands every ten seconds from the
player thread, and it keeps landing when the window is hidden, because the
audio does not live in the WebView.

Then sign the app in at `http://127.0.0.1:8096` with **any** username and
password — authentication is not checked.

## What's in the library

Eight invented artists, ten albums, 56 tracks, two playlists, six genres
(`fixtures.mjs`). Nothing is a real recording and no name refers to anything.

Media is generated on first run into `~/.cache/jellysic/devserver-media`, not
into the repo:

- **Covers** — abstract sleeve art, drawn with `pngjs` (already a devDependency)
  and seeded from the album id, so an album always gets the same cover. No text,
  which is why no font rasterizer is needed.
- **Audio** — 45 s FLAC per track via **ffmpeg**: three sine partials with a
  slow tremolo. Silence would make the visualizer and the spectrum bar look
  broken, so there is something with structure to react to.

`ffmpeg` must be on `PATH` for the audio step. Delete the cache directory to
regenerate.

## What it implements

Sign-in, `/Users/Me`, `/Items` (albums, tracks, artists, playlists, with the
filters and sorts the app sends), `/Artists`, `/Genres`, `/Years`, instant mix,
similar artists, playlist contents, favorites, cover images, audio streaming
with `Range` (so seeking works) and the `/Sessions/Playing*` reports.

It is **not** a Jellyfin implementation: it ignores authentication, keeps
everything in memory, and answers anything it does not know with an empty page
rather than an error. Unhandled routes are logged, which is the quickest way to
find out what a new feature asks the server for.

## Screenshots

The README images come from this library, captured through the real app:

1. Start the server.
2. Build a binary with its own app identifier, so it gets its own data
   directory and cannot touch a real session:
   ```sh
   npx tauri build --debug --no-bundle --config '{"identifier":"io.jellysic.demo"}'
   ```
3. Drive it with a WebDriver spec that signs in via
   `JELLYSIC_TEST_{URL,USER,PASS}` and calls `browser.saveScreenshot()` (see
   `e2e/README.md` for the harness and its prerequisites).

Redirecting `APPDATA` does **not** isolate the app: Tauri resolves the data
directory through `SHGetKnownFolderPath`, which ignores the variable. The
separate identifier is what does it.
