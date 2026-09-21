// WebdriverIO end-to-end harness for the built Tauri app (Windows).
//
// Pipeline: wdio -> tauri-driver (spawned below) -> msedgedriver -> WebView2,
// which launches the real `jellysic.exe` and drives the SvelteKit SPA inside
// it. This is the hand-rolled tauri-driver setup (the most Windows-proven
// path); see e2e/README.md for prerequisites and how to run it.
//
// Env knobs:
//   E2E_SKIP_BUILD=1        skip the debug build in onPrepare (binary must exist)
//   E2E_APP_BINARY=<path>   point at a specific jellysic.exe
//   MSEDGEDRIVER=<path>     msedgedriver.exe if it is not already on PATH
//   JELLYSIC_TEST_URL/USER/PASS   enable the authenticated spec (else it skips)

import os from "node:os";
import path from "node:path";
import { existsSync } from "node:fs";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const projectRoot = path.resolve(__dirname, "..");
const srcTauri = path.join(projectRoot, "src-tauri");
const isWindows = process.platform === "win32";
const exe = isWindows ? ".exe" : "";
const cargoBin = path.join(os.homedir(), ".cargo", "bin");

// Cargo's target dir is redirected off the SMB share by src-tauri/.cargo/
// config.toml, so the debug binary is NOT at src-tauri/target/debug. Ask cargo
// where it actually is (CARGO_TARGET_DIR/CI overrides are reflected here too),
// falling back to the in-tree default if cargo can't be reached.
function resolveTargetDir() {
  const res = spawnSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
    cwd: srcTauri,
    encoding: "utf8",
    // Make sure cargo is findable even if the npm shell PATH lacks ~/.cargo/bin.
    env: { ...process.env, PATH: `${cargoBin}${path.delimiter}${process.env.PATH ?? ""}` },
  });
  if (res.status === 0 && res.stdout) {
    try {
      return JSON.parse(res.stdout).target_directory;
    } catch {
      /* fall through */
    }
  }
  // Say why the fallback is being used. Without this the run dies much later
  // with "no msedge binary at src-tauri/target/debug/jellysic.exe", which
  // points at the wrong thing entirely: the binary is fine, it just is not
  // there. `~/.cargo/bin` is not enough on a machine where cargo lives in the
  // rustup toolchain directory and only cargo-installed tools sit in that bin.
  console.warn(
    "[e2e] could not run `cargo metadata` (is cargo on PATH?) — assuming the " +
      "in-tree target dir. If src-tauri/.cargo/config.toml redirects it, the " +
      "binary will not be found; put cargo on PATH or set E2E_APP_BINARY.",
  );
  return path.join(srcTauri, "target");
}

function resolveAppBinary() {
  if (process.env.E2E_APP_BINARY) return path.resolve(process.env.E2E_APP_BINARY);
  return path.join(resolveTargetDir(), "debug", `jellysic${exe}`);
}

const appBinary = resolveAppBinary();

let tauriDriver;
let driverShuttingDown = false;

export const config = {
  runner: "local",
  hostname: "127.0.0.1",
  port: 4444, // tauri-driver's default intermediary port
  specs: [path.join(__dirname, "specs", "**", "*.e2e.js")],
  maxInstances: 1, // parallel WebView2 sessions are unreliable — run serially
  capabilities: [
    {
      // tauri-driver reads this and launches the binary under WebDriver.
      "tauri:options": {
        application: appBinary,
      },
    },
  ],
  logLevel: "warn",
  bail: 0,
  waitforTimeout: 15000,
  connectionRetryTimeout: 120000,
  connectionRetryCount: 3,
  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    ui: "bdd",
    timeout: 90000, // WebView2 session warm-up is slow; keep this generous
  },

  // Build the debug binary once before the suite (unless told to skip). The
  // opus crate needs cmake on PATH; see e2e/README.md.
  onPrepare() {
    if (process.env.E2E_SKIP_BUILD === "1") {
      if (!existsSync(appBinary)) {
        throw new Error(
          `E2E_SKIP_BUILD=1 but no binary at ${appBinary}. Build it first or unset the flag.`,
        );
      }
      return;
    }
    const build = spawnSync(
      "npm",
      ["run", "tauri", "build", "--", "--debug", "--no-bundle"],
      { cwd: projectRoot, stdio: "inherit", shell: true },
    );
    if (build.status !== 0) {
      throw new Error(`tauri debug build failed (exit ${build.status})`);
    }
    if (!existsSync(appBinary)) {
      throw new Error(`build finished but no binary at ${appBinary}`);
    }
  },

  // Start tauri-driver before each WebDriver session.
  beforeSession() {
    const driverPath = path.join(cargoBin, `tauri-driver${exe}`);
    if (!existsSync(driverPath)) {
      throw new Error(
        `tauri-driver not found at ${driverPath}. Install it: cargo install tauri-driver --locked`,
      );
    }
    // msedgedriver must be on PATH, or point tauri-driver at it explicitly.
    const args = process.env.MSEDGEDRIVER
      ? ["--native-driver", path.resolve(process.env.MSEDGEDRIVER)]
      : [];
    tauriDriver = spawn(driverPath, args, {
      stdio: [null, process.stdout, process.stderr],
    });
    tauriDriver.on("error", (error) => {
      console.error("tauri-driver failed to start:", error);
      process.exit(1);
    });
    tauriDriver.on("exit", (code) => {
      if (!driverShuttingDown && code !== 0) {
        console.error("tauri-driver exited early with code", code);
        process.exit(1);
      }
    });
  },

  afterSession() {
    driverShuttingDown = true;
    tauriDriver?.kill();
  },
};
