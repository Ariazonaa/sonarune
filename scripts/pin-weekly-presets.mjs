// Pins the content of the online "Butterchurn Weekly" visualizer presets.
//
// Butterchurn compiles a preset's equation strings with `new Function`, so a
// preset file is code. The npm package `butterchurn-presets-weekly` only lists
// URLs (`https://s3-us-east-2.amazonaws.com/butterchurn-presets/<32 hex>.json`),
// and that 32-hex name is NOT a hash of the file — nothing anchors the bytes.
// This script downloads every listed file once, validates it (UTF-8, JSON, a
// `baseVals` object, size cap) and writes the SHA-256 of its content to
// `src-tauri/src/weekly_presets.json` ("<32hex>.json" -> "<sha256 hex>").
// The content is the decoded body: S3 serves these files with
// `Content-Encoding: gzip`, which `fetch` undoes here just as the Rust side
// does before hashing.
//
// The Rust command `fetch_weekly_preset` (`src-tauri/src/commands_visualizer.rs`)
// compiles that manifest in, fetches only listed files and hands a preset to
// the WebView only when its bytes hash to the pinned value. `presets.ts` hides
// Weekly entries that are not pinned.
//
// Run (needs network, Node 18+, no npm dependencies):
//
//   node scripts/pin-weekly-presets.mjs [--allow-missing]
//
// Re-run it when
//   - `butterchurn-presets-weekly` is added or updated (the drift check in
//     `src/lib/visualizer/presets.test.ts` fails until every listed URL is
//     pinned again), or
//   - the app reports that a Weekly preset "changed on the server" and you
//     decide to accept the new content.
// Pinning trusts whatever the server delivers at that moment: look at what
// changed (`git diff src-tauri/src/weekly_presets.json` names the files)
// before committing, because the new bytes run as code inside the app.
//
// `--allow-missing` writes the manifest without files that answer HTTP 403/404
// (gone from the bucket) instead of failing. Any other failure leaves the
// existing manifest untouched and exits non-zero.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");
const WEEKS_DIR = path.join(ROOT, "node_modules", "butterchurn-presets-weekly", "weeks");
const OUT = path.join(ROOT, "src-tauri", "src", "weekly_presets.json");
// Must match ORIGIN in commands_visualizer.rs and WEEKLY_ORIGIN in presets.ts.
const ORIGIN = "https://s3-us-east-2.amazonaws.com/butterchurn-presets/";
const FILE_NAME = /^[a-f0-9]{32}\.json$/;
// Must not exceed MAX_PRESET_BYTES in commands_visualizer.rs.
const MAX_BYTES = 2 * 1024 * 1024;
const CONCURRENCY = 8;
const ATTEMPTS = 5;
const TIMEOUT_MS = 30_000;
const allowMissing = process.argv.includes("--allow-missing");

/** A failure retrying cannot fix. */
class PermanentError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function describe(error) {
  const cause = error?.cause ? ` (${error.cause.code ?? error.cause.message ?? error.cause})` : "";
  return `${error?.message ?? error}${cause}`;
}

/** Every Weekly file name the installed package references, sorted. */
function listFiles() {
  if (!fs.existsSync(WEEKS_DIR)) {
    throw new Error(`${path.relative(ROOT, WEEKS_DIR)} not found — install the npm dependencies first`);
  }
  const sources = [path.join(WEEKS_DIR, "presets.json")];
  for (const dirent of fs.readdirSync(WEEKS_DIR, { withFileTypes: true })) {
    const candidate = path.join(WEEKS_DIR, dirent.name, "presets.json");
    if (dirent.isDirectory() && fs.existsSync(candidate)) sources.push(candidate);
  }
  const files = new Set();
  for (const source of sources) {
    const presets = JSON.parse(fs.readFileSync(source, "utf8"));
    for (const [name, value] of Object.entries(presets)) {
      if (typeof value !== "string") continue; // bundled preset, nothing to fetch
      const file = value.startsWith(ORIGIN) ? value.slice(ORIGIN.length) : "";
      if (!FILE_NAME.test(file)) {
        throw new Error(`${path.relative(ROOT, source)}: "${name}" has an unexpected URL: ${value}`);
      }
      files.add(file);
    }
  }
  return [...files].sort();
}

