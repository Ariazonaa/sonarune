# End-to-end tests (WebDriver)

These drive the **real built `jellysic.exe`** through WebView2, the way a user
would. They complement the fast Vitest/`cargo test` gates by covering flows that
only exist in the running desktop app.

Pipeline: `wdio` → `tauri-driver` → `msedgedriver` → WebView2 → the SvelteKit
SPA. This is the hand-rolled `tauri-driver` setup (the most Windows-proven
path). Config: [`wdio.conf.mjs`](./wdio.conf.mjs).

## What's covered

| Spec | Needs a signed-in app? | Covers |
|------|------------------------|--------|
| `specs/boot.e2e.js` | no | The app launches, WebView2 loads, the SPA mounts and reaches a known-good landing view (setup screen **or** — if a session was restored — the signed-in shell). Proves the whole build→driver→WebView pipeline. |
| `specs/visualizer.e2e.js` | yes | The **visualizer mounts (canvas) and tears down** cleanly with no error banner — a Phase-13 GUI core flow. Gets to the signed-in shell via a restored session (dev machine) or, on a clean machine/CI, by signing in with `JELLYSIC_TEST_{URL,USER,PASS}`. Skips if it can't reach the shell. |

`ensureSignedIn()` in `helpers.js` encapsulates that restored-session-or-login
logic. The **dialog focus trap** core flow is covered faster and more reliably
as a Vitest component test (`src/lib/components/ConfirmDialog.test.ts`), not
here.

Verified green end-to-end on Windows (real app, restored session).

**Still manual** (see `docs/testing.md`), by design:
- **Mini-player** and **projector** — separate `WebviewWindow`s; a single
  WebDriver session attaches to the main window only and can't reach them.
- **Drag & drop reorder** — HTML5 DnD is not reliably reproducible through
  WebDriver's input synthesis.

## Prerequisites (Windows)

1. **Rust + cmake on PATH** (same as a normal build — the `opus` crate builds
   libopus via cmake).
2. **tauri-driver:**
   ```sh
   cargo install tauri-driver --locked
   ```
   Lands at `%USERPROFILE%\.cargo\bin\tauri-driver.exe`.
3. **msedgedriver.exe matching the installed WebView2 major version.** Check
   the installed version with:
   ```powershell
   Get-ChildItem "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients" |
     ForEach-Object { Get-ItemProperty $_.PSPath } |
     Where-Object name -match 'WebView2' | Select-Object name, pv
   ```
   Download the matching driver from
   <https://developer.microsoft.com/microsoft-edge/tools/webdriver/> and either
   put it on PATH or pass its path via `MSEDGEDRIVER` (below). WebView2
   auto-updates, so **expect to refresh the driver** whenever the major version
   moves — a stale driver fails the run outright. The zip lives at
   `https://msedgedriver.microsoft.com/<version>/edgedriver_win64.zip`, and
   `https://msedgedriver.microsoft.com/LATEST_RELEASE_<major>_WINDOWS` gives the
   newest build of a major line if the exact runtime version isn't published.

## Run

```sh
npm run test:e2e
```

By default this builds the debug binary first (`tauri build --debug --no-bundle`)
and then runs every spec.

### Env knobs

| Var | Effect |
|-----|--------|
| `E2E_SKIP_BUILD=1` | Skip the debug build (the binary must already exist). Use while iterating on specs. **Not with a binary `tauri dev` produced**: that one is compiled to load the frontend from `devUrl` (`localhost:1420`), so without Vite running it opens a blank window and every spec fails with "neither the setup screen nor the app shell mounted". The skip is only safe after a `tauri build --debug`, which embeds the frontend. |
| `E2E_APP_BINARY=<path>` | Use a specific `jellysic.exe` instead of the resolved debug build. |
| `MSEDGEDRIVER=<path>` | Path to `msedgedriver.exe` if it isn't on PATH. |
| `JELLYSIC_TEST_URL` / `JELLYSIC_TEST_USER` / `JELLYSIC_TEST_PASS` | Let `visualizer.e2e.js` sign in when no session is restored. Use a throwaway/test library. |

The binary path is resolved via `cargo metadata`, so the SMB-share target-dir
redirect in `src-tauri/.cargo/config.toml` is handled automatically.

## Notes / gotchas

- **WebView2 is not headless** — a real interactive desktop session is required
  (fine locally and on the `windows-latest` CI runner; no Xvfb needed).
- The app **must not already be running** — WebDriver launches its own instance.
  Kill stray `jellysic.exe` / `tauri-driver` / `msedgedriver` between runs.
- Selectors use `data-testid` anchors (`setup-screen`, `setup-server`,
  `setup-username`, `setup-password`, `setup-connect`, `app-shell`,
  `visualizer`, `visualizer-toggle`). Keep those stable.
