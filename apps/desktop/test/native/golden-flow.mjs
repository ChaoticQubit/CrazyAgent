import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, closedPort, endTerminals, owner, required, serveApp, silentProvider, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const WAIT_MS = 90000;
const ONBOARD_MS = 120000;
const GIT = "/usr/bin/git";
const WAIT_MAX_MS = 300000;
const STEPS_MAX = 64;
export const POOL = "local-pool";

/** Starts one golden flow as a first run leaves the app: a real engine on a private root with a
 * scratch HOME and one pool, the bridge, a saved profile and projects folder and the built app,
 * as the flow's first step. It fills the caller's empty run, so `stopGolden` ends what a failed
 * start left. Inputs: that run, the flow's name (lowercase words joined by "-") and, for a team
 * flow, the Codex stand-in's absolute path, pinned with a silent provider. Output: none. */
export async function startGolden(run, flow, { codex = null } = {}) {
  assert.match(flow, /^[a-z]+(-[a-z]+)*$/);
  assert.ok(codex === null || (isAbsolute(codex) && existsSync(codex)), `the Codex stand-in: ${codex}`);
  assert.ok(flow.length <= 64);
  assert.deepEqual(Object.keys(run), [], "a golden run starts empty");
  Object.assign(run, { flow, steps: [], started: Date.now(), finished: false });
  run.mark = run.started;
  run.engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  run.bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  run.root = realpathSync(mkdtempSync(join(tmpdir(), "ca-golden-")));
  run.evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(run.root, "evidence");
  const runtime = join(run.root, "engine");
  run.runtime = runtime;
  const home = join(run.root, "home");
  run.projects = join(run.root, "projects");
  for (const folder of [runtime, run.evidence, home, join(home, ".codex"), run.projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  rmSync(recordPath(run), { force: true });
  run.provider = codex === null ? null : await silentProvider();
  const provider = run.provider === null ? await closedPort() : run.provider.port;
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "golden-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "${POOL}", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  run.log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  const pinned = codex === null ? [] : ["--codex-executable", codex];
  const args = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings", ...pinned];
  ({ child: run.engine } = await launch(run.engineBinary, args, { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await launch(run.bridgeBinary, [join(runtime, "engine.sock")], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = started.child;
  run.bridge = { port: Number(started.match[1]), token: started.match[2] };
  assert.equal((await value(run.bridge, "desktop_connect")).connected, true, "the bridge connects to the golden engine");
  assert.equal((await value(run.bridge, "desktop_save_owner_profile", { profile: { name: "Golden Owner" }, generation: 1 })).outcome, "applied");
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: run.projects, generation: 2 })).outcome, "applied");
  const dist = join(run.root, "dist");
  await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
  assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
  run.site = await serveApp(dist, run.bridge, [], run.log);
  step(run, "start the engine and the app");
}

/** Ends one step of the flow, timed from the end of the one before.
 * Inputs: the run and the step's name, a fixed text that holds no path and no name the owner
 * typed. Output: none. */
export function step(run, name) {
  assert.ok(typeof name === "string" && name.length > 0 && !run.finished);
  assert.ok(run.steps.length < STEPS_MAX, `a flow has at most ${STEPS_MAX} steps`);
  const now = Date.now();
  run.steps.push({ step: name, ms: now - run.mark });
  run.mark = now;
}

/** Ends the flow as passed: writes its run record, then reads it back from disk.
 * Input: the run.
 * Output: the record as the evidence folder holds it, or null when it holds none. */
export function finishGolden(run) {
  assert.ok(!run.finished);
  run.finished = true;
  writeRecord(run, "passed");
  return existsSync(recordPath(run)) ? JSON.parse(readFileSync(recordPath(run), "utf8")) : null;
}

/** Stops the app, the bridge, the engine, its provider and its agent terminals; an unfinished
 * flow's record says failed at the steps it reached, and a tmux server left behind fails it.
 * Keeps the root only when E2E_EVIDENCE names the evidence folder. Input: the run, as full as
 * its start got. Output: none. */
export async function stopGolden(run) {
  if (run.log) run.log.closing = true;
  if (run.evidence && !run.finished && run.steps.length > 0) writeRecord(run, "failed");
  if (run.evidence && run.log) writeFileSync(join(run.evidence, "calls.json"), JSON.stringify(run.log.calls, null, 1));
  run.site?.server.close();
  await stop(run.bridgeProcess);
  await stop(run.engine);
  run.provider?.server.close();
  if (run.runtime) await endTerminals(run.runtime);
  const left = run.runtime ? terminalServers(run.runtime) : [];
  if (run.root && !process.env.E2E_EVIDENCE) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  assert.deepEqual(left, [], "no tmux server outlives the golden engine");
}

/** Makes an owner repository: a git repository on main with a README in one commit.
 * Input: an empty folder, made first if missing.
 * Output: none. */
export function ownerRepo(folder) {
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "README.md"), "# Fruit stand\n\nSells fruit.\n");
  const identity = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...identity, "commit", "-q", "-m", "Start the fruit stand"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Makes an owner repository a project as onboarding does, then gives it the golden pool and
 * one Codex sub-agent with team tools, as the app's team rows do. Inputs: the run, the
 * repository and the first of the three write generations it uses. Output: the project id. */
export async function onboard(run, repo, generation) {
  const receipt = await value(run.bridge, "desktop_start_onboarding", { path: repo, pool: POOL, generation });
  assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
  const project = receipt.project_id;
  const busy = (step) => ["waiting", "running"].includes(step.kind);
  await waitUntil("onboarding to settle", () => value(run.bridge, "desktop_onboarding", { id: project }), (view) => !busy(view.graph) && !busy(view.mapping), ONBOARD_MS);
  assert.equal((await value(run.bridge, "desktop_open_project", { id: project })).project_id, project, "the new project opens");
  const pools = await value(run.bridge, "desktop_model_access_page", { collection: "pools", next: false });
  assert.equal(pools.writable, true, "the golden engine's settings are writable");
  const assigned = await value(run.bridge, "desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project, pool: POOL }, generation: generation + 1 });
  assert.equal(assigned.outcome, "applied", `the project gets the golden pool: ${JSON.stringify(assigned)}`);
  const stored = await value(run.bridge, "desktop_project_setup", { id: project });
  const team = { ...stored.team, sub_harnesses: [{ harness: "codex", agents: 1, tools: true }] };
  const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team, documents: [] };
  const saved = await value(run.bridge, "desktop_update_project_setup", { id: project, setup, name: null, revision: stored.revision, generation: generation + 2 });
  assert.equal(saved.outcome, "applied", `the project gets a Codex sub-agent: ${JSON.stringify(saved)}`);
  return project;
}

/** Runs steps on a fresh page of the served app with the owner's helpers, checking the page
 * raised no errors, and keeps a screenshot when a step fails. Inputs: the run, the label for
 * that screenshot and the steps. Output: none. */
export async function onPage(run, label, steps) {
  await withPage(async (page) => {
    const me = owner(page, run.evidence);
    try {
      await page.send("Page.enable");
      await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
      await steps(page, me);
      assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
    } catch (failure) {
      await me.capture(`${label}-failure`).catch(() => {});
      throw failure;
    }
  });
}

/** Opens a path of the served app in the page.
 * Inputs: the run, the page and the hash path.
 * Output: none. */
export async function open(run, page, path) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#${path}` });
}

