import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const TASK_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TASK = "Write fruit.md with one line about apples.";
const SKILL = "meal-tips";
const REMINDER = `Task start reminder: before you work on this task, run every command, test, search or file read whose output is not short and fixed through the \`ctx_batch_execute\`, \`ctx_execute\`, \`ctx_execute_file\`, \`ctx_fetch_and_index\`, \`ctx_search\` tools of the \`plugin_context-mode_context-mode\` MCP server (the shell only for short fixed output, state changes and edits), query the project graph with \`graphify query "<question>"\` before you read or search source, and check these skills and read and follow the SKILL.md of each one that applies: \`${SKILL}\`.`;
const OPTIONS = '[role=radiogroup][aria-label="Task-start reminder"] [role=radio]';

/** The Codex stand-in the engine runs every team agent with: it keeps a conversation file as
 * Codex does, answers each turn with "Wrote fruit.md." and writes every pasted line to `typed`
 * in its home, so the test sees the turn the engine typed into it. */
const TYPED_CODEX = String.raw`#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/11/05"
/bin/mkdir -p "$d" || exit 1
id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
f="$d/rollout-2026-11-05T00-00-00-$id.jsonl"
printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0; printf '%s\n' "$line" >> "$CODEX_HOME/typed" ;;
    *"$e[200~"*) open=1; printf '%s\n' "$line" >> "$CODEX_HOME/typed"; continue ;;
    *) [ "$open" = 1 ] && { printf '%s\n' "$line" >> "$CODEX_HOME/typed"; continue; } ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  /bin/sleep 0.3
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Wrote fruit.md."}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
`;

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Fruit stand\n\nSells fruit.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the fruit stand"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes the stand-in Codex the engine is started with. Input: the test root. Output: its
 * absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, TYPED_CODEX, { mode: 0o700 });
  return path;
}

/** Writes a skill folder with its SKILL.md, the shape the owner's Add skill takes. Inputs: the
 * parent folder and the skill name. Output: the skill folder. */
function skillFolder(parent, name) {
  const folder = join(parent, name);
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "SKILL.md"), `---\nname: ${name}\ndescription: Teaches ${name}.\n---\n\nSteps for ${name}.\n`);
  return folder;
}

/** Reads the engine's rows for the owner's task: its status, its agent and that agent's tools
 * switch, and what the engine typed into the agent's stand-in. Inputs: the engine root and the
 * project. Output: those, or null before the task exists. */
function engineRows(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const task = store.prepare("SELECT status, agent FROM team_tasks WHERE project=? AND prompt=?").get(project, TASK);
    if (!task) return null;
    const agent = task.agent === null ? null : store.prepare("SELECT tools FROM team_agents WHERE id=?").get(task.agent) ?? null;
    const typed = join(runtime, "team", "agents", `agent-${task.agent}`, "home", "typed");
    return {
      task: task.status,
      agent: task.agent,
      tools: agent === null ? null : agent.tools === 1,
      typed: existsSync(typed) ? readFileSync(typed, "utf8") : "",
    };
  } finally {
    store.close();
  }
}

/** Reads the Task-start reminder option the engine keeps for a project. Inputs: the engine root
 * and the project. Output: the stored option, or null while no coordination row is saved. */