/** Download, validate and hash one file. */
async function pinOnce(file) {
  const response = await fetch(ORIGIN + file, {
    redirect: "manual",
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });
  if (response.status !== 200) {
    await response.body?.cancel();
    const message = `HTTP ${response.status}`;
    if (response.status === 429 || response.status >= 500) throw new Error(message);
    throw new PermanentError(message, response.status);
  }
  // The app decodes gzip only; anything else would never verify there.
  const encoding = (response.headers.get("content-encoding") ?? "identity").trim().toLowerCase();
  if (!["identity", "gzip", "x-gzip"].includes(encoding)) {
    await response.body?.cancel();
    throw new PermanentError(`unsupported Content-Encoding "${encoding}"`);
  }
  if (Number(response.headers.get("content-length")) > MAX_BYTES) {
    await response.body?.cancel();
    throw new PermanentError(`transfer exceeds ${MAX_BYTES} bytes`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length > MAX_BYTES) throw new PermanentError(`content exceeds ${MAX_BYTES} bytes`);

  let text;
  try {
    // ignoreBOM keeps a BOM in the text, so JSON.parse rejects it here just as
    // it would in the app (Rust hands the bytes over unchanged).
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw new PermanentError("not valid UTF-8");
  }
  let preset;
  try {
    preset = JSON.parse(text);
  } catch {
    throw new PermanentError("not valid JSON");
  }
  if (!preset || typeof preset !== "object" || Array.isArray(preset) || !("baseVals" in preset)) {
    throw new PermanentError("not a butterchurn preset (no baseVals)");
  }
  return createHash("sha256").update(bytes).digest("hex");
}

async function pin(file) {
  for (let attempt = 1; ; attempt++) {
    try {
      return await pinOnce(file);
    } catch (error) {
      if (error instanceof PermanentError || attempt >= ATTEMPTS) throw error;
      const delay = 500 * 2 ** (attempt - 1);
      console.warn(`  ${file}: ${describe(error)} — retry ${attempt}/${ATTEMPTS - 1} in ${delay} ms`);
      await sleep(delay);
    }
  }
}

async function main() {
  const files = listFiles();
  console.log(`Pinning ${files.length} Weekly presets from ${ORIGIN}`);

  const pins = new Map();
  const missing = [];
  const failures = [];
  let next = 0;
  let done = 0;
  async function worker() {
    while (next < files.length) {
      const file = files[next++];
      try {
        pins.set(file, await pin(file));
      } catch (error) {
        const gone = error instanceof PermanentError && (error.status === 403 || error.status === 404);
        if (allowMissing && gone) missing.push(`${file}: ${describe(error)}`);
        else failures.push(`${file}: ${describe(error)}`);
      }
      done += 1;
      if (done % 50 === 0 || done === files.length) console.log(`  ${done}/${files.length}`);
    }
  }
  await Promise.all(Array.from({ length: CONCURRENCY }, worker));

  if (failures.length > 0) {
    console.error(`\n${failures.length} preset(s) failed; ${path.relative(ROOT, OUT)} left untouched:`);
    for (const failure of failures.sort()) console.error(`  ${failure}`);
    process.exitCode = 1;
    return;
  }

  const manifest = Object.fromEntries([...pins.keys()].sort().map((file) => [file, pins.get(file)]));
  fs.writeFileSync(OUT, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`\nWrote ${pins.size} pins to ${path.relative(ROOT, OUT)}`);
  if (missing.length > 0) {
    console.warn(`Left out ${missing.length} missing preset(s) (--allow-missing):`);
    for (const entry of missing.sort()) console.warn(`  ${entry}`);
  }
}

main().catch((error) => {
  console.error(describe(error));
  process.exit(1);
});
