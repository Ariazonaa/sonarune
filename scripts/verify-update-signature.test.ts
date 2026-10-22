// @vitest-environment node
import { spawnSync } from "node:child_process";
import { createHash, generateKeyPairSync, randomBytes, sign } from "node:crypto";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { verifyUpdateSignature } from "./verify-update-signature.mjs";

// Written by `tauri signer sign --app-version 9.9.9` (CLI 2.11.5) with a
// throwaway key made for this test and deleted afterwards: the format the
// release workflow gets from the real one.
const TAURI = {
  file: Buffer.from("Sonarune signature test\n"),
  fileName: "Sonarune_9.9.9_x64-setup.exe",
  publicKey:
    "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDM5MTM3NzQ4QUI1QTlCNDUKUldSRm0xcXJTSGNUT1l6Y0sva1RpcmY5MnZoTGFtZUo2Zk9xdWYxcjV4Vmo2OEMvcmp3L3dDS1YK",
  signature:
    "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSRm0xcXJTSGNUT2N4ZGkvR0J4NFBCNDV0eVBLN0JpYnVCR3Vob2F6eEdRMEQ3YmF6MVZWc1o1eHZGblNHTkZWRGxjTHlrL2gyRnZGMXN2bW90MFNkb0dKMEUva2EvTWdFPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyNDg1NTI1CWZpbGU6U29uYXJ1bmVfOS45LjlfeDY0LXNldHVwLmV4ZQl2ZXJzaW9uOjkuOS45Cm55b3BBZk82VE1RemVwTjNiczJSM2J1NC9BQ01DM0dqbEZia1BKNktjK3F6c2IxVDZPSHp6Rno5SU5GN0hGRGEvVTVwRElLdjlZM3dIb3hTOERycEJnPT0K",
  version: "9.9.9",
};

const b64 = (text: string) => Buffer.from(text).toString("base64");

/** A minisign key pair and signature in Tauri's wrapping, made here. */
function signed(file: Buffer, comment: string, algorithm: "ED" | "Ed" = "ED") {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const pk = Buffer.from(publicKey.export({ format: "jwk" }).x as string, "base64url");
  const id = randomBytes(8);
  const message = algorithm === "ED" ? createHash("blake2b512").update(file).digest() : file;
  const sig = sign(null, message, privateKey);
  const commentSig = sign(null, Buffer.concat([sig, Buffer.from(comment)]), privateKey);
  const keyLine = Buffer.concat([Buffer.from("Ed"), id, pk]).toString("base64");
  const sigLine = Buffer.concat([Buffer.from(algorithm), id, sig]).toString("base64");
  return {
    publicKey: b64(`untrusted comment: minisign public key\n${keyLine}\n`),
    signature: b64(
      `untrusted comment: test\n${sigLine}\ntrusted comment: ${comment}\n${commentSig.toString("base64")}\n`,
    ),
  };
}

/** The decoded lines of a wrapped signature, edited, and wrapped again. */
function edit(signature: string, change: (lines: string[]) => void) {
  const lines = Buffer.from(signature, "base64").toString("utf8").split("\n");
  change(lines);
  return b64(lines.join("\n"));
}

