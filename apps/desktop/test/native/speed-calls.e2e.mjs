import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { test } from "node:test";
import { setTimeout as pause } from "node:timers/promises";
import { withPage } from "../browser.mjs";
import { INTERNALS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop } from "./e2e-harness.mjs";
import { countFindings } from "./speed-budget.mjs";
import { seedSide, speedSettings } from "./speed-seed.mjs";

const SESSIONS_C1 = 3;
const SETTLE_MS = 2000;
const IDLE_MS = 60000;
const LOOK_MS = 50;
const CALLS_MAX = 20000;
const HOME = "#/home";
const SWITCHES = [
  ["project", ".sidebar .nav-projects a.nav-item"],
  ["agents", "a#tab-agents"],
  ["settings", '.sidebar nav.app-nav a[href="#/settings/accounts"]'],
  ["metrics", '.sidebar nav.app-nav a[href="#/metrics"]'],
  ["home", `.sidebar nav.app-nav a[href="${HOME}"]`],
];

/** Reads one side's built artifacts, as build-speed-base.sh lays them out. Input: the
 * environment variable naming the folder. Output: the engine, bridge and dist paths and the
 * engine's sha256. */
function artifacts(name) {
  const folder = required(name, "speed artifact folder with bin/ and dist/ (build-speed-base.sh)");
  const side = { folder, engine: join(folder, "bin", "crazyagent"), bridge: join(folder, "bin", "desktop-e2e-bridge"), dist: join(folder, "dist") };
  for (const path of [side.engine, side.bridge, join(side.dist, "index.html")]) assert.ok(existsSync(path), `${name} has ${path}`);
  side.sha256 = createHash("sha256").update(readFileSync(side.engine)).digest("hex");
  assert.match(side.sha256, /^[0-9a-f]{64}$/);
  return side;
}

/** Makes one side's run record and its private folders before anything starts, so the caller
 * can stop whatever a failed boot did start. Input: the label. Output: the run. */
