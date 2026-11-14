import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { endTerminals, start, stop } from "./e2e-harness.mjs";
import {
  KEPT_MIN, REFUSED_EXIT, incomplete, joinSample, judge, noiseShares, quiet, readQuiet, sampleMetrics, schedule,
  stayedQuiet,
} from "./speed-budget.mjs";
import { PROJECT } from "./speed-seed.mjs";

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const RUN = join(appRoot, "test", "native", "run.mjs");
const TUI = resolve(appRoot, "../../scripts/qualification/tui_speed.py");
const QUIET_WAIT_MS = 10 * 60000;
const QUIET_LOOK_MS = 15000;
const TAINTED_MAX = 3;
const RUN_MS_MAX = 20 * 60000;
const OUTPUT_BYTES_MAX = 1 << 20;
const STATE = /^\/tmp\/cas-(noise|compare)-(base|candidate)-[0-9a-f]{8}$/;

/** One side's artifacts as build-speed-base.sh lays them out, and the digest of its manifest
 * that names the build. Input: the folder. Output: the side. */
function artifacts(folder) {
  assert.ok(folder && isAbsolute(folder), `an absolute artifact folder: ${folder}`);
  const manifest = readFileSync(join(folder, "manifest.sha256"));
  for (const name of ["crazyagent", "crazyagent-desktop", "desktop-e2e-bridge", "desktop-owner-fixture"]) {
    assert.ok(existsSync(join(folder, "bin", name)), `${folder} has bin/${name}`);
  }
  const digest = createHash("sha256").update(manifest).digest("hex");
  assert.match(digest, /^[0-9a-f]{64}$/);
  return { folder, digest };
}

/** Makes a side's kept engine state folder anew, removing only a folder this driver names.
 * Inputs: the mode, the side's label and its artifacts. Output: the state folder. */
function freshState(mode, label, side) {
  const state = `/tmp/cas-${mode}-${label}-${side.digest.slice(0, 8)}`;
  assert.match(state, STATE, "a speed state folder");
  rmSync(state, { recursive: true, force: true });
  mkdirSync(join(state, "home"), { recursive: true, mode: 0o700 });
  return state;
}

/** Waits, bounded, until the Mac is quiet. Output: null once quiet, else the last reasons. */
async function waitQuiet() {
  let reasons = [];
  for (const deadline = Date.now() + QUIET_WAIT_MS; Date.now() < deadline; await pause(QUIET_LOOK_MS)) {
    const reading = quiet(readQuiet());
    if (reading.quiet) return null;
    reasons = reading.reasons;
  }
  assert.ok(reasons.length > 0);
  return reasons;
}

/** Runs one child to its end, bounded by `RUN_MS_MAX`, keeping the tail of its output. Inputs:
 * the program, its arguments and its environment. Output: `{ code, out, err }`. */
