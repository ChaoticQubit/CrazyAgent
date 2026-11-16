import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];

export const BAR = "header.top-bar";
export const CONTROL = "button.keep-awake-control";
export const BELL = "button.notice-bell";
export const PANEL = 'section[role=dialog][aria-label="Keep awake"]';
export const FORM = 'form[aria-label="Keep awake"]';
export const SETTINGS_TITLE = "When the computer stays awake for your agents, and when that ends.";

/** Runs the owner's git in a folder with no global or system config. Inputs: the folder and
 * the arguments. Output: what git printed, trimmed. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
export async function until(what, wanted, read) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Makes the test engine's project from a one-file repository and waits for its
 * onboarding. Inputs: the bridge, the repository folder and the pool. Output: the project id. */
async function project(bridge, repo) {
  writeFileSync(join(repo, "README.md"), "# Tool shop\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the tool shop"]]) git(repo, ...args);
  const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool: "local-pool", generation: 3 });
  assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
  for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
    const view = await value(bridge, "desktop_onboarding", { id: receipt.project_id });
    if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
    assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
  }
  return receipt.project_id;
}

/** Starts the real engine and the bridge, with a project when asked, serves the built app to
 * a browser page, runs the steps and stops everything. The engine is this test's own, in a
 * temporary folder; a hold it takes ends with it. Inputs: the test context, the temporary
 * folder prefix, the steps (given the bridge, the owner on the page, the page, a route opener,
 * a page reload and the project id or null) and whether a project is made. Output: none. */
export async function onEngine(t, prefix, steps, { withProject = false } = {}) {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), prefix)));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? join(process.env.E2E_EVIDENCE, prefix) : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "tool-shop");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const provider = await closedPort();
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let engine;
  let bridgeProcess;
  let site;
  const engineArgs = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"];
  try {
    ({ child: engine } = await launch(engineBinary, engineArgs, { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2], generation: 10 };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const projectId = withProject ? await project(bridge, repo) : null;
    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);
    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        let visits = 0;
        const clean = async () => assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
        const open = async (hash, what, check) => {
          await clean();
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}${hash}` });
          await me.waitFor(what, check);
        };
        const reload = async (words) => {
          await clean();
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(words)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, `the reloaded page never showed ${words}`);
          }
        };
        await steps({ t, bridge, me, page, open, reload, projectId });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the page sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
    await endTerminals(runtime);
  }
  assert.deepEqual(terminalServers(runtime), [], "no tmux server outlives the test's engine");
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Reads Keep awake through the owner protocol, as the engine last evaluated it. Input: the
 * bridge. Output: the view. */
export async function engineView(bridge) {
  const view = await value(bridge, "desktop_keep_awake_read");
  assert.ok(view.settings && view.state, `the engine gives a Keep awake view: ${JSON.stringify(view)}`);
  return view;
}

/** Sends one Keep awake change through the bridge with a fresh generation, for a row's set
 * up and clean up; the rows' own changes go through the window. Inputs: the bridge and the
 * change. Output: none. */
export async function engineAct(bridge, action) {
  bridge.generation += 1;
  const receipt = await value(bridge, "desktop_keep_awake_act", { action, generation: bridge.generation });
  assert.equal(receipt.outcome, "applied", `the engine takes ${JSON.stringify(action)}: ${JSON.stringify(receipt)}`);
}

/** Sends one Keep awake change through the bridge with a fresh generation and gives back
 * the engine's receipt, applied or not, for a row that proves a refusal. Inputs: the bridge
 * and the change. Output: the receipt. */
export async function engineTry(bridge, action) {
  bridge.generation += 1;
  return value(bridge, "desktop_keep_awake_act", { action, generation: bridge.generation });
}

/** The words on the Keep awake control. Input: the page. Output: the label, or null when no
 * control is shown. */
export async function controlLabel(page) {
  return page.evaluate(`document.querySelector(${JSON.stringify(CONTROL)})?.innerText.trim() ?? null`);
}

/** Opens the Keep awake panel when it is closed and waits for it. Inputs: the owner and the
 * page. Output: none. */
export async function openPanel(me, page) {
  const open = await page.evaluate(`document.querySelector(${JSON.stringify(PANEL)}) !== null`);
  if (!open) await me.follow(CONTROL);
  await until("the Keep awake panel", true, () => page.evaluate(`document.querySelector(${JSON.stringify(PANEL)}) !== null`));
  await until("the panel loaded", true, () => page.evaluate(`document.querySelector(${JSON.stringify(PANEL)})?.innerText.includes("Ends by itself") === true`));
}

/** The words in the open Keep awake panel on one line. Input: the page. Output: the text. */
export async function panelText(page) {
  return page.evaluate(`(document.querySelector(${JSON.stringify(PANEL)})?.innerText ?? "").replace(/\\s+/g, " ").trim()`);
}

/** Minutes until the engine's hold ends, rounded to the nearest. Input: the view. Output:
 * the minutes, or null when the hold has no end. */
export function minutesLeft(view) {
  return view.state.ends_at_ms === null ? null : Math.round((view.state.ends_at_ms - Date.now()) / 60000);
}
