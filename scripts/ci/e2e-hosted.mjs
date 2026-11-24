import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cpSync, createWriteStream, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { displays, shown } from "../../apps/desktop/test/native/test-window.mjs";
import { rowLine } from "./row-reporter.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const APP = join(ROOT, "apps/desktop");
const NATIVE = join(APP, "test/native");
const WDIO = join(APP, "node_modules/@wdio/cli/bin/wdio.js");
const REPORTER = join(ROOT, "scripts/ci/row-reporter.mjs");

/** How long one wdio configuration or closed suite may run before the job stops it. */
const RUN_MS = 45 * 60 * 1000;

/** How many distinct error lines a failed run prints as its reasons. */
const REASONS_MAX = 5;

/** How many node:test e2e files run at once on the hosted runner's three cores. */
const FILES_AT_ONCE = 2;

/** The closed suites `test/native/run.mjs` runs that need nothing outside the job. */
export const SUITES = Object.freeze([
  "model-access", "model-access-read-only", "native-auth", "pools-screen", "pools-spend", "first-run-guide",
  "first-run-skip-guide", "first-run-harnesses", "first-run-harnesses-node", "learning-replay-guide",
  "learning-replay-first-run", "refused-tool", "app-theme", "harness-states",
]);

/** The wdio configurations that need only the built app, the engine and a stand-in harness. */
export const HOSTED_CONFIGS = Object.freeze([
  "master-switch", "done-quiet", "master-idle-stop", "switch-time", "session-header", "native-terminal",
  "live-master-account", "live-terminal",
].map((name) => `${name}.wdio.conf.mjs`));

/** The configurations whose specs the closed suites run, through run.mjs. */
const SUITE_CONFIGS = Object.freeze(["model-access.wdio.conf.mjs", "native-auth.wdio.conf.mjs"]);

const OLD_RELEASE = "needs an older signed app release to start from, which this job does not build";
const TRIAL_KIT = "needs the trial kit's prepared engine, bridge, project and fixture root (TRIAL3_*, TRIAL_*)";

/** Every e2e file the hosted job does not run, with the reason it prints for it. */
export const NOT_RUN = Object.freeze({
  "engine-swap.e2e.mjs": OLD_RELEASE,
  "golden-upgrade-restore.e2e.mjs": OLD_RELEASE,
  "trial3.e2e.mjs": TRIAL_KIT,
  "trial4.e2e.mjs": TRIAL_KIT,
  "v1-flow.e2e.mjs": "needs a local Ollama model and the real Claude Code, Codex and rcli harnesses (V1_*)",
  "speed-native.e2e.mjs": "the speed suite is a timing gate, and a shared hosted runner gives no stable number",
  "speed-calls.e2e.mjs": "needs a built speed base (SPEED_BASE, build-speed-base.sh) and is a timing gate",
  "ide-window.e2e.mjs": "needs a local window id tool (IDE_WINDOW_ID_TOOL) and System Events keys under an Accessibility grant (E2E_REAL_KEYS)",
  "native-terminal-windows.e2e.mjs": "Windows only; this job runs on macos-15",
});

/** The e2e file names a wdio configuration's text names. Input: the text. Output: the names. */
function named(text) {
  return [...text.matchAll(/[a-z0-9-]+\.e2e\.mjs/g)].map((match) => match[0]);
}

/** Sorts every e2e file into how the hosted job runs it. Inputs: the e2e file names, the wdio
 * configuration names, and a reader from a file name in the native folder to its text.
 * Output: { nodeTest, hosted, suites, notRun, unplaced, twice }, each a list of file names;
 * unplaced and twice are empty when every file is run or skipped by name exactly once. */
export function classify(files, configs, read) {
  const hosted = new Set(HOSTED_CONFIGS.flatMap((config) => named(read(config))));
  const suites = new Set(SUITE_CONFIGS.flatMap((config) => named(read(config))));
  const byConfig = new Set(configs.flatMap((config) => named(read(config))));
  const sorted = { nodeTest: [], hosted: [], suites: [], notRun: [], unplaced: [], twice: [] };
  for (const file of files) {
    const places = [
      ["nodeTest", !byConfig.has(file) && !Object.hasOwn(NOT_RUN, file) && /from "node:test"/.test(read(file))],
      ["hosted", hosted.has(file)],
      ["suites", suites.has(file)],
      ["notRun", Object.hasOwn(NOT_RUN, file)],
    ].filter(([, here]) => here).map(([place]) => place);
    if (places.length === 0) sorted.unplaced.push(file);
    else if (places.length > 1) sorted.twice.push(`${file} (${places.join(", ")})`);
    else sorted[places[0]].push(file);
  }
  return sorted;
}

