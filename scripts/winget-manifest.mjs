// Write the winget manifests for a release that is already published.
//
//   node scripts/winget-manifest.mjs [version] [--out DIR] [--trust-sums]
//
// winget installs from a manifest that lives in microsoft/winget-pkgs, not
// from anything this repository serves: three YAML files per version, carrying
// the download URL and its SHA-256. This writes them, so the part that can go
// wrong -- a checksum copied by hand, a URL that names the wrong tag -- is not
// done by hand.
//
// Everything that changes per release is read from the release itself: the
// GitHub API is asked for the tag, which gives the installer's real download
// URL and the date it was published, and the checksum comes out of the
// `SHA256SUMS.txt` that rides along with it. By default the installer is then
// downloaded and hashed as well, because the sums file only proves what the
// build thought it wrote, not what the release actually serves today; the
// download is ~7 MB and `--trust-sums` skips it.
//
// Everything that has to agree with the app is read from the app's own
// manifests rather than repeated here: the ARP entries (what the installer
// writes into Windows' uninstall list, which is how winget recognises an
// installed copy and its version) come from `tauri.conf.json`, the licence
// from `Cargo.toml`. If the product name or publisher ever changes, the
// generated manifest follows instead of quietly lying.
//
// The output is the `manifests/` subtree winget-pkgs expects, so submitting is
// copying `winget/manifests` over a fork's `manifests` and opening a pull
// request:
//
//   1. fork github.com/microsoft/winget-pkgs (once)
//   2. node scripts/winget-manifest.mjs
//   3. copy winget/manifests/* into the fork's manifests/
//   4. winget validate --manifest <that version directory>   (optional, local)
//   5. commit on a branch, push, open the PR against master
//
// The pull request is validated automatically: the manifests are checked
// against the schema, then the package is installed and uninstalled in a
// sandbox. A release that is still a draft has no public download URL, so this
// script refuses to write a manifest for one.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { sectionText } from "./changelog-section.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/** The manifest schema version. Bump together with the three `$schema` lines. */
const SCHEMA = "1.28.0";
/** `Publisher.Package`, and the directory names under `manifests/`. */
const PUBLISHER = "Ariazonaa";
const PACKAGE = "Jellysic";
const IDENTIFIER = `${PUBLISHER}.${PACKAGE}`;
const REPO = "Ariazonaa/jellysic";
/** What the release notes say the app runs on: Windows 10, any build, 64-bit. */
const MINIMUM_OS = "10.0.0.0";

const read = (file) => readFileSync(join(root, file), "utf8");
const json = (file) => JSON.parse(read(file));

// --- YAML ------------------------------------------------------------------

/**
 * A scalar, quoted only where YAML would otherwise read it as something else.
 * Single quotes rather than double: nothing in a manifest needs an escape, and
 * `''` for a literal quote is the whole escaping story.
 */
function scalar(value) {
  const s = String(value);
  const risky =
    s === "" || /^[\s>|&*!%@`'"[\]{},#?-]/.test(s) || /:\s|\s#|[\r\n]/.test(s) || /\s$/.test(s);
  return risky ? `'${s.replaceAll("'", "''")}'` : s;
}

/** A literal block, for the only field that is a whole text: ReleaseNotes. */
function block(text, indent) {
  const pad = " ".repeat(indent);
  const body = text
    .split("\n")
    .map((line) => (line ? pad + line : ""))
    .join("\n");
  return `|-\n${body}`;
}

/**
 * A folded block: wrapped in the file, one paragraph once it is read. For the
 * description, which is a paragraph and should not arrive as one endless line
 * in a reviewer's diff.
 */
function folded(text, indent, width = 96) {
  const pad = " ".repeat(indent);
  const lines = [];
  let line = "";
  for (const word of text.split(/\s+/)) {
    if (line && (pad + line + " " + word).length > width) {
      lines.push(pad + line);
      line = word;
    } else {
      line = line ? `${line} ${word}` : word;
    }
  }
  if (line) lines.push(pad + line);
  return `>-\n${lines.join("\n")}`;
}

/** Render `{key: value}` pairs, skipping the ones with nothing to say. */
function fields(pairs, indent = 0) {
  const pad = " ".repeat(indent);
  return Object.entries(pairs)
    .filter(([, value]) => value !== undefined && value !== null && value !== "")
    .map(([key, value]) => `${pad}${key}: ${value}`)
    .join("\n");
}

function header(kind) {
  return `# yaml-language-server: $schema=https://aka.ms/winget-manifest.${kind}.${SCHEMA}.schema.json`;
}

// --- the release ------------------------------------------------------------

async function fetchJson(url) {
  const res = await fetch(url, {
    headers: { accept: "application/vnd.github+json", "user-agent": "jellysic-winget-manifest" },
  });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`GitHub API answered ${res.status} for ${url}`);
  return res.json();
}

