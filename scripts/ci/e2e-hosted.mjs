import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { cpSync, createWriteStream, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
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

/** The text an e2e file holds when it must run on the owner fixture built with the clean-Mac
 * gate feature, which keeps this runner's own Node and Homebrew out of the engine's view. */
const GATE_FEATURE = "crazyagent/clean-mac-gate";

/** The prefix of the temporary root each e2e file makes and keeps when E2E_EVIDENCE is set. */
const ROOT_PREFIX = "ca-";

/** How many bytes of one rollout or engine log are kept; a larger file keeps its last part. */
const KEPT_BYTES_MAX = 2 * 1024 * 1024;

/** How many rollout and log files are kept from one temporary root. */
const KEPT_FILES_MAX = 50;

/** How many Sandbox denial lines that name the temporary folder are kept. */
const DENIALS_MAX = 5000;

/** The engine database tables kept from each temporary root's engine.db: the team's tasks,
 * agents, worktrees and messages (the project notes, where a landing refusal is told) and the
 * change sets. */
export const KEPT_TABLES = Object.freeze(["team_tasks", "team_agents", "team_worktrees", "team_messages", "landing_change_sets"]);

/** How many of the newest rows of one kept table are written. */
const KEPT_ROWS_MAX = 200;

/** The read-only queries for the kept tables one engine database holds. Inputs: the table names
 * the database lists. Output: one SELECT per kept table it holds, in KEPT_TABLES order. */
export function keptQueries(present) {
  assert.ok(Array.isArray(present), "the listed tables");
  const queries = KEPT_TABLES.filter((table) => present.includes(table)).map((table) => `SELECT * FROM ${table} ORDER BY rowid DESC LIMIT ${KEPT_ROWS_MAX};`);
  assert.ok(queries.every((query) => /^SELECT \* FROM [a-z_]+ ORDER BY rowid DESC LIMIT \d+;$/.test(query)));
  return queries;
}

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

/** The node:test files a run keeps to when E2E_HOSTED_ONLY names some, comma separated. Each
 * named file must be one of this job's node:test files. Inputs: the node:test files and the
 * variable's value. Output: the named files in job order, or every file for an empty value. */
export function onlyFiles(nodeTest, value) {
  const names = (value ?? "").split(",").map((name) => name.trim()).filter(Boolean);
  for (const name of names) assert.ok(nodeTest.includes(name), `E2E_HOSTED_ONLY names ${name}, which is not a hosted node:test file`);
  return names.length === 0 ? [...nodeTest] : nodeTest.filter((file) => names.includes(file));
}

/** The lines of a Sandbox log that name a folder, as the folder or without its /private
 * prefix, at most DENIALS_MAX. Inputs: the log text and the folder. Output: the lines. */
export function denialsUnder(text, folder) {
  assert.ok(isAbsolute(folder), "the folder is absolute");
  const names = [folder, folder.replace(/^\/private(?=\/)/, "")];
  return text.split("\n").filter((line) => names.some((name) => line.includes(name))).slice(0, DENIALS_MAX);
}

/** The local time `log show --start` reads. Input: a Date. Output: "YYYY-MM-DD HH:MM:SS". */
export function logStart(date) {
  const two = (value) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${two(date.getMonth() + 1)}-${two(date.getDate())} ${two(date.getHours())}:${two(date.getMinutes())}:${two(date.getSeconds())}`;
}

/** Keeps what tells an agent's refused write apart from a wrong working folder. For every
 * temporary root the e2e files left, it writes each git checkout's listing, status and last
 * commits, copies each Codex rollout file (its session_meta names the stand-in's cwd) and
 * each engine log, and writes the kept tables of each engine.db, read only. Then it keeps the Sandbox denials since the start that name the temporary
 * folder. The roots hold stand-in and fixture data only. Inputs: the evidence folder and the
 * run's start. Output: none; it writes evidence/roots and evidence/sandbox-deny.txt. */
function keepRoots(evidence, started) {
  const temporary = realpathSync(tmpdir());
  for (const name of readdirSync(temporary).filter((entry) => entry.startsWith(ROOT_PREFIX)).sort()) {
    const root = join(temporary, name);
    const into = join(evidence, "roots", name);
    mkdirSync(into, { recursive: true, mode: 0o700 });
    const found = (args) => spawnSync("/usr/bin/find", [root, ...args], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }).stdout.split("\n").filter(Boolean);
    const lines = [];
    for (const checkout of found(["-name", ".git", "-not", "-path", "*/node_modules/*"]).map(dirname)) {
      lines.push(`== ${checkout}`);
      for (const [program, args] of [["/bin/ls", ["-la", checkout]], ["git", ["-C", checkout, "status", "--short", "--branch", "--untracked-files=all"]], ["git", ["-C", checkout, "log", "--oneline", "-3"]]]) {
        const result = spawnSync(program, args, { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
        lines.push(`$ ${program} ${args.join(" ")} (rc ${result.status})`, result.stdout ?? "", result.stderr ?? "");
      }
    }
    writeFileSync(join(into, "checkouts.txt"), lines.join("\n"), { mode: 0o600 });
    const files = [...found(["-name", "rollout-*.jsonl"]), ...found(["-path", "*/logs/*", "-name", "*.log"])].slice(0, KEPT_FILES_MAX);
    const index = [];
    for (const [at, file] of files.entries()) {
      const copy = `${at}-${basename(file)}`;
      try {
        const bytes = readFileSync(file);
        writeFileSync(join(into, copy), bytes.subarray(Math.max(0, bytes.length - KEPT_BYTES_MAX)), { mode: 0o600 });
        index.push(`${copy} ${file} ${bytes.length}`);
      } catch (error) {
        index.push(`${copy} ${file} unread: ${error.code ?? error.message}`);
      }
    }
    writeFileSync(join(into, "files.txt"), index.join("\n"), { mode: 0o600 });
    const tables = [];
    for (const database of found(["-name", "engine.db", "-type", "f"]).slice(0, KEPT_FILES_MAX)) {
      const sqlite = (sql) => spawnSync("/usr/bin/sqlite3", ["-readonly", "-header", database, sql], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
      const listed = sqlite("SELECT name FROM sqlite_master WHERE type='table';");
      tables.push(`== ${database} (tables rc ${listed.status})`, (listed.stderr ?? "").slice(0, 2000));
      for (const query of keptQueries((listed.stdout ?? "").split("\n").slice(1))) {
        const rows = sqlite(query);
        tables.push(`$ ${query} (rc ${rows.status})`, (rows.stdout ?? "").slice(-KEPT_BYTES_MAX), (rows.stderr ?? "").slice(0, 2000));
      }
    }
    writeFileSync(join(into, "engine-db.txt"), tables.join("\n"), { mode: 0o600 });
  }
  const shown = spawnSync("/usr/bin/log", ["show", "--start", logStart(started), "--style", "compact", "--predicate", 'sender == "Sandbox"'], { encoding: "utf8", maxBuffer: 512 * 1024 * 1024 });
  const denials = denialsUnder(shown.stdout ?? "", temporary);
  writeFileSync(join(evidence, "sandbox-deny.txt"), [`log show rc ${shown.status} since ${logStart(started)} under ${temporary}: ${denials.length} lines`, (shown.stderr ?? "").slice(0, 2000), ...denials].join("\n"), { mode: 0o600 });
  console.log(`EVIDENCE roots kept; Sandbox lines under ${temporary}: ${denials.length}`);
}

/** The owner fixture an e2e file runs on: the clean-Mac gate build when the file's text names
 * the feature, else the plain build. Inputs: the file's text and both fixture paths. Output:
 * the path. */
export function fixtureFor(text, plain, gated) {
  assert.ok(isAbsolute(plain) && isAbsolute(gated) && plain !== gated, "two distinct absolute fixtures");
  return text.includes(GATE_FEATURE) ? gated : plain;
}

/** Picks a file's owner fixture and prints a FIXTURE line when it is the gated one. Inputs: the
 * file name, its text and both fixture paths. Output: the path. */
function picked(file, text, plain, gated) {
  const fixture = fixtureFor(text, plain, gated);
  if (fixture === gated) console.log(`FIXTURE ${file} clean-mac-gate ${gated}`);
  return fixture;
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
 * once, with its own E2E_EVIDENCE folder, so its engine and bridge logs are kept. Environment:
 * E2E_ENGINE, E2E_BRIDGE, WD_APP, CRAZYAGENT_DESKTOP_TEST_FIXTURE, its clean-Mac gate build
 * CRAZYAGENT_DESKTOP_TEST_FIXTURE_CLEAN_MAC_GATE, CRAZYAGENT_CHROME and E2E_HOSTED_EVIDENCE.
 * A file whose text names the gate feature gets the gated fixture. E2E_HOSTED_ONLY, when set,
 * names the only node:test files to run, and the suites and wdio configurations are then left
 * out. After the node:test files, keepRoots keeps their checkouts, rollouts, engine logs, engine
 * database tables and Sandbox denials. Output: exit status 1 when any row or run failed or a
 * file is not placed. */
async function main() {
  const started = new Date();
  const [engine, , app, fixture, gatedFixture, , evidence] = ["E2E_ENGINE", "E2E_BRIDGE", "WD_APP",
    "CRAZYAGENT_DESKTOP_TEST_FIXTURE", "CRAZYAGENT_DESKTOP_TEST_FIXTURE_CLEAN_MAC_GATE", "CRAZYAGENT_CHROME",
    "E2E_HOSTED_EVIDENCE"].map(needed);
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
  const only = process.env.E2E_HOSTED_ONLY ?? "";
  const files = onlyFiles(sorted.nodeTest, only);
  if (only) console.log(`ONLY ${files.join(" ")}; suites and wdio configurations are left out`);
  console.log(`E2E node:test files ${files.length}, at most ${FILES_AT_ONCE} at once`);
  const waiting = [...files];
  const codes = [];
  const lane = async () => {
    for (let file = waiting.shift(); file; file = waiting.shift()) {
      const env = { ...process.env, E2E_EVIDENCE: join(evidence, `node-${file.replace(/\.e2e\.mjs$/, "")}`),
        CRAZYAGENT_DESKTOP_TEST_FIXTURE: picked(file, read(file), fixture, gatedFixture) };
      mkdirSync(env.E2E_EVIDENCE, { recursive: true, mode: 0o700 });
      const nodeTest = spawn(process.execPath, ["--test", `--test-reporter=${REPORTER}`, "--test-reporter-destination=stdout",
        join("test/native", file)], { cwd: APP, env, stdio: ["ignore", "inherit", "inherit"] });
      codes.push(await new Promise((done) => nodeTest.on("close", (exit, signal) => done(exit ?? `signal ${signal}`))));
    }
  };
  await Promise.all(Array.from({ length: FILES_AT_ONCE }, lane));
  assert.equal(codes.length, files.length, "every node:test file ran once");
  const nodeCode = codes.find((code) => code !== 0) ?? 0;
  console.log(`E2E node:test rc ${nodeCode}`);
  try {
    keepRoots(evidence, started);
  } catch (error) {
    console.log(`EVIDENCE roots not kept: ${error.message}`);
  }
  for (const suite of only ? [] : SUITES) {
    const specFile = `${suite}.e2e.mjs`;
    assert.ok(sorted.suites.includes(specFile), `the closed suite ${suite} runs ${specFile}`);
    const env = { ...process.env, CRAZYAGENT_DESKTOP_TEST_APP: app, CRAZYAGENT_DESKTOP_TEST_SUITE: suite,
      CRAZYAGENT_DESKTOP_TEST_FIXTURE: picked(specFile, read(specFile), fixture, gatedFixture) };
    const run = await logged(process.execPath, [join(NATIVE, "run.mjs")], env, join(evidence, `suite-${suite}.log`));
    const completion = run.text.trim().split("\n").reverse().find((line) => line.endsWith("completion.json"));
    const driver = completion ? JSON.parse(readFileSync(completion, "utf8")).owned.find((entry) => entry.name === "webdriver") : null;
    const spec = driver ? readFileSync(driver.streams[0].path, "utf8") : "";
    if (completion) cpSync(dirname(completion), join(evidence, `suite-${suite}`), { recursive: true });
    report(specRows(spec, `suite ${suite}`), `suite ${suite}`, { code: run.code, text: `${run.text}\n${spec}` }, counts);
  }
  for (const config of only ? [] : HOSTED_CONFIGS) {
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