/** Reads the rows a wdio spec reporter printed: `✓`, `✖` or `-` before a row's name, under
 * the `»` line naming its spec file. Inputs: the output and the file to name when none is
 * printed. Output: [{ file, name, result }] with result ok, not ok or skip. */
export function specRows(text, fallback) {
  const results = { "✓": "ok", "✖": "not ok", "-": "skip" };
  const rows = [];
  let file = fallback;
  for (const raw of text.split("\n")) {
    const line = raw.replace(/^\[[^\]]*\]\s?/, "");
    const spec = line.match(/^»\s+(\S+)$/);
    if (spec) file = spec[1].replace(/^.*\/test\/native\//, "");
    const row = line.match(/^\s+([✓✖-])\s(.+?)\s*$/);
    if (row) rows.push({ file, name: row[2], result: results[row[1]] });
  }
  return rows;
}

/** A free port on the loopback address above 1024. Output: the port. */
async function freePort() {
  const server = createServer();
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  assert.ok(Number.isInteger(port) && port > 1024);
  return port;
}

/** Runs a program with its output written to a file, stopped after RUN_MS. Inputs: the program,
 * its arguments, its environment and the log file. Output: { code, text } with text the log. */
async function logged(program, args, env, log) {
  const out = createWriteStream(log, { mode: 0o600 });
  const child = spawn(program, args, { cwd: APP, env, stdio: ["ignore", "pipe", "pipe"], detached: true });
  child.stdout.pipe(out, { end: false });
  child.stderr.pipe(out, { end: false });
  const timer = setTimeout(() => process.kill(-child.pid, "SIGKILL"), RUN_MS);
  const code = await new Promise((done) => child.on("close", (exit, signal) => done(exit ?? `signal ${signal}`)));
  clearTimeout(timer);
  await new Promise((done) => out.end(done));
  return { code, text: readFileSync(log, "utf8") };
}

/** Prints the rows of one run and a row for the run itself when it failed with no failed row,
 * and counts them. Inputs: the rows, the run's name, its exit and output, and the counts. */
function report(rows, name, { code, text }, counts) {
  for (const row of rows) {
    counts[row.result] += 1;
    process.stdout.write(rowLine(row.file, row.name, row.result));
  }
  if (code !== 0) {
    const errors = [...new Set(text.split("\n").filter((line) => /\b(?:\w*Error|BLOCKED)\b/.test(line) && !/^\s+at /.test(line)))];
    for (const line of errors.slice(0, REASONS_MAX)) console.log(`REASON ${name} :: ${line.trim().slice(0, 300)}`);
  }
  if (code !== 0 && !rows.some((row) => row.result === "not ok")) {
    counts["not ok"] += 1;
    const last = text.trim().split("\n").filter(Boolean).slice(-3).join(" | ");
    process.stdout.write(rowLine(name, "(the run)", "not ok", `exit ${code}: ${last}`));
  }
}

/** Reads a path the environment must name. Input: the name. Output: the absolute path. */
function needed(name) {
  const path = process.env[name];
  assert.ok(path && isAbsolute(path), `${name} must name an absolute path`);
  return path;
}

/** Runs every hosted e2e row on this Mac and prints each row's name and result, then the files
 * not run with their reason, then a tally. Each node:test file runs in its own process, two at
 * once, with its own E2E_EVIDENCE folder, so its engine and bridge logs are kept. Environment: E2E_ENGINE, E2E_BRIDGE, WD_APP,
 * CRAZYAGENT_DESKTOP_TEST_FIXTURE, CRAZYAGENT_CHROME and E2E_HOSTED_EVIDENCE. Output: exit
 * status 1 when any row or run failed or a file is not placed. */
