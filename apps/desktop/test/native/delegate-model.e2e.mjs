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
import { AGENTS_VIEW, INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const AGENT_DEFAULT = "gpt-6-sol";
const ALLOWED = ["gpt-6.1-sol", AGENT_DEFAULT];
const PICKED = "gpt-6.1-sol";

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Fruit stand\n\nSells fruit.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the fruit stand"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes the stand-in Codex executable the engine is started with; it exits at once, so no
 * model ever runs and a handed task only waits. Input: the test root. Output: its path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** Sets one app-wide Codex model slot through the app's own command, naming the version the
 * engine holds. Inputs: the bridge, the slot and the value. */
async function setCodexSlot(bridge, slot, value_) {
  const page = await value(bridge, "desktop_model_choice", { harness: "codex", projectId: null, fresh: false });
  const version = page.models.rows.find((row) => row.slot === slot).version;
  await value(bridge, "desktop_set_model_choice", { harness: "codex", projectId: null, slot, value: value_, version });
  const held = (await value(bridge, "desktop_model_choice", { harness: "codex", projectId: null, fresh: false })).models.rows.find((row) => row.slot === slot).here;
  assert.deepEqual(held, value_, `the engine holds the app-wide Codex ${slot} slot`);
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project made from
 * the owner's repository, with one Codex sub-agent harness, the app-wide Codex agent default
 * gpt-6-sol and the allowed list gpt-6.1-sol and gpt-6-sol, serves the built app to a
 * browser page on the project's Agents tab, runs the steps and stops everything. Inputs: the
 * test context, the temporary folder prefix and the steps. Output: none. */
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
  const repo = join(root, "repos", "fruit-stand");
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
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings", "--codex-executable", standInHarness(root)],
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
    assert.equal((await value(bridge, "desktop_open_project", { id: project })).project_id, project, "the new project opens");
    const stored = await value(bridge, "desktop_project_setup", { id: project });
    const subs = [{ harness: "codex", agents: 2, tools: true }];
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team: { ...stored.team, sub_harnesses: subs }, documents: [] };
    const saved = await value(bridge, "desktop_update_project_setup", { id: project, setup, name: null, revision: stored.revision, generation: 4 });
    assert.equal(saved.outcome, "applied", `the team gets a Codex sub-agent harness: ${JSON.stringify(saved)}`);
    await setCodexSlot(bridge, "agent", AGENT_DEFAULT);
    await setCodexSlot(bridge, "allowed", ALLOWED);
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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
        await me.click("Team", AGENTS_VIEW);
        await me.unfold("Agents and tasks");
        await me.waitFor("the hand-off form", (text) => text.includes("Hand a task to a sub-agent"));
        await steps({ project, me, page, status });
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

/** Picks one option of a select as the owner does. Inputs: the page, the select's id and
 * the value. Output: true once picked, false while the option is not offered. */
function choose(page, id, value_) {
  return page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(id)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === ${JSON.stringify(value_)})) return false;
    select.focus();
    select.value = ${JSON.stringify(value_)};
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
}

/** Reads the hand-off form's Model choices. Inputs: the page and the project. Output: each
 * option's value and label. */
function modelChoices(page, project) {
  return page.evaluate(`[...(document.getElementById(${JSON.stringify(`tg-hand-model-${project}`)})?.options ?? [])].map((option) => [option.value, option.textContent])`);
}

/** Sends a task to Codex from the hand-off form with one Model choice, once the form offers
 * Codex's models, and returns the task the engine keeps. Inputs: what the steps get, the
 * task's words and the model value, "" for Default. Output: the engine's task. */
async function handOff({ project, me, page, status }, prompt, model) {
  assert.ok(await choose(page, `tg-hand-to-${project}`, "codex"), "no Codex to pick in the Send to list");
  const offered = [["", `Default (${AGENT_DEFAULT})`], [PICKED, PICKED]];
  for (const deadline = Date.now() + STEP_MS; JSON.stringify(await modelChoices(page, project)) !== JSON.stringify(offered); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `the Model choice never offered Codex's default and allowed models: ${JSON.stringify(await modelChoices(page, project))}`);
  }
  assert.ok(await choose(page, `tg-hand-model-${project}`, model), `no ${model || "Default"} to pick in the Model list`);
  await me.type(`[id="tg-hand-text-${project}"]`, prompt);
  await me.click("Hand off", "form.tg-hand button");
  await me.waitFor("the hand-off note", (text) => text.includes("Task handed off."));
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const task = (await status()).tasks.find((item) => item.prompt === prompt);
    if (task) return task;
    assert.ok(Date.now() < deadline, "the engine never kept the handed task");
  }
}

test("Hand off with Model on Default: the task runs the harness's agent default and pins no pick (DEC-150 M-8)", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-dm-default-", async (run) => {
    const task = await handOff(run, "Write notes.md with one line.", "");
    assert.equal(task.harness, "codex");
    assert.equal(task.pick ?? null, null, `no pick is pinned: ${JSON.stringify(task)}`);
    assert.ok(!(await run.page.evaluate("document.body.innerText")).includes("Picked by the owner."), "no task says the owner picked its model");
    await run.me.capture("delegate-model-default");
  }));

test("Hand off with a picked model: the engine pins the owner's model and the task row says so (DEC-150 M-8)", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-dm-picked-", async (run) => {
    const task = await handOff(run, "Write plan.md with one line.", PICKED);
    assert.deepEqual(task.pick, { model: PICKED, reason: "Picked by the owner." }, `the owner's pick is pinned: ${JSON.stringify(task)}`);
    await run.me.waitFor("the task row's pick line", (text) => text.includes(`Model: ${PICKED}. Why: Picked by the owner.`));
    await run.me.capture("delegate-model-picked");
  }));
