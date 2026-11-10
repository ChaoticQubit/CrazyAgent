import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { CHECKS, TUI_CHECKS, loadChecks, plan } from "./trial4-checks.mjs";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";

const DESKTOP = fileURLToPath(new URL("../..", import.meta.url));
const WDIO = join(DESKTOP, "node_modules", "@wdio", "cli", "bin", "wdio.js");
const CONF = join(DESKTOP, "test", "native", "trial4.wdio.conf.mjs");
const SLUGS = TRIAL3_STEPS.map((step) => step.slug);
const RUN_MS = 120000;
const SPEC = join(DESKTOP, "test", "native", "trial4.e2e.mjs");
const OWNER_WRITERS = /scratchAccount\(|desktop_(?:add|remove)_account|button=New pool/u;
const CHECK_TITLE = /^([A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+) \(/u;
const ACCOUNT_WRITERS = ["N-D2", "N-D19", "N-D32"];

/** Writes check modules into a new scratch folder. Input: { file name: module source }.
 * Output: the folder's real path. */
function modules(files) {
  const folder = realpathSync(mkdtempSync("/tmp/ca-t4c-"));
  for (const [name, source] of Object.entries(files)) writeFileSync(join(folder, name), source);
  return folder;
}

/** Loads the real trial 4 spec in a node of its own, under stand-ins for mocha's globals, and
 * lists what it would do. Inputs: the mode and the TRIAL_OWNER_MS value, or null for none.
 * Output: { run, skipped }, the ids of the checks it runs and of the ones it shows as skipped. */
function specPlan(mode, ownerMs) {
  const folder = realpathSync(mkdtempSync("/tmp/ca-t4c-"));
  const loader = `const run = [];
const skipped = [];
globalThis.describe = (title, body) => body();
globalThis.before = () => {};
globalThis.it = (title) => run.push(title);
globalThis.it.skip = (title) => skipped.push(title);
await import(${JSON.stringify(pathToFileURL(SPEC).href)});
console.log(JSON.stringify({ run, skipped }));`;
  try {
    const env = { PATH: process.env.PATH, HOME: folder, LANG: "en_US.UTF-8", TMPDIR: folder, TRIAL3_MODE: mode,
      TRIAL_EVIDENCE: join(folder, "evidence"), TRIAL_PROJECT: join(folder, "project"), ...(ownerMs === null ? {} : { TRIAL_OWNER_MS: ownerMs }) };
    const result = spawnSync(process.execPath, ["--input-type=module", "-e", loader], { cwd: DESKTOP, env, encoding: "utf8", timeout: RUN_MS, killSignal: "SIGKILL" });
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    const { run, skipped } = JSON.parse(result.stdout.trim().split("\n").at(-1));
    const ids = (titles) => titles.map((title) => CHECK_TITLE.exec(title)?.[1]).filter(Boolean);
    return { run: ids(run), skipped: ids(skipped) };
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
}

/** Runs the real N-D4 check against a stand-in window whose Graphify switch flips when it is
 * pressed. Input: the run's mode. Output: { presses, on, detail }, the presses, the switch at
 * the end and the check's detail. */
async function graphifyRun(mode) {
  const { checks: [check] } = await import("./trial4/n-d4-graphify-once.mjs");
  let on = true;
  let presses = 0;
  const rows = () => [
    { section: "Tools", title: "Graphify command", words: ["Found"], on: null, id: null },
    { section: "Code graph", title: "Graphify skill for agents", words: [], on, id: "pack-graphify" },
  ];
  const element = {
    isDisplayed: async () => true,
    isClickable: async () => true,
    getAttribute: async (name) => (name === "aria-checked" ? String(on) : null),
    click: async () => {
      presses += 1;
      on = !on;
    },
  };
  const holds = async (condition) => assert(await condition(), "a stand-in wait did not hold");
  Object.assign(globalThis, { browser: { execute: async () => rows(), waitUntil: holds }, $: () => element, $$: async () => [element] });
  try {
    const kit = { MODE: mode, UI_MS: 1, go: async () => {}, until: holds, inView: async () => {}, screen: async () => "" };
    return { detail: await check.run(kit), presses, on };
  } finally {
    for (const name of ["browser", "$", "$$"]) delete globalThis[name];
  }
}

/** One check module's source. Input: the fields to change from a valid check. Output: the source. */
function check(fields = {}) {
  const base = { id: "N-D1", op: ["OP-7"], title: "Sign in again", why: "owner: I can't even sign in", after: "land-on", ...fields };
  const run = "run" in fields ? fields.run : "async (kit) => `ran with ${typeof kit.go}`";
  delete base.run;
  return `export const checks = [{ ...${JSON.stringify(base)}, run: ${run} }];\n`;
}

test("every committed trial4 check module has a unique id, OP rows, a title, a why, a real after and a run function", async (t) => {
  const checks = await loadChecks(CHECKS, SLUGS);
  t.diagnostic(`${checks.length} checks in ${CHECKS}`);
  for (const loaded of checks) assert.equal(typeof loaded.run, "function", loaded.id);
});

test("no committed trial4 check hands an XPath to the CSS-only text and texts helpers", () => {
  const xpathCalls = [];
  let calls = 0;
  for (const name of readdirSync(CHECKS).filter((file) => file.endsWith(".mjs")).sort()) {
    const source = readFileSync(join(CHECKS, name), "utf8");
    const xpaths = new Set([...source.matchAll(/^const ([A-Z_]+) = [`"]\(?\/\//gmu)].map((match) => match[1]));
    for (const call of source.matchAll(/\btexts?\(\s*([^,)]+)/gu)) {
      calls += 1;
      const argument = call[1].trim();
      if (/^[`"]\(?(?:\$\{[^}]+\})?\/\//u.test(argument) || xpaths.has(argument)) xpathCalls.push(`${name}: ${argument}`);
    }
  }
  assert(calls > 0, "the scan read the checks' text and texts calls");
  assert.deepEqual(xpathCalls, []);
});

test("every committed tui4 check module in scripts/trial/tui4 passes the same convention, and no id is both a desktop and a TUI check", async (t) => {
  assert.equal(TUI_CHECKS, fileURLToPath(new URL("../../../../scripts/trial/tui4/", import.meta.url)));
  const tui = await loadChecks(TUI_CHECKS, SLUGS);
  t.diagnostic(`${tui.length} TUI checks in ${TUI_CHECKS}`);
  const desktop = await loadChecks(CHECKS, SLUGS);
  assert.equal(plan(TRIAL3_STEPS, [...desktop, ...tui]).length, SLUGS.length + desktop.length + tui.length);
});

test("no trial step changes the owner's Terminal input, since a restore cannot survive an interrupted run", () => {
  const steps = [fileURLToPath(new URL("./trial3-steps.mjs", import.meta.url)), fileURLToPath(new URL("./trial-kit.mjs", import.meta.url))];
  const sources = [...steps, ...readdirSync(CHECKS).filter((file) => file.endsWith(".mjs")).map((file) => join(CHECKS, file))];
  assert(sources.length > steps.length, "the scan read the check modules");
  const writers = (spellings) => sources.filter((path) => spellings.some((spelling) => readFileSync(path, "utf8").includes(spelling)));
  assert.deepEqual(writers(['"Terminal input"', "localStorage.setItem"]), []);
});

test("a live trial leaves out every check that writes the owner's accounts or pools and shows each one as skipped", () => {
  const { run, skipped } = specPlan("live", null);
  for (const id of ["N-D1", ...ACCOUNT_WRITERS]) {
    assert(!run.includes(id), `${id} would run live and write the owner's data`);
    assert(skipped.includes(id), `${id} would vanish from the run without a word`);
  }
  assert(run.includes("N-D4"), "the Graphify check still reads the owner's Pack page live");
});

test("a fixture trial runs the account and pool checks, on the fixture's own engine", () => {
  const { run, skipped } = specPlan("fixture", null);
  for (const id of ACCOUNT_WRITERS) assert(run.includes(id), `${id} is proven nowhere`);
  assert.deepEqual(skipped, ["N-D1"], "nobody signs in on a fixture");
});

test("a live trial runs the sign-in check only for an owner who set how long to wait, and still no other writer", () => {
  const { run, skipped } = specPlan("live", "600000");
  assert(run.includes("N-D1"), "the owner asked for the sign-in check");
  for (const id of ACCOUNT_WRITERS) assert(skipped.includes(id), `${id} would run live`);
});

test("N-D4 on a live trial only reads: the owner's app-wide Graphify switch is never pressed", async () => {
  const seen = await graphifyRun("live");
  assert.equal(seen.presses, 0, "pressed on the owner's real engine, where an interrupted run leaves it off for every agent");
  assert.equal(seen.on, true);
  assert.match(seen.detail, /Graphify skill for agents shows once in Code graph/);
});

test("N-D4 on a fixture presses the switch off and back and leaves it as it found it", async () => {
  const seen = await graphifyRun("fixture");
  assert.equal(seen.presses, 2);
  assert.equal(seen.on, true);
});

test("every check that makes scratch accounts or pools is kept off a live trial", async () => {
  const checks = await loadChecks(CHECKS, SLUGS);
  const writers = checks.filter((loaded) => OWNER_WRITERS.test(readFileSync(join(CHECKS, loaded.file), "utf8")));
  assert(writers.length >= 4, `the scan found the account checks, got ${writers.length}`);
  assert.deepEqual(writers.filter((loaded) => !["fixture", "asked"].includes(loaded.only)).map((loaded) => loaded.id), []);
});

test("a native window given a home of its own also has its web store moved there, as HOME alone leaves it in the real user Library", () => {
  const native = fileURLToPath(new URL(".", import.meta.url));
  const windows = readdirSync(native).filter((file) => file.endsWith(".wdio.conf.mjs"))
    .flatMap((file) => readFileSync(join(native, file), "utf8").split("\n"))
    .filter((line) => /CRAZYAGENT_DESKTOP_SOCKET.*\bHOME: /.test(line));
  assert.equal(windows.length, 2);
  for (const line of windows) assert.match(line, /CFFIXED_USER_HOME: /);
});

test("plan refuses an id that a desktop and a TUI module both use", async () => {
  const desktop = modules({ "n-t1-a.mjs": check({ id: "N-T1" }) });
  const tui = modules({ "n-t1-b.mjs": check({ id: "N-T1", after: "end" }) });
  try {
    const checks = [...await loadChecks(desktop, SLUGS), ...(await loadChecks(tui, SLUGS)).map((loaded) => ({ ...loaded, surface: "tui" }))];
    assert.throws(() => plan(TRIAL3_STEPS, checks), /N-T1 is in n-t1-a\.mjs and n-t1-b\.mjs/);
  } finally {
    rmSync(desktop, { recursive: true, force: true });
    rmSync(tui, { recursive: true, force: true });
  }
});

test("checks run right after the trial 3 step they name, in file order, and end checks run last", async () => {
  const folder = modules({
    "n-d2-b.mjs": check({ id: "N-D2", after: "end" }),
    "n-d1-a.mjs": `export const checks = [${["N-D1", "N-D3"].map((id) => `{ id: "${id}", op: ["OP-1", "OP-6"], title: "t ${id}", why: "w", after: "land-on", run: async () => "ok" }`).join(", ")}];\n`,
  });
  try {
    const checks = await loadChecks(folder, SLUGS);
    assert.deepEqual(checks.map((loaded) => [loaded.id, loaded.file]), [["N-D1", "n-d1-a.mjs"], ["N-D3", "n-d1-a.mjs"], ["N-D2", "n-d2-b.mjs"]]);
    const order = plan(TRIAL3_STEPS, checks).map((item) => item.slug);
    const land = order.indexOf("land-on");
    assert.deepEqual(order.slice(land, land + 4), ["land-on", "n-d1", "n-d3", "background"]);
    assert.deepEqual(order.slice(-2), ["final-tree", "n-d2"]);
    assert.equal(order.length, SLUGS.length + 3);
    assert.equal(await checks[0].run({ go() {} }), "ok");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a missing checks folder loads no checks", async () => {
  const folder = modules({});
  try {
    assert.deepEqual(await loadChecks(join(folder, "trial4"), SLUGS), []);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a check module that breaks the convention is refused with its file and the rule", async () => {
  const cases = [
    [{ "a.mjs": check(), "b.mjs": check() }, /N-D1 is in a\.mjs and b\.mjs/],
    [{ "a.mjs": check({ after: "no-such-step" }) }, /a\.mjs N-D1: after must name a trial 3 step or "end", not no-such-step/],
    [{ "a.mjs": check({ op: ["7"] }) }, /a\.mjs N-D1: op must list OP-<n> rows/],
    [{ "a.mjs": check({ op: [] }) }, /a\.mjs N-D1: op must list OP-<n> rows/],
    [{ "a.mjs": check({ op: ["MAP-0"] }) }, /a\.mjs N-D1: op must list OP-<n> rows/],
    [{ "a.mjs": check({ op: ["MAPS-1"] }) }, /a\.mjs N-D1: op must list OP-<n> rows/],
    [{ "a.mjs": check({ id: "nd1" }) }, /a\.mjs: id must look like N-D1/],
    [{ "a.mjs": check({ title: "" }) }, /a\.mjs N-D1: title must be a non-empty string/],
    [{ "a.mjs": check({ why: 3 }) }, /a\.mjs N-D1: why must be a non-empty string/],
    [{ "a.mjs": check({ only: "sometimes" }) }, /a\.mjs N-D1: only must be "fixture" or "asked"/],
    [{ "a.mjs": check({ run: "\"not a function\"" }) }, /a\.mjs N-D1: run must be a function/],
    [{ "a.mjs": "export const check = {};\n" }, /a\.mjs: exports no non-empty checks list/],
    [{ "a.mjs": "export const checks = [];\n" }, /a\.mjs: exports no non-empty checks list/],
  ];
  for (const [files, refusal] of cases) {
    const folder = modules(files);
    try {
      await assert.rejects(loadChecks(folder, SLUGS), refusal);
    } finally {
      rmSync(folder, { recursive: true, force: true });
    }
  }
});

test("a check may name a MAP-<n> row of the 18.15 coverage list, which has no OP number yet", async () => {
  const folder = modules({ "a.mjs": check({ op: ["MAP-1"] }) });
  try {
    assert.deepEqual((await loadChecks(folder, SLUGS)).map((loaded) => loaded.op), [["MAP-1"]]);
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});

test("a live trial 4 run with no engine socket stops before any spec or window starts", () => {
  const folder = realpathSync(mkdtempSync("/tmp/ca-t4c-"));
  try {
    mkdirSync(join(folder, "project", ".git"), { recursive: true });
    const env = { PATH: process.env.PATH, HOME: process.env.HOME, LANG: "en_US.UTF-8", TMPDIR: folder, TRIAL3_MODE: "live",
      TRIAL3_APP: join(folder, "no-such-app"), TRIAL_EVIDENCE: join(folder, "evidence"), WD_PORT: "4798",
      CRAZYAGENT_DESKTOP_SOCKET: join(folder, "missing.sock"), TRIAL_PROJECT: join(folder, "project"), TRIAL_PROMPT: CONF };
    const result = spawnSync(process.execPath, [WDIO, "run", CONF], { cwd: DESKTOP, env, encoding: "utf8", timeout: RUN_MS, killSignal: "SIGKILL" });
    const output = `${result.stdout}\n${result.stderr}`;
    assert.notEqual(result.status, 0, output);
    assert.match(output, /cannot start: no engine socket at /, output);
    assert.doesNotMatch(output, /RUNNING in /, "no spec ran");
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
});
