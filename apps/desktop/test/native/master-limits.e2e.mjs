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
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";
import "../load-source.mjs";

const { RUN_DEFAULT_SECS, roundOptions, runOptions } = await import("../../src/screens/team/controls.tsx");

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const ROUNDS_SETTING = "Sub-agent rounds per message";
const RUN_SETTING = "Master run time";

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Night jobs\n\nRuns the night jobs.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the night jobs"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * serves the built app to one browser page, as the app window, opens the Agents tab's limits,
 * runs the steps and stops everything. Inputs: the test context, the temporary folder prefix
 * and the steps, given the project, the window, the team status reader and a reload of the
 * window. Output: none. */
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
  const repo = join(root, "repos", "night-jobs");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  ownerRepo(repo);
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
  try {
    ({ child: engine } = await launch(engineBinary,
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
      { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool: "local-pool", generation: 3 });
    assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
    const project = receipt.project_id;
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: project });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    const status = () => value(bridge, "desktop_team_status", { id: project });

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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=limits` });
        const shown = (text) => text.includes(ROUNDS_SETTING) && text.includes(RUN_SETTING);
        await me.waitFor("the master limits in Agent settings", shown);
        const reload = async () => {
          await page.send("Page.reload", { ignoreCache: true });
          await me.waitFor("the master limits in Agent settings after a reload", shown);
        };
        await steps({ project, side: { me, page }, status, reload });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the window raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the window sent reached the bridge");
    t.diagnostic(`native-only commands the window called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the window sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Waits until a page's script expression is true. Inputs: the page, the expression and what
 * it waits for. Output: none. */
async function until(page, expression, what) {
  for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(expression)); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `never: ${what}`);
  }
}

/** Picks one choice of a master limit select as the owner's pick in its list does, waits until
 * the engine keeps it in the team status and the select shows it again, ready for the next
 * pick. Inputs: the window, the select's id, the choice's value, the status reader and the
 * status field. Output: none. */
async function pick(side, id, choice, status, field) {
  const ready = `(() => { const select = document.getElementById(${JSON.stringify(id)}); return !!select && !select.disabled; })()`;
  await until(side.page, ready, `${id} takes a pick`);
  const picked = await side.page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(id)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === ${JSON.stringify(choice)} && !option.disabled)) return false;
    select.focus();
    select.value = ${JSON.stringify(choice)};
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, `no ${choice} to pick in ${id}`);
  const want = choice === "off" ? null : Number(choice);
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status())[field] ?? null;
    if (held === want) break;
    assert.ok(Date.now() < deadline, `the engine never kept ${field} ${JSON.stringify(want)}; it holds ${JSON.stringify(held)}`);
  }
  await until(side.page, `${ready} && document.getElementById(${JSON.stringify(id)}).value === ${JSON.stringify(choice)}`, `${id} shows ${choice}`);
}

/** Runs one row per choice of a master limit select: from another choice the owner picks it,
 * the engine keeps it, and the window shows it again after a reload, which reads the team
 * status afresh. Inputs: the test context, the steps' arguments, the select's id, its
 * options, the status field and the evidence name. Output: none. */
async function everyChoice(t, { side, status, reload }, id, options, field, name) {
  for (const option of options) {
    await t.test(`${name}: ${option.label} is saved and read back`, async () => {
      const other = options.find((each) => each.value !== option.value);
      await pick(side, id, other.value, status, field);
      await pick(side, id, option.value, status, field);
      await reload();
      await until(side.page, `document.getElementById(${JSON.stringify(id)})?.value === ${JSON.stringify(option.value)}`, `the reloaded window shows ${option.label}`);
      await side.me.capture(`${field}-${option.value}`);
    });
  }
}

test("Master limits: every Sub-agent rounds and Master run time choice is saved by the owner's pick and read back", { timeout: 900000 }, async (t) => {
  await onEngine(t, "ca-mlim-", async (run) => {
    const { project, status } = run;
    const unset = await status();
    assert.equal(unset.master_rounds ?? null, null, "Until the goal is done until the owner picks");
    assert.equal(unset.master_run_secs, RUN_DEFAULT_SECS, "7 days until the owner picks");
    const rounds = roundOptions();
    const runs = runOptions();
    assert.equal(rounds.length, 7, "Until the goal is done and six numbers");
    assert.equal(runs.length, 6, "five durations and No limit");
    await everyChoice(t, run, `tg-master-rounds-${project}`, rounds, "master_rounds", ROUNDS_SETTING);
    await everyChoice(t, run, `tg-master-run-${project}`, runs, "master_run_secs", RUN_SETTING);
  });
});
