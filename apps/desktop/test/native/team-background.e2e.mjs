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
import { INTERNALS, POLL_MS, STEP_MS, bridgeCall, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SETTING = "Background commands at task end";
const REFUSED = "Pick a grace time from 10 to 3600 seconds.";

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
 * serves the built app to a browser page, runs the steps and stops everything. Inputs: the
 * test context, the temporary folder prefix and the steps, given the project, the bridge,
 * the owner on the page, the page, a tab opener, a page reload and the team status reader.
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
        const agents = async () => {
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=limits` });
          await me.waitFor("the Background commands setting in Agent settings", (text) => text.includes(SETTING));
        };
        const reload = async () => {
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(SETTING)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, "the reloaded page never showed the Background commands setting");
          }
        };
        await agents();
        await steps({ project, bridge, me, page, reload, status });
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
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Picks one choice of the Background commands select, as the owner's pick in its list
 * does: the select takes the value and tells the page it changed. Inputs: the page, the
 * project and the choice's value. Output: none. */
async function choose(page, project, choice) {
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(`tg-background-${project}`)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === ${JSON.stringify(choice)} && !option.disabled)) return false;
    select.focus();
    select.value = ${JSON.stringify(choice)};
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, `no ${choice} to pick in the Background commands list`);
}

/** Waits until the engine's team status holds a Background commands choice. Inputs: the
 * status reader and the choice. Output: none. */
async function kept(status, choice) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).background_at_end;
    if (JSON.stringify(held) === JSON.stringify(choice)) return;
    assert.ok(Date.now() < deadline, `the engine never kept ${JSON.stringify(choice)}; it holds ${JSON.stringify(held)}`);
  }
}

/** Reads what the Agents tab shows for the setting: the select's value and the grace time
 * field's text, null without one. Inputs: the page and the project. Output: both. */
function shown(page, project) {
  return page.evaluate(`(() => ({
    choice: document.getElementById(${JSON.stringify(`tg-background-${project}`)})?.value ?? null,
    grace: document.getElementById(${JSON.stringify(`tg-grace-${project}`)})?.value ?? null,
  }))()`);
}

test("Background commands: the owner's pick of Wait is kept by the engine and shown again after a reload", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-bg-w-", async ({ project, me, page, reload, status }) => {
    assert.deepEqual((await status()).background_at_end, { mode: "grace", secs: 1800 }, "a 30 minute grace time is the default");
    assert.deepEqual(await shown(page, project), { choice: "grace", grace: "1800" }, "the tab shows the default");
    await choose(page, project, "stop");
    await kept(status, { mode: "stop" });
    await reload();
    await choose(page, project, "wait");
    await kept(status, { mode: "wait" });
    await reload();
    assert.deepEqual(await shown(page, project), { choice: "wait", grace: null }, "Wait is shown again with no grace time to type");
    await me.capture("background-wait");
  });
});

test("Background commands: a grace time the owner types and saves is kept and shown after a reload, and a refused time changes nothing", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-bg-g-", async ({ project, bridge, me, page, reload, status }) => {
    await choose(page, project, "wait");
    await kept(status, { mode: "wait" });
    await choose(page, project, "grace");
    await kept(status, { mode: "grace", secs: 1800 });
    await reload();
    assert.deepEqual(await shown(page, project), { choice: "grace", grace: "1800" }, "the offered 30 minute grace time is kept");
    await me.type(`[id="tg-grace-${project}"]`, "45");
    await me.click("Save", 'form[aria-label="Grace time"] button');
    await kept(status, { mode: "grace", secs: 45 });
    await reload();
    assert.deepEqual(await shown(page, project), { choice: "grace", grace: "45" }, "the typed grace time is shown again");
    await me.capture("background-grace");

    await me.type(`[id="tg-grace-${project}"]`, "5");
    await me.click("Save", 'form[aria-label="Grace time"] button');
    await me.waitFor("the plain refusal for a grace time under 10 seconds", (text) => text.includes(REFUSED));
    await me.capture("background-grace-refused");
    assert.deepEqual((await status()).background_at_end, { mode: "grace", secs: 45 }, "a refused time changes nothing");
    const asked = await bridgeCall(bridge, "desktop_team_background", { id: project, choice: { mode: "grace", secs: 5 }, generation: 900 });
    assert.deepEqual(asked.failure, { code: "team_action_invalid", cause: "team_background_grace_invalid" }, `the engine refuses 5 seconds itself: ${JSON.stringify(asked)}`);
    assert.deepEqual((await status()).background_at_end, { mode: "grace", secs: 45 }, "the engine's refusal changes nothing");
    await reload();
    assert.deepEqual(await shown(page, project), { choice: "grace", grace: "45" }, "the kept grace time is shown after the refusal");
  });
});

test("Background commands: the owner's pick of Stop is kept by the engine and shown again after a reload", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-bg-s-", async ({ project, me, page, reload, status }) => {
    await choose(page, project, "stop");
    await kept(status, { mode: "stop" });
    await reload();
    assert.deepEqual(await shown(page, project), { choice: "stop", grace: null }, "Stop is shown again with no grace time to type");
    await me.capture("background-stop");
  });
});