/** Waits until the page's hash route passes `check`.
 * Inputs: the page, what is waited for and the check.
 * Output: the hash. */
export function hashUntil(page, what, check) {
  return waitUntil(`the page to reach ${what}`, () => page.evaluate("location.hash"), check);
}

/** Reads until `done` accepts what `read` gave, every POLL_MS, for at most `ms`: the one place a
 * golden flow sleeps (Q2). Inputs: what is awaited, for the failure, the read, the check and
 * the limit (at most five minutes). Output: the value the check accepted. */
export async function waitUntil(what, read, done, ms = WAIT_MS) {
  assert.ok(ms > 0 && ms <= WAIT_MAX_MS, `${what}: ${ms} ms`);
  const deadline = Date.now() + ms;
  for (;;) {
    const found = await read();
    if (done(found)) return found;
    assert.ok(Date.now() < deadline, `${what} never came in ${ms} ms: ${JSON.stringify(found)}`);
    await pause(POLL_MS);
  }
}

/** Where a flow's run record goes in the evidence folder.
 * Input: the run.
 * Output: the path. */
function recordPath(run) {
  return join(run.evidence, `${run.flow}-desktop.json`);
}

/** Writes the run record: the flow, its outcome, its whole time and each step's time, with
 * no path and no name.
 * Inputs: the run and "passed" or "failed". Output: none. */
function writeRecord(run, outcome) {
  assert.ok(outcome === "passed" || outcome === "failed");
  const record = { flow: run.flow, client: "desktop", outcome, total_ms: Date.now() - run.started, steps: run.steps };
  writeFileSync(recordPath(run), JSON.stringify(record, null, 1), { mode: 0o600 });
}