async function main() {
  const [engine, , app, fixture, , evidence] = ["E2E_ENGINE", "E2E_BRIDGE", "WD_APP",
    "CRAZYAGENT_DESKTOP_TEST_FIXTURE", "CRAZYAGENT_CHROME", "E2E_HOSTED_EVIDENCE"].map(needed);
  mkdirSync(evidence, { recursive: true, mode: 0o700 });
  printDisplays();
  const listing = readdirSync(NATIVE);
  const read = (file) => readFileSync(join(NATIVE, file), "utf8");
  const sorted = classify(listing.filter((file) => file.endsWith(".e2e.mjs")).sort(),
    listing.filter((file) => file.endsWith(".wdio.conf.mjs")).sort(), read);
  const counts = { ok: 0, "not ok": 0, skip: 0 };
  for (const file of [...sorted.unplaced, ...sorted.twice]) {
    counts["not ok"] += 1;
    process.stdout.write(rowLine(file, "(placement)", "not ok", "an e2e file the hosted job neither runs nor names as not run, or places twice"));
  }
  console.log(`E2E node:test files ${sorted.nodeTest.length}, at most ${FILES_AT_ONCE} at once`);
  const waiting = [...sorted.nodeTest];
  const codes = [];
  const lane = async () => {
    for (let file = waiting.shift(); file; file = waiting.shift()) {
      const env = { ...process.env, E2E_EVIDENCE: join(evidence, `node-${file.replace(/\.e2e\.mjs$/, "")}`) };
      mkdirSync(env.E2E_EVIDENCE, { recursive: true, mode: 0o700 });
      const nodeTest = spawn(process.execPath, ["--test", `--test-reporter=${REPORTER}`, "--test-reporter-destination=stdout",
        join("test/native", file)], { cwd: APP, env, stdio: ["ignore", "inherit", "inherit"] });
      codes.push(await new Promise((done) => nodeTest.on("close", (exit, signal) => done(exit ?? `signal ${signal}`))));
    }
  };
  await Promise.all(Array.from({ length: FILES_AT_ONCE }, lane));
  assert.equal(codes.length, sorted.nodeTest.length, "every node:test file ran once");
  const nodeCode = codes.find((code) => code !== 0) ?? 0;
  console.log(`E2E node:test rc ${nodeCode}`);
  for (const suite of SUITES) {
    const env = { ...process.env, CRAZYAGENT_DESKTOP_TEST_APP: app, CRAZYAGENT_DESKTOP_TEST_FIXTURE: fixture, CRAZYAGENT_DESKTOP_TEST_SUITE: suite };
    const run = await logged(process.execPath, [join(NATIVE, "run.mjs")], env, join(evidence, `suite-${suite}.log`));
    const completion = run.text.trim().split("\n").reverse().find((line) => line.endsWith("completion.json"));
    const driver = completion ? JSON.parse(readFileSync(completion, "utf8")).owned.find((entry) => entry.name === "webdriver") : null;
    const spec = driver ? readFileSync(driver.streams[0].path, "utf8") : "";
    if (completion) cpSync(dirname(completion), join(evidence, `suite-${suite}`), { recursive: true });
    report(specRows(spec, `suite ${suite}`), `suite ${suite}`, { code: run.code, text: `${run.text}\n${spec}` }, counts);
  }
  for (const config of HOSTED_CONFIGS) {
    const root = join(realpathSync(mkdtempSync("/tmp/ms-")), "root");
    const env = { ...process.env, WD_APP: app, WD_PORT: String(await freePort()), MS_ENGINE: engine, MS_ROOT: root,
      MS_EVIDENCE: join(evidence, config.replace(/\.wdio\.conf\.mjs$/, "")) };
    mkdirSync(env.MS_EVIDENCE, { recursive: true, mode: 0o700 });
    const run = await logged(process.execPath, [WDIO, "run", join("test/native", config)], env, join(evidence, `${config}.log`));
    report(specRows(run.text, config), config, run, counts);
  }
  for (const [file, reason] of Object.entries(NOT_RUN)) process.stdout.write(rowLine(file, "(all rows)", `NOT RUN on hosted: ${reason}`));
  console.log(`E2E wdio and suite rows ok ${counts.ok} not-ok ${counts["not ok"]} skip ${counts.skip}; node:test rc ${nodeCode}; not run ${Object.keys(NOT_RUN).length} files`);
  process.exitCode = nodeCode === 0 && counts["not ok"] === 0 ? 0 : 1;
}

/** Prints each display as the test window reads it, one DISPLAY line each. Output: none. */
function printDisplays() {
  for (const display of displays()) console.log(`DISPLAY ${shown(display)} frame ${display.width}x${display.height}`);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (process.argv[2] === "--displays") printDisplays();
  else await main();
}
