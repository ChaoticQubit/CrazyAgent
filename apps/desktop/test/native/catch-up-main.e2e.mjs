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
import { AGENTS_VIEW, INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const TASK_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SETTING = "Catch up with main";
const LAND = "Write fruit.md with one line about apples.";
const TASK = "Merge main into your branch, then list the pears.";

/** The Codex stand-in the engine runs every team agent with: as it starts it keeps in `told`
 * whether the developer instructions it was given tell it to merge main. A turn about apples
 * commits fruit.md with the git on its PATH, the agent's own; a turn about pears keeps what
 * that git says `crazyagent/main` is in `main`, then runs `git merge --no-edit crazyagent/main`
 * and keeps its output and exit status in `merged`. It keeps a conversation file as Codex does
 * and answers each turn with "done". */
const MERGING_CODEX = String.raw`#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/11/07"
/bin/mkdir -p "$d" || exit 1
id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
f="$d/rollout-2026-11-07T00-00-00-$id.jsonl"
printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
if /usr/bin/grep -qF 'git merge crazyagent/main before you finish' "$CODEX_HOME/config.toml"; then echo told; else echo untold; fi > "$CODEX_HOME/told"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
said=
while IFS= read -r line; do
  case "$line" in *"$e[200~"*) open=1; said= ;; esac
  said="$said $line"
  case "$line" in *"$e[201~"*) open=0 ;; esac
  [ "$open" = 1 ] && continue
  case "$said" in
    *pears*)
      git rev-parse crazyagent/main > "$CODEX_HOME/main" 2>&1
      git merge --no-edit crazyagent/main > "$CODEX_HOME/merged" 2>&1
      printf 'exit %s\n' "$?" >> "$CODEX_HOME/merged" ;;
    *apples*)
      printf 'Apples are crisp.\n' > fruit.md
      git add fruit.md && GIT_AUTHOR_NAME=Agent GIT_AUTHOR_EMAIL=agent@example.invalid GIT_COMMITTER_NAME=Agent GIT_COMMITTER_EMAIL=agent@example.invalid git commit -q -m 'Write fruit.md' > "$CODEX_HOME/wrote" 2>&1
      printf 'exit %s\n' "$?" >> "$CODEX_HOME/wrote" ;;
  esac
  said=
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  /bin/sleep 0.3
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
`;

/** Makes the owner's project folder: a git repository on main with one commit and the owner's
 * Git name and email, which an Accept needs to commit the landed set. Input: the folder.
 * Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Fruit stand\n\nSells fruit.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the fruit stand"],
    ["config", "user.name", "E2E Owner"], ["config", "user.email", "owner@example.invalid"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes the stand-in Codex the engine is started with. Input: the test root. Output: its
 * absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, MERGING_CODEX, { mode: 0o700 });
  return path;
}

/** Reads what the engine keeps for one of the owner's tasks, its status, and what its agent's
 * stand-in kept: the commit of fruit.md, what its git says crazyagent/main is, the merge
 * output with its exit status and whether it was told to merge main. Inputs: the engine root,
 * the project and the task's words. Output: those, or null before the task exists. */
function taskRow(runtime, project, prompt) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const task = store.prepare("SELECT status, agent FROM team_tasks WHERE project=? AND prompt=?").get(project, prompt);
    if (!task) return null;
    const home = join(runtime, "team", "agents", `agent-${task.agent}`, "home");
    const read = (name) => existsSync(join(home, name)) ? readFileSync(join(home, name), "utf8") : null;
    return { status: task.status, wrote: read("wrote"), main: read("main"), merged: read("merged"), told: read("told")?.trim() ?? null };
  } finally {
    store.close();
  }
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project made from
 * the owner's repository, serves the built app to a browser page on the project's Agents
 * tab, runs the steps and stops everything. Inputs: the test context, the temporary folder
 * prefix and the steps, given the project, the engine root, the owner on the page, the page,
 * a page reload and the team status reader. Output: none. */
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
    const stored = await value(bridge, "desktop_project_setup", { id: project });
    const subs = [...(stored.team.sub_harnesses ?? []).filter((slot) => slot.harness !== "codex"), { harness: "codex", agents: 1, tools: true }];
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team: { ...stored.team, sub_harnesses: subs }, documents: [] };
    const saved = await value(bridge, "desktop_update_project_setup", { id: project, setup, name: null, revision: stored.revision, generation: 5 });
    assert.equal(saved.outcome, "applied", `the Codex sub-agent gets its team tools: ${JSON.stringify(saved)}`);
    const kept = (await value(bridge, "desktop_project_setup", { id: project })).team.sub_harnesses;
    assert.ok(kept.some((slot) => slot.harness === "codex" && slot.tools), `the project keeps a Codex sub-agent with team tools: ${JSON.stringify(kept)}`);
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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=settings` });
        await me.waitFor("the Catch up with main setting in Agent settings", (text) => text.includes(SETTING));
        const reload = async () => {
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(SETTING)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, "the reloaded page never showed the Catch up with main setting");
          }
        };
        await steps({ project, runtime, me, page, reload, status, bridge, evidence, repo });
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

/** Clicks one choice of the Catch up with main setting, as the owner does. Inputs: the owner
 * on the page and the choice's label. Output: none. */
function pick(me, label) {
  return me.click(label, `[role=radiogroup][aria-label="${SETTING}"] [role=radio]`);
}

/** Reads which choice of the Catch up with main setting the Agents tab shows as picked.
 * Input: the page. Output: the picked choice's label, or null. */
function shown(page) {
  return page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"] [role=radio][aria-checked=true]`)})?.innerText.trim() ?? null`);
}

/** Waits until the engine's team status holds the Catch up with main setting as given.
 * Inputs: the status reader and the mode. Output: none. */
async function kept(status, mode) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).catch_up_main;
    if (held === mode) return;
    assert.ok(Date.now() < deadline, `the engine never kept Catch up with main ${mode}; it holds ${JSON.stringify(held)}`);
  }
}

/** Brings the Catch up with main setting into the window, so a capture shows the pick: it
 * opens the Agents tab's Agent settings view first, since a hand-off leaves the Team view
 * open. Inputs: the owner on the page and the page. Output: none. */
async function showSetting(me, page) {
  await me.click("Agent settings", AGENTS_VIEW);
  const group = `document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"]`)})`;
  for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(`${group} !== null`)); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, "the Agent settings view never showed the Catch up with main setting");
  }
  return page.evaluate(`${group}.scrollIntoView({ block: "center" })`);
}