describe("verifyUpdateSignature", () => {
  it("accepts what the Tauri signer writes, and reports key and comment", () => {
    expect(verifyUpdateSignature(TAURI)).toEqual({
      keyId: "39137748AB5A9B45",
      trustedComment: "timestamp:1792485525\tfile:Sonarune_9.9.9_x64-setup.exe\tversion:9.9.9",
    });
  });

  it("rejects a file changed after signing", () => {
    const file = Buffer.from(TAURI.file);
    file[0] ^= 1;
    expect(() => verifyUpdateSignature({ ...TAURI, file })).toThrow(/does not match/);
  });

  it("rejects a signature from another key", () => {
    const other = signed(TAURI.file, "timestamp:1\tfile:Sonarune_9.9.9_x64-setup.exe\tversion:9.9.9");
    expect(() => verifyUpdateSignature({ ...TAURI, publicKey: other.publicKey })).toThrow(
      /signed with key 39137748AB5A9B45, but the app trusts key/,
    );
  });

  it("rejects a trusted comment edited after signing", () => {
    const signature = edit(TAURI.signature, (lines) => {
      lines[2] = lines[2].replace("version:9.9.9", "version:9.9.10");
    });
    expect(() => verifyUpdateSignature({ ...TAURI, signature, version: "9.9.10" })).toThrow(
      /trusted comment does not match its signature/,
    );
  });

  it("insists on the file name and the version the workflow signed", () => {
    expect(() => verifyUpdateSignature({ ...TAURI, version: "9.9.8" })).toThrow(
      /carries version "9.9.9", expected 9.9.8/,
    );
    expect(() =>
      verifyUpdateSignature({ ...TAURI, fileName: "Sonarune_9.9.9_x64-portable.zip" }),
    ).toThrow(/names file "Sonarune_9.9.9_x64-setup.exe", expected Sonarune_9.9.9_x64-portable.zip/);
    const unversioned = signed(TAURI.file, "timestamp:1\tfile:Sonarune_9.9.9_x64-setup.exe");
    expect(() => verifyUpdateSignature({ ...TAURI, ...unversioned })).toThrow(
      /carries no version, expected 9.9.9/,
    );
    expect(verifyUpdateSignature({ ...TAURI, ...unversioned, version: undefined }).keyId).toMatch(
      /^[0-9A-F]{16}$/,
    );
  });

  it("checks the legacy algorithm too, which signs the file itself", () => {
    const comment = "timestamp:1\tfile:Sonarune_9.9.9_x64-setup.exe\tversion:9.9.9";
    const legacy = signed(TAURI.file, comment, "Ed");
    expect(verifyUpdateSignature({ ...TAURI, ...legacy }).trustedComment).toBe(comment);
    const other = Buffer.from("something else\n");
    expect(() => verifyUpdateSignature({ ...TAURI, ...legacy, file: other })).toThrow(/does not match/);
  });

  // What the release workflow runs: the script in a tree of its own, with a
  // tauri.conf.json that carries the test key.
  it("runs as the workflow calls it, and never prints the signature", () => {
    const dir = mkdtempSync(join(tmpdir(), "verify-update-signature-"));
    try {
      mkdirSync(join(dir, "scripts"));
      mkdirSync(join(dir, "src-tauri"));
      const script = join(dir, "scripts", "verify-update-signature.mjs");
      copyFileSync(fileURLToPath(new URL("./verify-update-signature.mjs", import.meta.url)), script);
      writeFileSync(
        join(dir, "src-tauri", "tauri.conf.json"),
        JSON.stringify({ plugins: { updater: { pubkey: TAURI.publicKey } } }),
      );
      const installer = join(dir, "dist", TAURI.fileName);
      mkdirSync(join(dir, "dist"));
      writeFileSync(installer, TAURI.file);
      writeFileSync(`${installer}.sig`, TAURI.signature);
      const cli = (...args: string[]) =>
        spawnSync(process.execPath, [script, ...args], { cwd: dir, encoding: "utf8" });

      const good = cli(join("dist", TAURI.fileName), "--version", "9.9.9");
      expect(good.status, good.stderr).toBe(0);
      expect(good.stdout).toContain(`${TAURI.fileName}: good signature from key 39137748AB5A9B45`);
      expect(good.stdout).toContain("trusted comment: timestamp:1792485525\tfile:");
      const inner = Buffer.from(TAURI.signature, "base64").toString("utf8").split("\n")[1];
      for (const secretish of [TAURI.signature, inner]) {
        expect(good.stdout + good.stderr).not.toContain(secretish);
      }

      const wrong = cli("--version", "9.9.8", installer);
      expect(wrong.status).toBe(1);
      expect(wrong.stderr).toContain("expected 9.9.8");
      expect(cli().status).toBe(2);
      expect(cli(installer, "--version").status).toBe(2);
    } finally {
      rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
    }
  });

  it("refuses what is not a wrapped minisign file", () => {
    expect(() => verifyUpdateSignature({ ...TAURI, signature: "bm90aGluZw==" })).toThrow(
      /signature is not a base64-wrapped minisign file/,
    );
    expect(() => verifyUpdateSignature({ ...TAURI, publicKey: b64("untrusted comment: x\nAAAA\n") })).toThrow(
      /public key has 3 bytes, not 42/,
    );
  });
});