function newRun(label) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), `ca-sc-${label}-`)));
  const run = { label, root, runtime: join(root, "engine"), home: join(root, "home"),
    log: { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false } };
  for (const folder of [run.runtime, run.home, join(run.home, ".codex"), join(root, "projects")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  return run;
}

/** Starts one side's real engine and bridge on a stand-in HOME, with the owner's profile and
 * one project made through the bridge, and serves its dist, filling the run as each part
 * starts. Inputs: the run, the artifacts and the evidence folder. Output: none. */
async function boot(run, side, evidence) {
  const { label, root, runtime, home } = run;
  writeFileSync(join(runtime, "global.toml"), speedSettings(await closedPort()), { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  ({ child: run.engine } = await launch(side.engine,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
    { ...env }, evidence, `${label}-engine`, /^ready$/m));
  const started = await launch(side.bridge, [join(runtime, "engine.sock")], { ...env }, evidence, `${label}-bridge`, /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = started.child;
  run.bridge = { port: Number(started.match[1]), token: started.match[2] };
  run.seeded = await seedSide(run.bridge, join(root, "projects"), SESSIONS_C1);
  assert.equal(run.seeded.sessions, SESSIONS_C1, `${label}: the project has its master sessions`);
  run.site = await serveApp(side.dist, run.bridge, [], run.log);
}

/** Stops one side's file server, bridge, engine and agent terminals, whichever started. Input:
 * the side. Output: none. */
async function halt(run) {
  run.log.closing = true;
  run.site?.server.close();
  await stop(run.bridgeProcess);
  await stop(run.engine);
  await endTerminals(run.runtime);
}

/** Waits until a page is usable: its address, a heading in main and no skeleton left in main.
 * Inputs: the page's evaluate and the address. Output: none. */
async function usable(evaluate, hash) {
  assert.match(hash, /^#\//, "an app address");
  const check = `location.hash === ${JSON.stringify(hash)} && Boolean(document.querySelector("main h1")) && !document.querySelector("main .skeleton")`;
  for (const deadline = Date.now() + STEP_MS; !(await evaluate(check)); await pause(LOOK_MS)) {
    if (Date.now() > deadline) assert.fail(`${hash} never became usable: ${await evaluate("document.body?.innerText?.slice(0, 2000) ?? ''")}`);
  }
}

/** Counts the bridge calls made since a mark. Inputs: the call log and the mark. Output:
 * `{ cmd: count }`. */
function counted(log, mark) {
  assert.ok(mark <= log.calls.length && log.calls.length <= CALLS_MAX, `the call log stays bounded: ${log.calls.length}`);
  const counts = {};
  for (const cmd of log.calls.slice(mark)) counts[cmd] = (counts[cmd] ?? 0) + 1;
  return counts;
}

/** Walks one side's page the owner's way and counts its bridge calls per scenario: the start
 * on Home, each sidebar or tab switch, and a minute idle on Home and on the project. Input: the
 * running side. Output: `{ scenario: { cmd: count } }`. */
async function walk(run, evidence) {
  return withPage(async ({ evaluate, send }) => {
    const me = owner({ evaluate, send }, evidence);
    const counts = {};
    await send("Page.enable");
    await send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
    await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
    let mark = run.log.calls.length;
    await send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/${HOME}` });
    await usable(evaluate, HOME);
    await pause(SETTLE_MS);
    counts.start = counted(run.log, mark);
    for (const [page, selector] of SWITCHES) {
      const hash = await evaluate(`document.querySelector(${JSON.stringify(selector)})?.getAttribute("href") ?? null`);
      assert.ok(hash, `${run.label}: the ${page} link is shown (${selector})`);
      mark = run.log.calls.length;
      await me.follow(selector);
      await usable(evaluate, hash);
      await pause(SETTLE_MS);
      counts[`switch:${page}`] = counted(run.log, mark);
    }
    mark = run.log.calls.length;
    await pause(IDLE_MS);
    counts["idle:home"] = counted(run.log, mark);
    const project = await evaluate(`document.querySelector(${JSON.stringify(SWITCHES[0][1])}).getAttribute("href")`);
    await me.follow(SWITCHES[0][1]);
    await usable(evaluate, project);
    await pause(SETTLE_MS);
    mark = run.log.calls.length;
    await pause(IDLE_MS);
    counts["idle:project"] = counted(run.log, mark);
    await me.capture(`${run.label}-speed-calls-end`);
    assert.deepEqual(await evaluate("window.__e2eErrors"), [], `${run.label}: the page raised no errors`);
    return counts;
  });
}

test("C1: the candidate makes no bridge call 2.3.2 never made and none more than once beyond it, at start, on each page switch and in a minute idle (DEC-169)", { timeout: 900000 }, async () => {
  const base = artifacts("SPEED_BASE");
  const candidate = artifacts("SPEED_CANDIDATE");
  assert.notEqual(base.sha256, candidate.sha256, "the base and the candidate are different engines");
  process.umask(0o077);
  const kept = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : null;
  const evidence = kept ?? realpathSync(mkdtempSync(join(tmpdir(), "ca-sc-evidence-")));
  const counts = {};
  for (const [label, side] of [["base", base], ["candidate", candidate]]) {
    const run = newRun(label);
    try {
      await boot(run, side, evidence);
      counts[label] = await walk(run, evidence);
      writeFileSync(join(evidence, `calls-${label}.json`), JSON.stringify({ engine_sha256: side.sha256, folder: side.folder, counts: counts[label], calls: run.log.calls }, null, 1));
    } finally {
      await halt(run);
      if (!kept) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
    }
  }
  const findings = countFindings(counts.base, counts.candidate);
  writeFileSync(join(evidence, "findings.json"), JSON.stringify(findings, null, 1));
  assert.deepEqual(findings, [], `calls 2.4.x adds over 2.3.2 (scenario cmd base -> candidate):\n${findings.map((item) => `${item.scenario} ${item.cmd} ${item.base} -> ${item.candidate}`).join("\n")}\nevidence ${evidence}`);
});