/** Hands a task to a Codex sub-agent from the Agents tab's Team view, as the owner does, and
 * waits until the engine finished it and its stand-in kept what `done` waits for. Inputs: the
 * owner on the page, the page, the project, the engine root, the task's words and the wait.
 * Output: what `taskRow` reads then. */
async function handOff(me, page, project, runtime, prompt, done) {
  await me.click("Team", AGENTS_VIEW);
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
  await me.type(`[id="tg-hand-text-${project}"]`, prompt);
  await me.click("Hand off", "form.tg-hand button");
  await me.waitFor("the hand-off note", (text) => text.includes("Task handed off."));
  for (const deadline = Date.now() + TASK_MS; ; await pause(POLL_MS * 5)) {
    const row = taskRow(runtime, project, prompt);
    if (row && done(row) && !["queued", "needs_pool", "running"].includes(row.status)) return row;
    assert.ok(Date.now() < deadline, `the engine never finished the task: ${JSON.stringify(row)}`);
  }
}

/** Lands one change set before the agent catches up: a Codex sub-agent commits fruit.md, and
 * the owner accepts the set it opens, as the Changes tab does. Inputs: what the steps get.
 * Output: the commit the set landed as on main. */
async function landOne({ me, page, project, runtime, bridge, repo }) {
  const row = await handOff(me, page, project, runtime, LAND, (row) => row.wrote !== null);
  assert.equal(row.status, "done", `the task finished: ${JSON.stringify(row)}`);
  assert.ok(row.wrote.trimEnd().endsWith("exit 0"), `the agent committed fruit.md: ${row.wrote}`);
  let set;
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS * 5)) {
    set = (await value(bridge, "desktop_changes", { projectId: project })).find((change) => change.state === "pending");
    if (set) break;
    assert.ok(Date.now() < deadline, "the agent's commit never opened a change set");
  }
  const outcome = await value(bridge, "desktop_accept_change", { id: set.id, expectedHead: set.head, message: null, target: null });
  assert.equal(outcome.kind, "landed", `the set lands: ${JSON.stringify(outcome)}`);
  const main = spawnSync(GIT, ["rev-parse", "main"], { cwd: repo, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: repo, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(main.stdout.trim(), outcome.commit, "main holds the landed commit");
  return outcome.commit;
}

/** Keeps what the agent's stand-in saw in the evidence folder. Inputs: the folder, the case
 * and the task's row. Output: none. */
function keep(evidence, label, row) {
  for (const name of ["told", "main", "merged"]) writeFileSync(join(evidence, `${label}-${name}.txt`), `${row[name] ?? "(none)"}\n`);
}

test("Catch up with main On: the owner's pick is kept, and after a set lands the agent is told to merge main and its own git reads and merges the landed crazyagent/main", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-cu-on-", async (run) => {
    const { project, runtime, me, page, reload, status, evidence } = run;
    assert.equal((await status()).catch_up_main, "on", "On is the default");
    assert.equal(await shown(page), "On", "the tab shows the default");
    await pick(me, "Off");
    await kept(status, "off");
    await pick(me, "On");
    await kept(status, "on");
    await reload();
    assert.equal(await shown(page), "On", "On is shown again after a reload");
    await showSetting(me, page);
    await me.capture("catch-up-on");

    const landed = await landOne(run);
    const row = await handOff(me, page, project, runtime, TASK, (row) => row.merged !== null);
    keep(evidence, "on", row);
    assert.equal(row.status, "done", `the task finished: ${JSON.stringify(row)}`);
    assert.equal(row.told, "told", "the agent's instructions tell it to merge crazyagent/main");
    assert.equal(row.main?.trim(), landed, `the agent's own git sees the landed set as crazyagent/main: ${row.main}`);
    assert.ok(row.merged?.trimEnd().endsWith("exit 0"), `the agent's own git merged crazyagent/main: ${row.merged}`);
    assert.ok(!row.merged.includes("is refused"), `the merge was not refused: ${row.merged}`);
    await showSetting(me, page);
    await me.capture("catch-up-on-merged");
  });
});

