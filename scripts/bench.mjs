// Measures a desktop music client: how long until its window is up, how much
// memory it holds, and how much CPU it burns while playing.
//
//   node scripts/bench.mjs --exe <path> --label <name> [--seconds 45] [--settle 15] [--json]
//
// Both clients are pointed at the same local dev server (scripts/devserver),
// so they browse the same library over the same network path. The numbers are
// only comparable if the runs are: same machine, same window size, same
// library, nothing else busy.
//
// What is measured, and what that means:
//   startup   process start -> the main window has a handle. This is "the
//             window is up", not "the library is drawn", because only one of
//             the two clients can be asked the latter.
//   memory    private working set of the whole process tree. A browser engine
//             spreads itself over a handful of processes; counting only the
//             parent would flatter it enormously, so every descendant counts.
//   cpu       total processor time across the tree over the sample window,
//             as a percentage of one core. Sampling starts only after the
//             settle period, because a client is still parsing, laying out and
//             fetching for several seconds after its window appears -- counting
//             that as "idle CPU" measures startup and calls it rest.
//
// Windows only (Get-CimInstance, Get-Process).
import { spawn, spawnSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};

const exe = opt("exe");
const label = opt("label", "app");
const seconds = Number(opt("seconds", 45));
const settle = Number(opt("settle", 15));
if (!exe) {
  console.error(
    "usage: node scripts/bench.mjs --exe <path> --label <name> [--seconds 45] [--settle 15] [--json]",
  );
  process.exit(2);
}

function ps(script) {
  const r = spawnSync("powershell", ["-NoProfile", "-Command", script], { encoding: "utf8" });
  return r.stdout.trim();
}

/** The process and every descendant of it, by pid. */
function tree(rootPid) {
  const out = ps(`
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
    $want = New-Object System.Collections.Generic.HashSet[int]
    [void]$want.Add(${rootPid})
    for ($i = 0; $i -lt 6; $i++) {
      foreach ($p in $all) { if ($want.Contains([int]$p.ParentProcessId)) { [void]$want.Add([int]$p.ProcessId) } }
    }
    ($want -join ",")
  `);
  return out ? out.split(",").map(Number).filter(Boolean) : [rootPid];
}

/** Private working set (MB) and total CPU seconds across the tree. */
function sample(pids) {
  const list = pids.join(",");
  const out = ps(`
    $p = Get-Process -Id ${list} -ErrorAction SilentlyContinue
    if (-not $p) { "0 0 0" } else {
      $ws = ($p | Measure-Object WorkingSet64 -Sum).Sum
      $pm = ($p | Measure-Object PrivateMemorySize64 -Sum).Sum
      $cpu = ($p | Measure-Object CPU -Sum).Sum
      "$ws $pm $cpu"
    }
  `);
  const [ws, pm, cpu] = out.split(/\s+/).map(Number);
  return { workingSetMb: ws / 1024 / 1024, privateMb: pm / 1024 / 1024, cpuSeconds: cpu || 0 };
}

const started = Date.now();
const child = spawn(exe, [], { detached: true, stdio: "ignore" });
child.unref();

// Startup: poll until the process owns a window.
let windowMs = null;
for (let i = 0; i < 600; i++) {
  const has = ps(
    `$p = Get-Process -Id ${child.pid} -ErrorAction SilentlyContinue; ` +
      `if ($p -and $p.MainWindowHandle -ne 0) { "1" } else { "0" }`,
  );
  if (has === "1") {
    windowMs = Date.now() - started;
    break;
  }
  await sleep(100);
}

// Let the client finish starting before anything is recorded.
await sleep(settle * 1000);

const pids = tree(child.pid);
const samples = [];
for (let s = 0; s < seconds; s += 5) {
  await sleep(5000);
  samples.push({ t: s + 5, ...sample(tree(child.pid)) });
}

const peak = Math.max(...samples.map((s) => s.workingSetMb));
const settled = samples.slice(Math.floor(samples.length / 2));
const median = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
const cpuFirst = samples[0]?.cpuSeconds ?? 0;
const cpuLast = samples.at(-1)?.cpuSeconds ?? 0;
const wallSeconds = (samples.at(-1)?.t ?? seconds) - (samples[0]?.t ?? 0);

const result = {
  label,
  exe,
  settleSeconds: settle,
  processes: pids.length,
  startupMs: windowMs,
  workingSetPeakMb: Number(peak.toFixed(1)),
  workingSetMedianMb: Number(median(settled.map((s) => s.workingSetMb)).toFixed(1)),
  privateMedianMb: Number(median(settled.map((s) => s.privateMb)).toFixed(1)),
  cpuPercentOfOneCore:
    wallSeconds > 0 ? Number((((cpuLast - cpuFirst) / wallSeconds) * 100).toFixed(1)) : null,
  samples,
};

if (args.includes("--json")) {
  console.log(JSON.stringify(result, null, 1));
} else {
  const ESC = String.fromCharCode(27);
  const BOLD = `${ESC}[1m`;
  const DIM = `${ESC}[2m`;
  const OFF = `${ESC}[0m`;
  const row = (k, v) => console.log(`  ${k.padEnd(24)}${v}`);
  console.log("");
  console.log(`${BOLD}${label}${OFF}  ${DIM}${exe}${OFF}`);
  row("processes", result.processes);
  row("launch → window", result.startupMs === null ? "no window" : `${result.startupMs} ms`);
  row("working set, median", `${result.workingSetMedianMb} MB`);
  row("working set, peak", `${result.workingSetPeakMb} MB`);
  row("private bytes, median", `${result.privateMedianMb} MB`);
  row("cpu, % of one core", `${result.cpuPercentOfOneCore ?? "?"} %`);
  console.log(
    `${DIM}  ${samples.length} samples over ${seconds}s after a ${settle}s settle` +
      ` · --json for the raw data${OFF}`,
  );
  console.log("");
}
