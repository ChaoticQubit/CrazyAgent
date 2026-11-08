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
import { INTERNALS, POLL_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TAB_PRESSES_MAX = 400;
const GROUPS = [
  ["Model and harness", ["Master harness", "Sub-agent harnesses", "Keep sub-agents ready", "On master harness switch", "Spread harnesses across accounts", "Max harnesses per project", "Max teammates per harness"]],
  ["Access and merging", ["Agent access", "Under Full access, merging into main", "Ask me before merging into the master's branch", "Agent branches", "After you accept, the agent's branch restarts from main", "Master's branch", "Land on", "Sub-agent work lands on its lead's branch", "Refuse a change set when one new folder adds more than"]],
  ["Skills and tools", ["Skills per role", "MCP servers", "Workflow nudges", "Task-start reminder", "Counted tools", "Re-ask a sub-agent's empty reply"]],
  ["Coordination", ["Claude Code sub-agents", "Codex sub-agents", "rcli sub-agents", "Sub-agent depth", "Sub-agent questions go to", "Overlapping edits", "Terminals per window", "Catch up with main", "Finished agents", "Messages to Claude Code agents", "Claude Code terminal display", "Messages to Codex agents", "Messages to rcli agents"]],
  ["Limits", ["Each task may run", "Each agent session may run", "Stop an idle sub-agent after", "Background commands at task end", "Token budget"]],
];
const READ_ONLY = ["MCP servers", "After you accept, the agent's branch restarts from main"];
const MEASURE = `(() => {
  const groups = [...document.querySelectorAll("section.set-group")];
  const past = groups.flatMap((group) => {
    const edge = group.getBoundingClientRect();
    return [...group.querySelectorAll("*")].filter((part) => {
      const box = part.getBoundingClientRect();
      return box.width > 1 && (box.right > edge.right + 0.5 || box.left < edge.left - 0.5);
    }).map((part) => part.closest(".set-row")?.dataset.setting ?? part.getAttribute("class") ?? part.tagName);
  });
  const above = groups.flatMap((group) => {
    const heading = group.querySelector("h2").getBoundingClientRect();
    return [...group.querySelectorAll(".set-row")].filter((row) => row.getBoundingClientRect().top < heading.bottom).map((row) => row.dataset.setting);
  });
  const main = document.querySelector(".main");
  return {
    groups: groups.map((group) => [group.querySelector("h2")?.textContent ?? null, [...group.querySelectorAll(".set-row")].map((row) => row.dataset.setting)]),
    past: [...new Set(past)],
    above,
    sideways: Math.max(main ? main.scrollWidth - main.clientWidth : 0, document.documentElement.scrollWidth - document.documentElement.clientWidth),
  };
})()`;

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Seed shop\n\nSells seeds.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the seed shop"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes the stand-in Codex executable the engine is started with; it exits at once, so no
 * model ever runs. Input: the test root. Output: its absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * at Full access with one Codex sub-agent harness, so every row with a control draws it, serves
 * the built app to a browser page, runs the steps and stops everything. Inputs: the
 * test context, the temporary folder prefix and the steps, given the project, the owner on
 * the page, the page and the site's port. Output: none. */
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
  const repo = join(root, "repos", "seed-shop");
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
    const policy = await value(bridge, "desktop_project_agent_policy", { id: project });
    const full = await value(bridge, "desktop_set_project_agent_policy", {
      id: project, version: policy.version, generation: 4,
      policy: { access: "full", network: policy.network, toolchains: [], ide_git_hooks: policy.ide_git_hooks },
    });
    assert.equal(full.outcome, "applied", `the project runs at Full access, so the merge row shows its choices: ${JSON.stringify(full)}`);
    const pools = await value(bridge, "desktop_model_access_page", { collection: "pools", next: false });
    const assigned = await value(bridge, "desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project, pool: "local-pool" }, generation: 5 });
    assert.equal(assigned.outcome, "applied", `the project gets the pool its sub-agents run on: ${JSON.stringify(assigned)}`);
    const stored = await value(bridge, "desktop_project_setup", { id: project });
    const team = { ...stored.team, sub_harnesses: [{ harness: "codex", agents: 2, tools: true }] };
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team, documents: [] };
    const saved = await value(bridge, "desktop_update_project_setup", { id: project, setup, name: null, revision: stored.revision, generation: 6 });
    assert.equal(saved.outcome, "applied", `the team has a sub-agent harness, so its row shows its steppers: ${JSON.stringify(saved)}`);

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);
    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await steps({ project, me, page, port: site.port });
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

/** Presses Tab from the top of the page until every setting row had the focus or the presses
 * run out, as a keyboard owner does. Input: the page. Output: the rows the focus reached, in
 * order, once each. */
async function tabThrough(page) {
  const wanted = GROUPS.flatMap(([, rows]) => rows).filter((row) => !READ_ONLY.includes(row));
  await page.evaluate("document.activeElement?.blur(), true");
  const reached = [];
  for (let press = 0; press < TAB_PRESSES_MAX && !wanted.every((row) => reached.includes(row)); press += 1) {
    for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 });
    const row = await page.evaluate("document.activeElement?.closest('.set-row')?.dataset.setting ?? null");
    if (row !== null && !reached.includes(row)) reached.push(row);
  }
  return reached;
}

for (const width of [1024, 1440]) {
  test(`Agent settings at a ${width} px window: every setting sits under its group heading, Tab reaches each one and nothing scrolls sideways`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-set-${width}-`, async ({ project, me, page, port }) => {
      await page.send("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.navigate", { url: `http://127.0.0.1:${port}/#/projects/${encodeURIComponent(project)}/agents?section=settings` });
      await me.waitFor("every setting group in Agent settings", (text) => GROUPS.every(([title]) => text.includes(title)) && text.includes("Token budget"));
      const measured = await page.evaluate(MEASURE);
      assert.deepEqual(measured.groups, GROUPS, "each setting is drawn in its own group's panel, in the approved order");
      assert.deepEqual(measured.above, [], "no setting sits above its group's heading");
      assert.deepEqual(measured.past, [], "nothing runs past a group's panel");
      assert.ok(measured.sideways <= 0, `the page does not scroll sideways, ${measured.sideways} px`);
      await me.capture(`agent-settings-${width}`);
      const reached = await tabThrough(page);
      assert.deepEqual(GROUPS.flatMap(([, rows]) => rows).filter((row) => !READ_ONLY.includes(row) && !reached.includes(row)), [], "Tab reaches every setting with a control");
      await page.send("Page.navigate", { url: `http://127.0.0.1:${port}/#/projects/${encodeURIComponent(project)}/agents?section=limits` });
      await me.waitFor("Agent settings opened at Limits", (text) => text.includes("Token budget"));
      for (const deadline = Date.now() + 5000; (await page.evaluate("document.activeElement?.textContent ?? null")) !== "Limits"; await pause(POLL_MS)) {
        assert.ok(Date.now() < deadline, "a route naming a group puts the focus on that group's heading");
      }
      await me.capture(`agent-settings-${width}-limits`);
    }));
}