function storedTaskStart(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const row = store.prepare("SELECT json FROM coord_settings WHERE project=?").get(project);
    return row ? JSON.parse(row.json).task_start ?? null : null;
  } finally {
    store.close();
  }
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project made from
 * the owner's repository, on a keyless local account, approves one skill and an MCP server
 * named context-mode as the owner does, serves the built app to a browser page on the
 * project's Agents tab, runs the steps and stops everything. Inputs: the test context, the
 * temporary folder prefix and the steps, given the project, the engine root, the owner on the
 * page and the page. Output: none. */
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
    const pools = await value(bridge, "desktop_model_access_page", { collection: "pools", next: false });
    assert.equal(pools.writable, true, "the test engine's settings are writable");
    const assigned = await value(bridge, "desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project, pool: "local-pool" }, generation: 4 });
    assert.equal(assigned.outcome, "applied", `the project gets the pool its sub-agents run on: ${JSON.stringify(assigned)}`);
    const added = await value(bridge, "desktop_add_skill", { scope: { kind: "global" }, folder: skillFolder(join(root, "picked"), SKILL) });
    assert.equal(added.name, SKILL, `the engine approves ${SKILL}: ${JSON.stringify(added)}`);
    const spec = { kind: "stdio", command: "/usr/bin/true", args: [] };
    const server = await value(bridge, "desktop_add_mcp_server", { scope: { kind: "global" }, name: "context-mode", spec, secrets: [] });
    assert.equal((await value(bridge, "desktop_approve_mcp_server", { id: server.id, spec })).status, "approved", "the owner approves the context-mode server");
    assert.equal(storedTaskStart(runtime, project), null, "a new project keeps no coordination row");

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
        await me.waitFor("the Team graph and the Task-start reminder setting on the Agents tab", (text) => text.includes("Team graph") && text.includes("Task-start reminder"));
        await steps({ project, runtime, me, page });
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

/** Reads which Task-start reminder option the Agents tab shows checked. Input: the page.
 * Output: each option's label and whether it is checked. */
function shownOptions(page) {
  return page.evaluate(`[...document.querySelectorAll(${JSON.stringify(OPTIONS)})].map((radio) => [radio.innerText.trim(), radio.getAttribute("aria-checked")])`);
}

/** Waits until the Agents tab shows the wanted Task-start reminder option checked. Inputs: the
 * owner on the page, the page and the wanted labels with their checked state. Output: none. */
async function shows(me, page, wanted) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await shownOptions(page);
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    if (Date.now() > deadline) {
      await me.capture("timeout-task-start-options");
      assert.fail(`the Agents tab never showed ${JSON.stringify(wanted)}: ${JSON.stringify(seen)}`);
    }
  }
}

/** Hands the owner's task to Codex from the Agents tab, as the owner does, and waits until the
 * engine finished it on a live tools-on agent. Inputs: the owner on the page, the page, the
 * project and the engine root. Output: the engine's rows then. */
async function handOff(me, page, project, runtime) {
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(`tg-hand-to-${project}`)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === "codex")) return false;
    select.focus();
    select.value = "codex";
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, "no Codex to pick in the Send to list");
  await me.type(`[id="tg-hand-text-${project}"]`, TASK);
  await me.click("Hand off", "form.tg-hand button");
  await me.waitFor("the hand-off note", (text) => text.includes("Task handed off."));
  for (const deadline = Date.now() + TASK_MS; ; await pause(POLL_MS * 5)) {
    const row = engineRows(runtime, project);
    if (row && row.task === "done" && row.typed.includes(TASK)) {
      assert.equal(row.tools, true, "the sub-agent runs with tools on");
      return row;
    }
    assert.ok(Date.now() < deadline, `the engine never finished the task: ${JSON.stringify(row)}`);
  }
}

test("Task-start reminder On, the default: the Agents tab shows On and a tools-on sub-agent's first task turn names its context-mode tools and approved skill", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-tsr-on-", async ({ project, runtime, me, page }) => {
    await shows(me, page, [["On", "true"], ["Off", "false"]]);
    await me.capture("task-start-on");
    const done = await handOff(me, page, project, runtime);
    assert.equal(done.typed.split(REMINDER).length - 1, 1, `the first turn carries the reminder once: ${done.typed}`);
    assert.equal(storedTaskStart(runtime, project), null, "On is the default with no row saved");
  }));

test("Task-start reminder Off: the owner picks Off on the Agents tab, the engine keeps it and a tools-on sub-agent's first task turn carries no reminder", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-tsr-off-", async ({ project, runtime, me, page }) => {
    await shows(me, page, [["On", "true"], ["Off", "false"]]);
    await me.click("Off", OPTIONS);
    for (const deadline = Date.now() + STEP_MS; storedTaskStart(runtime, project) !== "off"; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `the engine never kept Off: ${storedTaskStart(runtime, project)}`);
    }
    await shows(me, page, [["On", "false"], ["Off", "true"]]);
    await me.capture("task-start-off");
    const done = await handOff(me, page, project, runtime);
    assert.ok(!done.typed.includes("Task start reminder"), `Off gives the first turn no reminder: ${done.typed}`);
  }));
