import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const WAIT_MS = 90000;
const WAIT_MAX_MS = 300000;
const STEPS_MAX = 64;
export const POOL = "local-pool";

/** Starts one golden flow as a first run leaves the app: a real engine on a private root with a
 * scratch HOME and one local pool, the bridge, the owner's profile saved, a projects folder
 * picked and the built app served, and counts that as the flow's first step. It fills the
 * caller's empty run as it goes, so `stopGolden` ends whatever a failed start left running.
 * Inputs: that run and the flow's name (lowercase words joined by "-"). Output: none. */
export async function startGolden(run, flow) {
  assert.match(flow, /^[a-z]+(-[a-z]+)*$/);
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
  const home = join(run.root, "home");
  run.projects = join(run.root, "projects");
  for (const folder of [runtime, run.evidence, home, join(home, ".codex"), run.projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  rmSync(recordPath(run), { force: true });
  const provider = await closedPort();
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
  const args = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"];
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

/** Stops the app, the bridge and the engine; an unfinished flow's record says failed at the
 * steps it reached. Keeps the root only when E2E_EVIDENCE names the evidence folder. Input: the
 * run, as full as its start got. Output: none. */
export async function stopGolden(run) {
  if (run.log) run.log.closing = true;
  if (run.evidence && !run.finished && run.steps.length > 0) writeRecord(run, "failed");
  if (run.evidence && run.log) writeFileSync(join(run.evidence, "calls.json"), JSON.stringify(run.log.calls, null, 1));
  run.site?.server.close();
  await stop(run.bridgeProcess);
  await stop(run.engine);
  if (run.root && !process.env.E2E_EVIDENCE) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
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