/**
 * The published release for a version: its date, the installer's download URL
 * and the checksum the build recorded for it.
 *
 * Unauthenticated, which is deliberate — a draft release is invisible without a
 * token, and a draft is exactly what must not be turned into a manifest: its
 * download URL is not public, so every install from it would fail.
 */
async function release(version) {
  const tag = `v${version}`;
  const found = await fetchJson(`https://api.github.com/repos/${REPO}/releases/tags/${tag}`);
  if (!found) {
    throw new Error(
      `no published release ${tag} at github.com/${REPO}.\n` +
        `A draft counts as missing here: publish it first, then run this again.`,
    );
  }

  const installerName = `jellysic_${version}_x64-setup.exe`;
  const asset = (name) => found.assets.find((a) => a.name === name);
  const installer = asset(installerName);
  const sums = asset("SHA256SUMS.txt");
  if (!installer) throw new Error(`${tag} carries no ${installerName}`);
  if (!sums) throw new Error(`${tag} carries no SHA256SUMS.txt`);

  const sumsText = await (await fetch(sums.browser_download_url)).text();
  const line = sumsText.split(/\r?\n/).find((l) => l.trim().endsWith(installerName));
  if (!line) throw new Error(`SHA256SUMS.txt of ${tag} does not mention ${installerName}`);
  const sha256 = line.trim().split(/\s+/)[0];
  if (!/^[a-f0-9]{64}$/i.test(sha256)) {
    throw new Error(`SHA256SUMS.txt of ${tag} has no usable checksum for ${installerName}`);
  }

  return {
    tag,
    date: found.published_at.slice(0, 10),
    url: installer.browser_download_url,
    size: installer.size,
    sha256: sha256.toUpperCase(),
  };
}

/** Download the installer and hash it, so the manifest is checked and not trusted. */
async function verifyDownload(info) {
  const res = await fetch(info.url, { redirect: "follow" });
  if (!res.ok) throw new Error(`the installer URL answered ${res.status}: ${info.url}`);
  const bytes = Buffer.from(await res.arrayBuffer());
  const actual = createHash("sha256").update(bytes).digest("hex").toUpperCase();
  if (actual !== info.sha256) {
    throw new Error(
      `the installer served at ${info.url} hashes to ${actual},\n` +
        `but SHA256SUMS.txt of the release says ${info.sha256}. Do not submit this.`,
    );
  }
  return bytes.length;
}

// --- the manifests ----------------------------------------------------------

function manifests(version, info) {
  const conf = json("src-tauri/tauri.conf.json");
  const cargo = read("src-tauri/Cargo.toml");
  const license = /^license = "(.+)"$/m.exec(cargo)?.[1];
  if (!license) throw new Error("Cargo.toml has no license field");

  // What the NSIS installer writes into the uninstall list. Both default to the
  // product name in Tauri's template, and the install directory and the
  // registry key are the product name too — that key's name is the ProductCode
  // winget correlates on for a non-MSI installer.
  const productName = conf.productName;
  const publisher = conf.bundle?.publisher ?? productName;

  const notes = sectionText(read("CHANGELOG.md"), version, 10000);

  const version_yaml = [
    header("version"),
    "",
    fields({
      PackageIdentifier: scalar(IDENTIFIER),
      PackageVersion: scalar(version),
      DefaultLocale: "en-US",
      ManifestType: "version",
      ManifestVersion: SCHEMA,
    }),
    "",
  ].join("\n");

  const installer_yaml = [
    header("installer"),
    "",
    fields({
      PackageIdentifier: scalar(IDENTIFIER),
      PackageVersion: scalar(version),
      MinimumOSVersion: MINIMUM_OS,
      // Tauri builds an NSIS installer; winget's name for that is "nullsoft",
      // and it knows the /S switch, so no InstallerSwitches are needed. The
      // installer is a currentUser one, hence user scope and no elevation.
      InstallerType: "nullsoft",
      Scope: "user",
      UpgradeBehavior: "install",
      ReleaseDate: info.date,
    }),
    "InstallModes:",
    "  - interactive",
    "  - silent",
    "  - silentWithProgress",
    // Without these, winget compares the manifest's name and publisher against
    // the uninstall entry, finds "jellysic" where it expected "Jellysic" by
    // "Ariazonaa", and cannot tell that the package is installed or which
    // version it is.
    "AppsAndFeaturesEntries:",
    fields(
      {
        DisplayName: scalar(productName),
        Publisher: scalar(publisher),
        DisplayVersion: scalar(version),
        ProductCode: scalar(productName),
        InstallerType: "nullsoft",
      },
      4,
    ).replace(/^ {4}/, "  - "),
    "Installers:",
    fields(
      {
        Architecture: "x64",
        InstallerUrl: scalar(info.url),
        InstallerSha256: info.sha256,
      },
      4,
    ).replace(/^ {4}/, "  - "),
    fields({ ManifestType: "installer", ManifestVersion: SCHEMA }),
    "",
  ].join("\n");

  const locale_yaml = [
    header("defaultLocale"),
    "",
    fields({
      PackageIdentifier: scalar(IDENTIFIER),
      PackageVersion: scalar(version),
      PackageLocale: "en-US",
      Publisher: scalar(PUBLISHER),
      PublisherUrl: scalar(`https://github.com/${PUBLISHER}`),
      PublisherSupportUrl: scalar(`https://github.com/${REPO}/issues`),
      Author: scalar(PUBLISHER),
      PackageName: scalar(PACKAGE),
      PackageUrl: scalar(`https://github.com/${REPO}`),
      License: scalar(license),
      LicenseUrl: scalar(`https://github.com/${REPO}/blob/main/LICENSE`),
      Copyright: scalar(`Copyright (C) 2026 ${PUBLISHER}`),
      CopyrightUrl: scalar(`https://github.com/${REPO}/blob/main/LICENSE`),
      ShortDescription: scalar(conf.bundle?.shortDescription ?? ""),
      Description: folded(DESCRIPTION, 2),
      Moniker: "jellysic",
    }),
    "Tags:",
    ...TAGS.map((tag) => `  - ${tag}`),
    fields({
      ReleaseNotes: notes ? block(notes, 2) : undefined,
      ReleaseNotesUrl: scalar(`https://github.com/${REPO}/releases/tag/${info.tag}`),
      ManifestType: "defaultLocale",
      ManifestVersion: SCHEMA,
    }),
    "",
  ].join("\n");

  return {
    [`${IDENTIFIER}.yaml`]: version_yaml,
    [`${IDENTIFIER}.installer.yaml`]: installer_yaml,
    [`${IDENTIFIER}.locale.en-US.yaml`]: locale_yaml,
  };
}

