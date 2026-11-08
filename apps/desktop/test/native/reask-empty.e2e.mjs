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
import { AGENTS_VIEW, INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const TASK_MS = 120000;
const SETTLE_MS = 5000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SETTING = "Re-ask a sub-agent's empty reply";
const TASK = "Write fruit.md with one line about apples.";
const REASK = "Your master got no reply text from your last turn. Reply now with a short summary of what you did for your task.";
const EMPTY = "Done, with no reply text.";

/** The Codex stand-in the engine runs every team agent with: it keeps a conversation file
 * as Codex does and ends each turn with no reply text, except a turn that asks it again for
 * its reply, which it answers with "pear". Every pasted line goes to `typed` in its home. */
const QUIET_ONCE_CODEX = String.raw`#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/11/02"
/bin/mkdir -p "$d" || exit 1
id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
f="$d/rollout-2026-11-02T00-00-00-$id.jsonl"
printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0; printf '%s\n' "$line" >> "$CODEX_HOME/typed" ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && { printf '%s\n' "$line" >> "$CODEX_HOME/typed"; continue; } ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  /bin/sleep 0.3
  case "$line" in
    *"no reply text from your last turn"*) printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"pear"}]}}\n' >> "$f" ;;
  esac
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
  writeFileSync(path, QUIET_ONCE_CODEX, { mode: 0o700 });
  return path;
}

/** Reads what the engine keeps for the owner's task: its status, result and agent, that
 * agent's turns, and how many times the re-ask reached the agent, from its stand-in's log.
 * Inputs: the engine root and the project. Output: those, or null before the task exists. */
function taskRow(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const task = store.prepare("SELECT status, result, agent FROM team_tasks WHERE project=? AND prompt=?").get(project, TASK);
    if (!task) return null;
    const turns = task.agent === null ? null : store.prepare("SELECT turns FROM team_agents WHERE id=?").get(task.agent)?.turns ?? null;
    const typed = join(runtime, "team", "agents", `agent-${task.agent}`, "home", "typed");
    const asked = existsSync(typed) ? readFileSync(typed, "utf8").split(REASK).length - 1 : 0;
    return { status: task.status, result: task.result, turns, asked };
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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=tools` });
        await me.waitFor("the Re-ask setting in Agent settings", (text) => text.includes(SETTING));
        const reload = async () => {
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(SETTING)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, "the reloaded page never showed the Re-ask setting");
          }
        };
        await steps({ project, runtime, me, page, reload, status });
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
  assert.deepEqual(terminalServers(runtime), [], "no tmux server outlives the test's engine");
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Clicks one choice of the Re-ask setting, as the owner does. Inputs: the owner on the page
 * and the choice's label. Output: none. */
function pick(me, label) {
  return me.click(label, `[role=radiogroup][aria-label="${SETTING}"] [role=radio]`);
}

/** Reads which choice of the Re-ask setting the Agents tab shows as picked. Input: the page.
 * Output: the picked choice's label, or null. */
function shown(page) {
  return page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"] [role=radio][aria-checked=true]`)})?.innerText.trim() ?? null`);
}

/** Waits until the engine's team status holds the Re-ask setting as given. Inputs: the
 * status reader and the switch. Output: none. */
async function kept(status, on) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).reask_empty_reply;
    if (held === on) return;
    assert.ok(Date.now() < deadline, `the engine never kept Re-ask ${on}; it holds ${JSON.stringify(held)}`);
  }
}

/** Hands the task to a Codex sub-agent from the Agents tab's Team view, as the owner does, and waits
 * until the engine finishes it. Inputs: the owner on the page, the page, the project and the
 * engine root. Output: what `taskRow` reads once the task is done. */
async function handOff(me, page, project, runtime) {
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
  for (const deadline = Date.now() + TASK_MS; ; await pause(POLL_MS * 5)) {
    const row = taskRow(runtime, project);
    if (row && !["queued", "needs_pool", "running"].includes(row.status)) return row;
    assert.ok(Date.now() < deadline, `the engine never finished the task: ${JSON.stringify(row)}`);
  }
}

/** Reads the latest sub-agent events in the open questions drawer. Input: the page. Output:
 * their text. */
function events(page) {
  return page.evaluate(`document.querySelector('section[aria-label="Sub-agent events"] ul[aria-label="Latest events"]')?.innerText ?? ""`);
}

/** Waits until the latest sub-agent events, in the drawer behind the Agents toolbar's "N ask
 * you", show the text, opening the drawer first. Inputs: the owner on the page, the page,
 * what the owner waits for and the text. Output: none. */
async function eventShown(me, page, what, text) {
  if (!(await page.evaluate(`document.querySelector("dialog.drawer[open]") !== null`))) await me.asks();
  for (const deadline = Date.now() + STEP_MS; !(await events(page)).includes(text); await pause(POLL_MS)) {
    if (Date.now() > deadline) {
      await me.capture(`timeout-${what.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`);
      assert.fail(`the owner never saw ${what}; the events show:\n${await events(page)}`);
    }
  }
}

test("Re-ask empty reply On: the owner's pick is kept, and a sub-agent whose task ends with no reply text is asked once and its reply is the result", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-ra-on-", async ({ project, runtime, me, page, reload, status }) => {
    assert.equal((await status()).reask_empty_reply, true, "On is the default");
    assert.equal(await shown(page), "On", "the tab shows the default");
    await pick(me, "Off");
    await kept(status, false);
    await pick(me, "On");
    await kept(status, true);
    await reload();
    assert.equal(await shown(page), "On", "On is shown again after a reload");
    await me.capture("reask-on");

    assert.deepEqual(await handOff(me, page, project, runtime), { status: "done", result: "pear", turns: 2, asked: 1 },
      "the quiet sub-agent was asked once more and its second reply is the task's result");
    await eventShown(me, page, "the sub-agent's reply in the events", "pear");
    await pause(SETTLE_MS);
    assert.deepEqual(taskRow(runtime, project), { status: "done", result: "pear", turns: 2, asked: 1 }, "the agent is not asked again after it replied");
    assert.ok(!(await events(page)).includes(EMPTY), `no empty reply is reported: ${await events(page)}`);
    await me.capture("reask-on-replied");
  });
});

test("Re-ask empty reply Off: the owner's pick is kept, and a sub-agent's task that ends with no reply text is reported at once without asking again", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-ra-off-", async ({ project, runtime, me, page, reload, status }) => {
    await pick(me, "Off");
    await kept(status, false);
    await reload();
    assert.equal(await shown(page), "Off", "Off is shown again after a reload");
    await me.capture("reask-off");

    assert.deepEqual(await handOff(me, page, project, runtime), { status: "done", result: null, turns: 1, asked: 0 },
      "the task finished on its one quiet turn");
    await eventShown(me, page, "the empty reply in the events", EMPTY);
    await pause(SETTLE_MS);
    assert.deepEqual(taskRow(runtime, project), { status: "done", result: null, turns: 1, asked: 0 }, "the agent is never asked again");
    await me.capture("reask-off-reported");
  });
});