async function finished(program, args, env) {
  const child = spawn(program, args, { cwd: appRoot, env, stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  let err = "";
  child.stdout.on("data", (chunk) => { out = (out + chunk).slice(-OUTPUT_BYTES_MAX); });
  child.stderr.on("data", (chunk) => { err = (err + chunk).slice(-OUTPUT_BYTES_MAX); });
  const timer = setTimeout(() => child.kill("SIGTERM"), RUN_MS_MAX);
  try {
    const code = await new Promise((done, fail) => { child.once("error", fail); child.once("close", done); });
    assert.ok(out.length <= OUTPUT_BYTES_MAX && err.length <= OUTPUT_BYTES_MAX);
    return { code, out, err };
  } finally {
    clearTimeout(timer);
  }
}

/** Runs one speed launch through run.mjs's speed suite and reads its records. Inputs: the side
 * and its state. Output: the run's evidence folder and its two records. */
async function runOnce(side, state) {
  const env = { ...process.env, CRAZYAGENT_DESKTOP_TEST_SUITE: "speed",
    CRAZYAGENT_DESKTOP_TEST_APP: join(side.folder, "bin", "crazyagent-desktop"),
    CRAZYAGENT_DESKTOP_TEST_FIXTURE: join(side.folder, "bin", "desktop-owner-fixture"),
    CRAZYAGENT_E2E_HOME: join(state, "home"), SPEED_SIDE: side.folder, SPEED_STATE: state };
  const { code, out, err } = await finished(process.execPath, [RUN], env);
  const report = out.trim().split("\n").at(-1) ?? "";
  assert.ok(code === 0 && report.endsWith("completion.json"), `run.mjs speed failed (${code}): ${err.slice(-3000)}`);
  const completion = JSON.parse(readFileSync(report, "utf8"));
  assert.equal(completion.failure, null, `run.mjs speed failed: ${completion.failure}`);
  const evidence = dirname(report);
  const read = (name) => JSON.parse(readFileSync(join(evidence, name), "utf8"));
  return { evidence, engine: read("speed-engine.json"), app: read("speed-app.json") };
}

/** Runs row U1 on a side's kept state right after its app run, in the same slot: starts the
 * side's engine on that state as the owner's launchd engine runs, times the TUI through
 * tui_speed.py, then stops the engine and its agent terminals. Inputs: the side, its state and
 * the app run's evidence folder. Output: the TUI's `{ tui_<step>_ms: ms }`. */
async function tuiOnce(side, state, evidence) {
  const runtime = join(state, "engine");
  const home = join(state, "home");
  assert.ok(existsSync(join(state, "seeded.json")), "the app run seeded this side first");
  const tmp = join(evidence, "tui-tmp");
  mkdirSync(tmp, { mode: 0o700 });
  const env = { PATH: `${dirname(process.execPath)}:/usr/bin:/bin:/usr/sbin:/sbin`, HOME: home,
    CFFIXED_USER_HOME: home, TMPDIR: tmp, LANG: "en_US.UTF-8" };
  const engine = join(side.folder, "bin", "crazyagent");
  const argv = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"),
    "engine.sock", "--writable-settings"];
  const { child } = await start(engine, argv, { ...env, NO_COLOR: "1" }, evidence, "tui-engine", /^ready$/m);
  try {
    const python = { ...env, PATH: process.env.PATH, PYTHONDONTWRITEBYTECODE: "1" };
    const { code, out, err } = await finished("python3", [TUI, engine, join(runtime, "engine.sock"), PROJECT], python);
    writeFileSync(join(evidence, "tui-speed.err"), err, { flag: "wx", mode: 0o600 });
    assert.equal(code, 0, `tui_speed.py failed (${code}): ${err.slice(-3000)}`);
    const times = JSON.parse(out.trim().split("\n").at(-1));
    assert.ok(Object.keys(times).length > 0, "tui_speed.py timed its steps");
    return times;
  } finally {
    await stop(child);
    await endTerminals(runtime);
  }
}

/** Writes a JSON record into the output folder. Inputs: the folder, the name and the value.
 * Output: none. */
function record(out, name, value) {
  writeFileSync(join(out, name), JSON.stringify(value, null, 1), { flag: "wx", mode: 0o600 });
}

/** Stops the run as not judged: the Mac was too busy. Inputs: the output folder, the reasons
 * and the runs so far. Output: never returns. */
function refuse(out, reasons, runs) {
  assert.ok(reasons.length > 0, "a refusal names its reasons");
  record(out, "refused.json", { at: new Date().toISOString(), reasons, runs });
  console.error(`REFUSED: ${reasons.join("; ")}`);
  process.exit(REFUSED_EXIT);
}

/** Runs every scheduled launch, each only on a quiet Mac and kept only when the Mac stayed
 * quiet through it. Inputs: the mode, both sides, kept runs and the output folder. Output: the
 * kept samples per side. */
async function collect(mode, sides, kept, out) {
  const states = {
    base: freshState(mode, "base", sides.base),
    candidate: freshState(mode, "candidate", sides.candidate),
  };
  assert.notEqual(states.base, states.candidate, "each side keeps its own state");
  const samples = { base: [], candidate: [] };
  const runs = [];
  try {
    for (const { side, warm } of schedule(kept)) {
      for (let tainted = 0; ; tainted += 1) {
        if (tainted >= TAINTED_MAX) refuse(out, [`${TAINTED_MAX} runs in a row saw the Mac get busy`], runs);
        const busy = await waitQuiet();
        if (busy) refuse(out, busy, runs);
        const run = await runOnce(sides[side], states[side]);
        const tui = await tuiOnce(sides[side], states[side], run.evidence);
        const after = stayedQuiet(readQuiet());
        runs.push({ side, warm, evidence: run.evidence, quiet_after: after.quiet, reasons: after.reasons });
        if (!after.quiet) continue;
        if (!warm) samples[side].push(joinSample(sampleMetrics(run.engine, run.app), tui));
        break;
      }
    }
  } finally {
    record(out, "runs.json", runs);
    record(out, "samples.json", samples);
  }
  assert.equal(samples.base.length, kept);
  assert.equal(samples.candidate.length, kept);
  return samples;
}

/** The speed driver (DEC-169). `noise` runs the base against itself and writes noise.json;
 * `compare` runs the base against the candidate, judges every metric with that noise and writes
 * verdict.json. Exits 0 pass, 1 fail or error, 75 refused (not judged). */
async function main() {
  const usage = "usage: speed-run.mjs noise|compare --base <dir> [--candidate <dir> --noise <noise.json>] --out <dir>";
  const mode = process.argv[2];
  assert.ok(mode === "noise" || mode === "compare", usage);
  const { values } = parseArgs({ args: process.argv.slice(3), allowPositionals: false, options: {
    base: { type: "string" }, candidate: { type: "string" }, noise: { type: "string" },
    out: { type: "string" }, kept: { type: "string", default: "10" } } });
  assert.ok(values.out && isAbsolute(values.out), "--out names an absolute folder");
  const kept = Number(values.kept);
  assert.ok(Number.isInteger(kept) && kept >= KEPT_MIN, `--kept is a whole number of at least ${KEPT_MIN}`);
  const base = artifacts(values.base);
  const candidate = mode === "noise" ? base : artifacts(values.candidate);
  const shares = mode === "compare" ? readNoise(values.noise, base, candidate) : null;
  mkdirSync(values.out, { recursive: true, mode: 0o700 });
  assert.equal(readdirSync(values.out).length, 0, "--out is empty");
  const samples = await collect(mode, { base, candidate }, kept, values.out);
  if (mode === "noise") {
    const noise = { base: base.digest, shares: noiseShares(samples.base, samples.candidate) };
    record(values.out, "noise.json", noise);
    return 0;
  }
  const rows = judge(samples.base, samples.candidate, shares);
  const gaps = incomplete(rows);
  record(values.out, "verdict.json", { base: base.digest, candidate: candidate.digest, rows, incomplete: gaps });
  for (const row of rows) {
    const medians = row.base ? `${row.base.median} -> ${row.candidate.median}` : `kept ${row.kept}`;
    console.log(`${row.status.padEnd(10)} ${row.metric} ${medians} ${(row.reasons ?? []).join("; ")}`);
  }
  if (gaps.length > 0) console.error(`NOT JUDGED: ${gaps.join(", ")}`);
  return rows.some((row) => row.status === "fail") || gaps.length > 0 ? 1 : 0;
}

/** Reads the noise a `noise` run wrote, refusing one read on another base or a candidate that
 * is the base itself. Inputs: its path and both sides. Output: `{ metric: share }`. */
function readNoise(path, base, candidate) {
  assert.ok(path && isAbsolute(path), "--noise names the noise.json a noise run wrote");
  assert.notEqual(candidate.digest, base.digest, "the candidate is another build than the base");
  const noise = JSON.parse(readFileSync(path, "utf8"));
  assert.equal(noise.base, base.digest, "the noise was read on this base");
  assert.ok(noise.shares && Object.keys(noise.shares).length > 0, "the noise has shares");
  return noise.shares;
}

process.exitCode = await main();
