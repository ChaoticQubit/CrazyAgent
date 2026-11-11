import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SHOWN = `document.querySelector('input[type=radio][name="step-limit"]:checked')?.value ?? null`;
const SAVE = 'form[aria-label="Master"] button[type=submit]';
const RESTING = `[${SHOWN}, document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null]`;
const TOP = "How far a project's master goes in one turn.";
const CARDS = {
  no_limit: "No limit A turn runs until the master replies or its time is up.",
  steps50: "50 steps A turn stops after 50 steps.",
  steps100: "100 steps A turn stops after 100 steps.",
  steps200: "200 steps A turn stops after 200 steps.",
};

/** Runs the owner's git in a folder with no global or system config. Inputs: the folder and
 * the arguments. Output: what git printed, trimmed. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
async function until(what, wanted, read) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * serves the built app to a browser page, runs the steps and stops everything, ending any
 * terminal server left in this test's root. Inputs: the test context, the temporary folder
 * prefix and the steps, given the bridge, the owner on the page, a route opener, a page
 * reload, the engine root and an engine restart that runs `between` while no engine runs.
 * Output: none. */
async function onEngine(t, prefix, steps) {
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
  writeFileSync(join(repo, "README.md"), "# Tool shop\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the tool shop"]]) git(repo, ...args);
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
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool: "local-pool", generation: 3 });
    assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: receipt.project_id });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    let starts = 0;
    const restart = async (between = () => {}) => {
      starts += 1;
      await stop(bridgeProcess);
      await stop(engine);
      await between();
      ({ child: engine } = await launch(engineBinary, engineArgs, { ...env }, evidence, `engine-${starts}`, /^ready$/m));
      const again = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, `bridge-${starts}`, /^ready (\d+) ([0-9a-f]{64})$/m);
      bridgeProcess = again.child;
      bridge.port = Number(again.match[1]);
      bridge.token = again.match[2];
      assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the engine started again");
    };

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
        await steps({ bridge, me, page, open, reload, runtime, restart });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    t.diagnostic(`native-only commands the page called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
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

/** Opens Settings, Master and waits for the four step limit options. Inputs: the owner and
 * the route opener. Output: none. */
async function master(me, open) {
  await open("#/settings/master", "the Master setting", (text) => text.includes(TOP));
  await me.waitFor("the four options", (text) => ["Master step limit", "No limit", "50 steps", "100 steps", "200 steps", "reply with 'continue' to go on"].every((words) => text.includes(words)));
}

/** The stored step limit as the engine reads it. Input: the bridge. Output: the limit. */
async function stored(bridge) {
  return (await value(bridge, "desktop_master_step_settings")).limit;
}

/** Picks one step limit in Settings, Master and saves it, waiting until the engine stores it
 * and the page shows the stored value with Save resting. Inputs: the bridge, the owner, the
 * page and the limit. Output: none. */
async function save({ bridge, me, page }, limit) {
  await me.click(CARDS[limit], "label.radio-card");
  await until(`${limit} picked on the page`, [limit, false], () => page.evaluate(RESTING));
  await me.click("Save changes", SAVE);
  await until(`${limit} in the engine`, limit, () => stored(bridge));
  await me.waitFor("the saved note", (text) => text.includes("Master step limit saved."));
  await until(`${limit} stored on the page`, [limit, true], () => page.evaluate(RESTING));
}

/** Saves one step limit as `save` does, then checks that it stays after a page reload and an
 * engine restart. Inputs: the steps' tools and the limit. Output: none. */
async function picked(tools, limit) {
  const { bridge, me, page, open, reload, restart } = tools;
  await save(tools, limit);
  await reload(TOP);
  await until(`${limit} after a reload`, limit, () => page.evaluate(SHOWN));
  await restart();
  await until(`${limit} after an engine restart`, limit, () => stored(bridge));
  await master(me, open);
  await until(`${limit} on the page after the restart`, [limit, true], () => page.evaluate(RESTING));
}

test("Master step limit: No limit is the default, and picking it again after 50 steps is saved and stays after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-steps-none-", async (tools) => {
  const { bridge, me, page, open } = tools;
  await master(me, open);
  await until("the default in the engine", "no_limit", () => stored(bridge));
  await until("the default on the page, with Save waiting for a change", ["no_limit", true], () => page.evaluate(RESTING));
  await save(tools, "steps50");
  await picked(tools, "no_limit");
  await me.capture("master-step-limit-none");
}));

for (const limit of ["steps50", "steps100", "steps200"]) {
  test(`Master step limit: ${CARDS[limit].split(" A turn")[0]} is saved and stays after a reload and an engine restart`, { timeout: 600000 }, (t) => onEngine(t, `ca-${limit}-`, async (tools) => {
    await master(tools.me, tools.open);
    await picked(tools, limit);
    await tools.me.capture(`master-step-limit-${limit}`);
  }));
}
