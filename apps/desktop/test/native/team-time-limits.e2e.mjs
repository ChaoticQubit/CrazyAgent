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

const { TEAM_SETTINGS_STALE } = await import("../../src/screens/team/controls.tsx");

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SETTING = "Each task may run";

/** Holds a window's team reads while `window.__e2eHeld` is set, so the window keeps the team
 * it read before the other window's change, as a window whose next read has not come yet does.
 * A held read goes to the bridge once released and returns the bridge's real answer.
 * `window.__e2eTeamReads` counts the reads on their way. */
const HOLD = `(() => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const waiting = [];
  window.__e2eHeld = false;
  window.__e2eTeamReads = 0;
  window.__e2eRelease = () => {
    window.__e2eHeld = false;
    for (const go of waiting.splice(0)) go();
  };
  window.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
    if (cmd !== "desktop_team_status") return invoke(cmd, args);
    if (window.__e2eHeld) await new Promise((go) => waiting.push(go));
    window.__e2eTeamReads += 1;
    try {
      return await invoke(cmd, args);
    } finally {
      window.__e2eTeamReads -= 1;
    }
  };
})();`;

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
 * serves the built app to two browser pages, as two app windows on one engine, opens the
 * Agents tab in both, runs the steps and stops everything. The second window can hold its team
 * reads. Inputs: the test context, the temporary folder prefix and the steps, given the
 * project, both windows and the team status reader. Output: none. */
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
    const open = async (page, name, scripts) => {
      mkdirSync(join(evidence, name), { recursive: true, mode: 0o700 });
      const me = owner(page, join(evidence, name));
      await page.send("Page.enable");
      for (const source of scripts) await page.send("Page.addScriptToEvaluateOnNewDocument", { source });
      await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
      await me.waitFor(`the Time limits on the Agents tab of the ${name} window`, (text) => text.includes(SETTING));
      return { me, page };
    };
    await withPage(async (firstPage) => withPage(async (secondPage) => {
      let first;
      let second;
      try {
        first = await open(firstPage, "first", [INTERNALS]);
        second = await open(secondPage, "second", [INTERNALS, HOLD]);
        await steps({ project, first, second, status });
        for (const side of [first, second]) assert.deepEqual(await side.page.evaluate("window.__e2eErrors"), [], "the windows raised no errors");
      } catch (failure) {
        for (const side of [first, second]) await side?.me.capture("failure").catch(() => {});
        throw failure;
      }
    }));
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the windows sent reached the bridge");
    t.diagnostic(`native-only commands the windows called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the windows sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Picks one choice of a time limit select, as the owner's pick in its list does: the select
 * takes the value and tells the page it changed. Inputs: the page, the select's id and the
 * choice's value. Output: none. */
async function choose(page, id, choice) {
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(id)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === ${JSON.stringify(choice)} && !option.disabled)) return false;
    select.focus();
    select.value = ${JSON.stringify(choice)};
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, `no ${choice} to pick in ${id}`);
}

/** Waits until the engine's team status holds a task and a session limit. Inputs: the status
 * reader and both limits in seconds, null for Off. Output: none. */
async function kept(status, limits) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const team = await status();
    const held = [team.turn_limit_secs ?? null, team.session_limit_secs ?? null];
    if (JSON.stringify(held) === JSON.stringify(limits)) return;
    assert.ok(Date.now() < deadline, `the engine never kept ${JSON.stringify(limits)}; it holds ${JSON.stringify(held)}`);
  }
}

/** Waits until a page's script expression is true. Inputs: the page, the expression and what
 * it waits for. Output: none. */
async function until(page, expression, what) {
  for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(expression)); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `never: ${what}`);
  }
}

test("Time limits: a change from a window's old read is refused, the other window's change stays, and the change applies once the window reads again", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-ttr-", async ({ project, first, second, status }) => {
    const turn = `tg-turn-limit-${project}`;
    const session = `tg-session-limit-${project}`;
    await kept(status, [null, null]);
    await second.page.evaluate("window.__e2eHeld = true");
    await until(second.page, "window.__e2eTeamReads === 0", "the second window has no team read on its way");

    await choose(first.page, turn, "3600");
    await kept(status, [3600, null]);

    await choose(second.page, session, "14400");
    await second.me.waitFor("the refusal of the second window's change from its old read", (text) => text.includes(TEAM_SETTINGS_STALE));
    await second.me.capture("stale-refused");
    await kept(status, [3600, null]);

    await second.page.evaluate("window.__e2eRelease()");
    await until(second.page, `document.getElementById(${JSON.stringify(turn)})?.value === "3600"`, "the second window shows the first window's task limit");
    await choose(second.page, session, "14400");
    await kept(status, [3600, 14400]);
    await until(second.page, `document.getElementById(${JSON.stringify(session)})?.value === "14400"`, "the second window shows its kept session limit");
    await second.me.capture("fresh-applied");
  });
});
