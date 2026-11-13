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
import { AGENTS_VIEW, INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const TASK_MS = 240000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TASK = "Run the full test suite five times.";
const HEAVY = 5;
const OPTIONS = '[role=radiogroup][aria-label="Heavy lane"] [role=radio]';
const WORDS = { one: "1 at a time", two: "2 at a time", three: "3 at a time", four: "4 at a time", no_limit: "No limit" };

/** The Codex stand-in the engine runs every team agent with: it keeps a conversation file as
 * Codex does and, on its first turn, finds the heavy lane line its instructions hold in its
 * `config.toml`, writes it to `lane-line`, and runs five heavy commands through it at once,
 * each adding the first shared resource the instructions name; every command appends start,
 * sleeps 6 seconds and appends end to `lane-log` in its home. It ends the turn once all five
 * ended. */
const LANE_CODEX = String.raw`#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/11/05"
/bin/mkdir -p "$d" || exit 1
id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
f="$d/rollout-2026-11-05T00-00-00-$id.jsonl"
printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
ran=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  if [ "$ran" = 0 ]; then
    ran=1
    lane=$(/usr/bin/grep -o "'[^']*' lane --folder '[^']*'" "$CODEX_HOME/config.toml" | /usr/bin/head -n 1)
    uses=$(/usr/bin/grep -o "such as --uses [a-z0-9._-]*" "$CODEX_HOME/config.toml" | /usr/bin/head -n 1 | /usr/bin/sed 's/^such as //; s/[.]$//')
    printf '%s\n' "$lane" > "$CODEX_HOME/lane-line"
    if [ -n "$lane" ]; then
      for n in 1 2 3 4 5; do
        eval "$lane $uses --label heavy-$n -- /bin/sh -c 'echo start >> \"\$0\"; /bin/sleep 6; echo end >> \"\$0\"' \"\$CODEX_HOME/lane-log\" &"
      done
      wait
    fi
  fi
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Ran the suite five times."}]}}\n' >> "$f"
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
  writeFileSync(path, LANE_CODEX, { mode: 0o700 });
  return path;
}

/** Reads the coordination settings the engine keeps for a project. Inputs: the engine root and
 * the project. Output: the stored settings, or null while no row is saved. */
function storedCoord(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const row = store.prepare("SELECT json FROM coord_settings WHERE project=?").get(project);
    return row ? JSON.parse(row.json) : null;
  } finally {
    store.close();
  }
}

/** Reads the owner's task and what its agent's stand-in wrote: the lane line it found and the
 * heavy commands' log. Inputs: the engine root and the project. Output: those, or null before
 * the task exists. */
function engineRows(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const task = store.prepare("SELECT status, agent FROM team_tasks WHERE project=? AND prompt=?").get(project, TASK);
    if (!task) return null;
    const agent = task.agent === null ? null : store.prepare("SELECT tools FROM team_agents WHERE id=?").get(task.agent) ?? null;
    const home = join(runtime, "team", "agents", `agent-${task.agent}`, "home");
    const read = (name) => existsSync(join(home, name)) ? readFileSync(join(home, name), "utf8") : "";
    const lane = store.prepare("SELECT COUNT(*) AS held FROM coord_lane WHERE project=?").get(project);
    return { task: task.status, agent: task.agent, tools: agent === null ? null : agent.tools === 1, line: read("lane-line").trim(), log: read("lane-log"), held: lane.held };
  } finally {
    store.close();
  }
}

/** The most heavy commands the log shows running at once. Input: the log of start and end
 * lines in the order the commands wrote them. Output: the count. */
function mostAtOnce(log) {
  const lines = log.split("\n").filter(Boolean);
  assert.equal(lines.filter((line) => line === "start").length, HEAVY, `every heavy command ran: ${log}`);
  let running = 0;
  let most = 0;
  for (const line of lines) {
    assert.ok(line === "start" || line === "end", `the log holds start and end lines only: ${line}`);
    running += line === "start" ? 1 : -1;
    assert.ok(running >= 0, `an end never comes before its start: ${log}`);
    most = Math.max(most, running);
  }
  assert.equal(running, 0, `every heavy command ended: ${log}`);
  return most;
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project made from
 * the owner's repository, on a keyless local account, serves the built app to a browser page
 * on the Coordination group of the project's Agent settings, runs the steps and stops
 * everything. Inputs: the test context, the temporary folder prefix and the steps, given the
 * project, the engine root, the owner on the page and the page. Output: none. */
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
    assert.equal(storedCoord(runtime, project), null, "a new project keeps no coordination row");

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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=coord` });
        await me.waitFor("the Heavy lane setting in Agent settings", (text) => text.includes("Heavy lane") && text.includes("Shared resources"));
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
    await endTerminals(runtime);
  }
  assert.deepEqual(terminalServers(runtime), [], "no tmux server outlives the test's engine");
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Reads which Heavy lane option the Agents tab shows checked. Input: the page. Output: the
 * checked option's label, or null. */
async function checkedLane(page) {
  const seen = await page.evaluate(`[...document.querySelectorAll(${JSON.stringify(OPTIONS)})].map((radio) => [radio.innerText.trim(), radio.getAttribute("aria-checked")])`);
  assert.deepEqual(seen.map(([label]) => label), Object.values(WORDS), "the Heavy lane row lists every option in order");
  return seen.find(([, checked]) => checked === "true")?.[0] ?? null;
}