/** The store listing. One paragraph, the same claim the README opens with. */
const DESCRIPTION = [
  "Jellysic is a desktop music client for a Jellyfin server. The interface is",
  "Tauri 2 and Svelte 5; the audio path -- decoding, DSP, gapless playback,",
  "crossfade, output device -- lives in Rust, so there is no Electron and no",
  "external mpv binary. It plays your library with a queue, playlists, smart",
  "views, lyrics, an equaliser, a visualizer, a mini player and media-key and",
  "taskbar integration, and it keeps itself up to date.",
].join(" ");

/** At most 16, and they are what someone would type into `winget search`. */
const TAGS = [
  "audio",
  "flac",
  "jellyfin",
  "media",
  "music",
  "music-player",
  "player",
  "rust",
  "streaming",
  "tauri",
];

// --- main -------------------------------------------------------------------

const argv = process.argv.slice(2);
/** The flags that take a value, so the value is not read as the version. */
const TAKES_VALUE = new Set(["--out"]);
const positional = [];
for (let i = 0; i < argv.length; i += 1) {
  if (argv[i].startsWith("--")) {
    if (TAKES_VALUE.has(argv[i])) i += 1;
  } else {
    positional.push(argv[i]);
  }
}
const flag = (name) => argv.includes(name);
const option = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : fallback;
};

const version = positional[0] ?? json("package.json").version;
const out = option("--out", join(root, "winget"));

const info = await release(version);
if (flag("--trust-sums")) {
  console.log(`checksum taken from SHA256SUMS.txt, installer not downloaded`);
} else {
  const bytes = await verifyDownload(info);
  console.log(`downloaded ${(bytes / 1024 / 1024).toFixed(1)} MB, hash matches SHA256SUMS.txt`);
}

// manifests/<first letter of the publisher, lowercased>/<Publisher>/<Package>/<version>
const dir = join(
  out,
  "manifests",
  PUBLISHER[0].toLowerCase(),
  PUBLISHER,
  PACKAGE,
  version,
);
mkdirSync(dir, { recursive: true });
for (const [name, text] of Object.entries(manifests(version, info))) {
  // LF and no BOM: the validator reads these as UTF-8 and a BOM is a parse error.
  writeFileSync(join(dir, name), text.replace(/\r\n/g, "\n"), "utf8");
  console.log(`wrote ${join(dir, name)}`);
}

console.log(`\n${IDENTIFIER} ${version} (${info.tag}, published ${info.date})`);
console.log(`  ${info.url}`);
console.log(`  ${info.sha256}`);

// `winget validate` is the same schema check the pull request runs first, and
// it is on every Windows 11 machine. Not having it is not a failure here; a
// manifest it rejects is.
try {
  const output = execFileSync("winget", ["validate", "--manifest", dir], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  console.log(`\n${output.trim()}`);
} catch (error) {
  if (error.code === "ENOENT") {
    console.log("\nwinget is not on PATH; skipped `winget validate`");
  } else {
    console.error(`\n${error.stdout ?? ""}${error.stderr ?? ""}`.trim());
    process.exitCode = 1;
  }
}
