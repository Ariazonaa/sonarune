// Local quality gate: the same checks CI runs, in one command (`npm run verify`).
// Fails fast on the first gate that fails. Rust gates need cmake + cargo on
// PATH (the `opus` crate builds bundled libopus); the frontend gates need
// `npm install` to have run.
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const rust = join(root, "src-tauri");

/** @type {{ name: string, cmd: string, cwd: string }[]} */
const gates = [
  { name: "svelte-check", cmd: "npm run check", cwd: root },
  { name: "frontend tests", cmd: "npm run test", cwd: root },
  { name: "rustfmt", cmd: "cargo fmt --check", cwd: rust },
  { name: "clippy", cmd: "cargo clippy --all-targets -- -D warnings", cwd: rust },
  { name: "cargo test", cmd: "cargo test --lib", cwd: rust },
  { name: "frontend build", cmd: "npm run build", cwd: root },
];

// Only present in the private origin: it checks that nothing machine- or
// account-specific has crept into a file that gets published. Cheap, and it
// catches such a detail on the commit that introduces it rather than on the day
// someone runs the export. A public clone does not have the script and does not
// need the check.
const exportGuard = join(root, "scripts", "publish-public.mjs");
if (existsSync(exportGuard)) {
  gates.push({
    name: "publishable",
    cmd: "node scripts/publish-public.mjs --check",
    cwd: root,
  });
}

for (const gate of gates) {
  console.log(`\n\x1b[1m▶ ${gate.name}\x1b[0m  (${gate.cmd})`);
  const result = spawnSync(gate.cmd, { cwd: gate.cwd, stdio: "inherit", shell: true });
  if (result.status !== 0) {
    console.error(`\n\x1b[31m✗ quality gate failed: ${gate.name}\x1b[0m`);
    process.exit(result.status ?? 1);
  }
}

console.log("\n\x1b[32m✓ all quality gates passed\x1b[0m");