/** Picks a Heavy lane option on the Agents tab as the owner does, unless it is the default the
 * page already shows, and waits until the engine keeps it and the page shows it checked.
 * Inputs: the owner on the page, the page, the engine root, the project and the option.
 * Output: none. */
async function pickLane(me, page, runtime, project, option) {
  if (option === "one") {
    assert.equal(await checkedLane(page), WORDS.one, "1 at a time shows as the default");
    assert.equal(storedCoord(runtime, project), null, "the default keeps no row");
    return;
  }
  await me.click(WORDS[option], OPTIONS);
  for (const deadline = Date.now() + STEP_MS; storedCoord(runtime, project)?.heavy_lane !== option; await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `the engine never kept ${option}: ${JSON.stringify(storedCoord(runtime, project))}`);
  }
  for (const deadline = Date.now() + STEP_MS; (await checkedLane(page)) !== WORDS[option]; await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `the Agents tab never showed ${WORDS[option]} checked`);
  }
}

/** Hands the owner's task to Codex from the Agents tab's Team view, as the owner does, watches
 * the Team view show the heavy lane while the agent's commands run, and waits until the engine
 * finished the task and the lane is clear. Inputs: the owner on the page, the page, the
 * project, the engine root and what the lane panel must say. Output: the engine's rows then. */
async function handOff(me, page, project, runtime, panel) {
  await me.click("Team", AGENTS_VIEW);
  await me.unfold("Agents and tasks");
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
  let shown = false;
  for (const deadline = Date.now() + TASK_MS; ; await pause(POLL_MS)) {
    if (!shown) {
      const text = await page.evaluate(`document.querySelector('section[aria-label="Heavy lane"]')?.innerText ?? ""`);
      if (panel.test(text.replace(/\s+/gu, " "))) {
        shown = true;
        await me.capture("heavy-lane-team-view");
      }
    }
    const row = engineRows(runtime, project);
    if (row && row.task === "done" && row.held === 0) {
      assert.equal(row.tools, true, "the sub-agent runs with tools on");
      assert.ok(shown, `the Team view showed the heavy lane as ${panel}`);
      return row;
    }
    assert.ok(Date.now() < deadline, `the engine never finished the task: ${JSON.stringify(row)}`);
  }
}

for (const [option, most] of [["one", 1], ["two", 2], ["three", 3], ["four", 4], ["no_limit", HEAVY]]) {
  test(`Heavy lane ${WORDS[option]}: the owner picks it on the Agents tab, a sub-agent's five heavy commands through the lane line of its instructions run ${most} at a time, and the Team view shows the lane`, { timeout: 600000 }, (t) =>
    onEngine(t, `ca-lane-${option.replace("_", "-")}-`, async ({ project, runtime, me, page }) => {
      await pickLane(me, page, runtime, project, option);
      await me.capture(`heavy-lane-${option}`);
      const done = await handOff(me, page, project, runtime, new RegExp(`^Heavy lane ${WORDS[option]}: [1-${HEAVY}] running, [0-${HEAVY}] waiting`, "u"));
      assert.match(done.line, /^'\/[^']+' lane --folder '\/[^']+\/crazyagent-lane'$/u, `the instructions hold the lane line: ${done.line}`);
      assert.ok(done.line.includes(`agent-${done.agent}`), "the lane folder is the agent's own");
      assert.equal(mostAtOnce(done.log), most, `${WORDS[option]} runs ${most} at once: ${done.log}`);
    }));
}

test("Shared resource ollama:1 under No limit: the owner names it on the Agents tab, the instructions name it, and five heavy commands that use it run one at a time", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-lane-resource-", async ({ project, runtime, me, page }) => {
    await pickLane(me, page, runtime, project, "no_limit");
    await me.type(`[id="tg-resources-${project}"]`, "ollama:1");
    await me.click("Save", 'form[aria-label="Shared resources"] button');
    for (const deadline = Date.now() + STEP_MS; JSON.stringify(storedCoord(runtime, project)?.shared_resources) !== JSON.stringify([{ name: "ollama", slots: 1 }]); await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `the engine never kept ollama:1: ${JSON.stringify(storedCoord(runtime, project))}`);
    }
    assert.equal(storedCoord(runtime, project).heavy_lane, "no_limit", "the resource save keeps No limit");
    await me.capture("heavy-lane-resource");
    const done = await handOff(me, page, project, runtime, /^Heavy lane No limit: 1 running, [0-4] waiting.*uses ollama/u);
    const config = readFileSync(join(runtime, "team", "agents", `agent-${done.agent}`, "home", "config.toml"), "utf8");
    assert.ok(config.includes("This project's shared resources: ollama."), "the instructions name the shared resource");
    assert.equal(mostAtOnce(done.log), 1, `ollama:1 serves one command at a time: ${done.log}`);
  }));

test("A shared resource the engine refuses is never saved: the Agents tab names the reason and sends nothing", { timeout: 600000 }, (t) =>
  onEngine(t, "ca-lane-bad-resource-", async ({ project, runtime, me, page }) => {
    await me.type(`[id="tg-resources-${project}"]`, "ollama:9");
    await me.waitFor("the slots reason", (text) => text.includes("ollama can serve 1 to 8 commands at once."));
    const disabled = await page.evaluate(`document.querySelector('form[aria-label="Shared resources"] button[type=submit]').disabled`);
    assert.equal(disabled, true, "Save stays off for a refused resource");
    assert.equal(storedCoord(runtime, project), null, "nothing reaches the engine");
  }));