test("Catch up with main Off: the owner's pick is kept, and after a set lands the agent is not told to merge main and its own git refuses crazyagent/main", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-cu-off-", async (run) => {
    const { project, runtime, me, page, reload, status, evidence } = run;
    await pick(me, "Off");
    await kept(status, "off");
    await reload();
    assert.equal(await shown(page), "Off", "Off is shown again after a reload");
    await showSetting(me, page);
    await me.capture("catch-up-off");

    const landed = await landOne(run);
    const row = await handOff(me, page, project, runtime, TASK, (row) => row.merged !== null);
    keep(evidence, "off", row);
    assert.equal(row.status, "done", `the task finished: ${JSON.stringify(row)}`);
    assert.equal(row.main?.trim(), landed, `the landed set is in the agent's repository as crazyagent/main, and Off still refuses to merge it: ${row.main}`);
    assert.equal(row.told, "untold", "the agent's instructions say nothing of crazyagent/main");
    assert.ok(row.merged?.trimEnd().endsWith("exit 1"), `the agent's own git refused crazyagent/main: ${row.merged}`);
    assert.ok(row.merged.includes("crazyagent: git merge is refused."), `the refusal names the merge: ${row.merged}`);
    assert.ok(!row.merged.includes("git merge crazyagent/main"), `the refusal does not offer main: ${row.merged}`);
    await showSetting(me, page);
    await me.capture("catch-up-off-refused");
  });
});
