// Check an update signature the way the installed app checks it: against the
// public key in src-tauri/tauri.conf.json.
//
//   node scripts/verify-update-signature.mjs <installer> [--version <v>]
//
// Reads `<installer>.sig` next to the installer. The release workflow runs
// this right after signing, in a step that has no key: a signing secret that
// no longer belongs to the public key the app ships with would produce
// releases no installed app accepts, and nobody would notice before the first
// update failed out there. It prints the key id and the trusted comment, never
// the signature -- the log of a public repository is public.
//
// The .sig and the pubkey are what Tauri writes: a minisign file, base64
// encoded once more. The signature is Ed25519 over the BLAKE2b-512 hash of the
// file ("ED"; the legacy "Ed" signs the file itself), and a second Ed25519
// signature covers the first plus the trusted comment, which is where the
// `version:` that `requireSignedVersion` checks lives. Node's own crypto does
// all of it; nothing from node_modules runs.
import { createHash, createPublicKey, verify } from "node:crypto";
import { readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

/** The lines of a base64-wrapped minisign file. */
function unwrap(text, what) {
  const lines = Buffer.from(text.trim(), "base64").toString("utf8").split(/\r?\n/);
  if (!lines[0].startsWith("untrusted comment: ")) {
    throw new Error(`${what} is not a base64-wrapped minisign file`);
  }
  return lines;
}

function decode(line, length, what) {
  const raw = Buffer.from(line ?? "", "base64");
  if (raw.length !== length) throw new Error(`${what} has ${raw.length} bytes, not ${length}`);
  return raw;
}

/** minisign shows a key id as the little-endian number it is, in hex. */
const keyId = (raw) => Buffer.from(raw).reverse().toString("hex").toUpperCase();

/**
 * Throws with the reason when the signature does not hold up.
 *
 * @param {{ file: Buffer, fileName: string, signature: string, publicKey: string, version?: string }} input
 * @returns {{ keyId: string, trustedComment: string }}
 */
export function verifyUpdateSignature({ file, fileName, signature, publicKey, version }) {
  const pk = decode(unwrap(publicKey, "the public key")[1], 42, "the public key");
  if (pk.subarray(0, 2).toString("latin1") !== "Ed") {
    throw new Error("the public key is not a minisign Ed25519 key");
  }
  const key = createPublicKey({
    key: { kty: "OKP", crv: "Ed25519", x: pk.subarray(10).toString("base64url") },
    format: "jwk",
  });

  const lines = unwrap(signature, "the signature");
  const sig = decode(lines[1], 74, "the signature");
  const algorithm = sig.subarray(0, 2).toString("latin1");
  if (algorithm !== "ED" && algorithm !== "Ed") {
    throw new Error(`the signature uses an unknown algorithm (${JSON.stringify(algorithm)})`);
  }
  if (!sig.subarray(2, 10).equals(pk.subarray(2, 10))) {
    throw new Error(
      `signed with key ${keyId(sig.subarray(2, 10))}, but the app trusts key ${keyId(pk.subarray(2, 10))}`,
    );
  }
  const prefix = "trusted comment: ";
  if (!lines[2]?.startsWith(prefix)) throw new Error("the signature has no trusted comment");
  const trustedComment = lines[2].slice(prefix.length);
  const commentSig = decode(lines[3], 64, "the trusted comment's signature");

  const signed = algorithm === "ED" ? createHash("blake2b512").update(file).digest() : file;
  if (!verify(null, signed, key, sig.subarray(10))) {
    throw new Error(`the signature does not match ${fileName}`);
  }
  const comment = Buffer.concat([sig.subarray(10), Buffer.from(trustedComment, "utf8")]);
  if (!verify(null, comment, key, commentSig)) {
    throw new Error("the trusted comment does not match its signature");
  }

  // What the signer is expected to have put there.
  const fields = new Map(
    trustedComment
      .split("\t")
      .filter((field) => field.includes(":"))
      .map((field) => [field.slice(0, field.indexOf(":")), field.slice(field.indexOf(":") + 1)]),
  );
  const said = (name, what) =>
    fields.has(name) ? `${what} ${JSON.stringify(fields.get(name))}` : `no ${name}`;
  if (fields.get("file") !== fileName) {
    throw new Error(`the trusted comment names ${said("file", "file")}, expected ${fileName}`);
  }
  if (version !== undefined && fields.get("version") !== version) {
    throw new Error(`the trusted comment carries ${said("version", "version")}, expected ${version}`);
  }
  return { keyId: keyId(pk.subarray(2, 10)), trustedComment };
}

// Run as a script (not when imported by a test).
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const args = process.argv.slice(2);
  const versionAt = args.indexOf("--version");
  const version = versionAt >= 0 ? args[versionAt + 1] : undefined;
  const installer = args.find(
    (arg, i) => !arg.startsWith("--") && (versionAt < 0 || i !== versionAt + 1),
  );
  if (!installer || (versionAt >= 0 && !version)) {
    console.error("usage: node scripts/verify-update-signature.mjs <installer> [--version <v>]");
    process.exit(2);
  }
  try {
    const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
    const result = verifyUpdateSignature({
      file: readFileSync(installer),
      fileName: basename(installer),
      signature: readFileSync(`${installer}.sig`, "utf8"),
      publicKey: conf.plugins.updater.pubkey,
      version,
    });
    console.log(`${basename(installer)}: good signature from key ${result.keyId}`);
    console.log(`trusted comment: ${result.trustedComment}`);
  } catch (error) {
    console.error(`✗ ${basename(installer)}: ${error instanceof Error ? error.message : error}`);
    process.exit(1);
  }
}
