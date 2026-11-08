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
const HELD_MS = 8000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TASK = "Write fruit.md with one line about apples.";
const NOTE = "Please add a line about pears too.";
const NO_CHANNEL = "Can't receive messages: Native only needs tools on for Codex.";
const WAITS = `${NO_CHANNEL} It waits in the agent's mailbox until tools are on or message delivery changes.`;

/** The Codex stand-in the engine runs every team agent with, tools off: it keeps a
 * conversation file as Codex does, answers each turn with "Wrote fruit.md." and writes every
 * pasted line to `typed` in its home, so the test sees what the engine typed into it. */
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

/** Reads what the engine keeps for the owner's task and its agent: the task's status, the
 * agent's id, tools switch and status, the receipt of the owner's message to it, and whether
 * the message reached its stand-in. Inputs: the engine root and the project. Output: those,
 * or null before the task exists. */
function engineRows(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const task = store.prepare("SELECT status, agent FROM team_tasks WHERE project=? AND prompt=?").get(project, TASK);
    if (!task) return null;
    const agent = task.agent === null ? null : store.prepare("SELECT tools, status FROM team_agents WHERE id=?").get(task.agent) ?? null;
    const message = task.agent === null ? null
      : store.prepare("SELECT receipt FROM coord_messages WHERE project=? AND to_kind='agent' AND to_agent=? AND body=?").get(project, task.agent, NOTE) ?? null;
    const typed = join(runtime, "team", "agents", `agent-${task.agent}`, "home", "typed");
    return {
      task: task.status,
      agent: task.agent,
      tools: agent === null ? null : agent.tools === 1,
      status: agent?.status ?? null,
      receipt: message?.receipt ?? null,
      reached: existsSync(typed) && readFileSync(typed, "utf8").includes(NOTE),
    };
  } finally {
    store.close();
  }
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project made from
 * the owner's repository, on a keyless local account, serves the built app to a browser page
 * on the project's Agents tab, runs the steps and stops everything. Inputs: the test context,
 * the temporary folder prefix and the steps, given the project, the engine root, the owner on
 * the page, the page, the team status reader and the graph reader. Output: none. */
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
    const graph = () => value(bridge, "desktop_team_graph", { id: project });

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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/settings?section=team` });
        await codexToolsOff(me, page, () => value(bridge, "desktop_project_setup", { id: project }));
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
        await me.waitFor("the Team graph on the Agents tab", (text) => text.includes("Team graph"));
        await steps({ project, runtime, me, page, status, graph });
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

/** Gives the project one Codex agent row with tools off in Project Settings > Team, as the owner
 * does, and waits until the engine keeps that team. Inputs: the owner on the page, the page and
 * the stored setup reader. Output: none. */
async function codexToolsOff(me, page, setup) {
  await me.waitFor("the Team section", (text) => text.includes("+ Add harness") && text.includes("Save team"));
  await me.click("+ Add harness");
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById("ps-row-0");
    if (!select || ![...select.options].some((option) => option.value === "codex" && !option.disabled)) return false;
    select.focus();
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(select, "codex");
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, "no Codex to pick for the new agent row");
  await me.follow(`[id="ps-tools-0"]`);
  assert.equal(await page.evaluate(`document.getElementById("ps-tools-0")?.getAttribute("aria-checked")`), "false", "the Codex row's tools switch is off");
  await me.click("Save team");
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const rows = (await setup())?.team?.sub_harnesses ?? [];
    if (rows.length === 1 && rows[0].harness === "codex" && rows[0].tools === false) return;
    assert.ok(Date.now() < deadline, `the engine never kept a tools-off Codex row: ${JSON.stringify(rows)}`);
  }
}

/** Picks one choice of the "Messages to Codex agents" setting in Agent settings, as the owner's
 * pick in its list does, waits until the engine keeps it and goes back to the Team view. Inputs:
 * the owner on the page, the page, the project, the status reader and the choice. Output: none. */
async function deliver(me, page, project, status, choice) {
  await me.click("Agent settings", AGENTS_VIEW);
  await me.waitFor("the Codex delivery setting in Agent settings", (text) => text.includes("Messages to Codex agents"));
  const id = `tg-delivery-codex-${project}`;
  const picked = await page.evaluate(`(() => {
    const select = document.getElementById(${JSON.stringify(id)});
    if (!select || select.disabled || ![...select.options].some((option) => option.value === ${JSON.stringify(choice)} && !option.disabled)) return false;
    select.focus();
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(select, ${JSON.stringify(choice)});
    select.dispatchEvent(new Event("input", { bubbles: true }));
    select.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  })()`);
  assert.ok(picked, `no ${choice} to pick in ${id}`);
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).coordination?.harnesses.find((item) => item.harness === "codex")?.delivery;
    if (held === choice) break;
    assert.ok(Date.now() < deadline, `the engine never kept Codex delivery ${choice}; it holds ${JSON.stringify(held)}`);
  }
  await me.click("Team", AGENTS_VIEW);
}

/** Hands the task to a Codex sub-agent from the Agents tab, as the owner does, and waits until
 * the engine finishes it while its agent stays up. Inputs: the owner on the page, the page, the
 * project and the engine root. Output: what `engineRows` reads then. */
async function handOff(me, page, project, runtime) {
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
    const row = engineRows(runtime, project);
    if (row && row.task === "done" && ["idle", "working"].includes(row.status)) return row;
    assert.ok(Date.now() < deadline, `the engine never finished the task on a live agent: ${JSON.stringify(row)}`);
  }
}

/** Reads the text of the graph card of one agent, or null when it has none. Inputs: the page
 * and the agent's name. Output: the card's text. */
function cardText(page, name) {
  return page.evaluate(`(() => {
    const card = [...document.querySelectorAll(".tg-card")].find((item) => item.querySelector(${JSON.stringify(`[aria-label="Actions for ${name}"]`)}));
    return card ? card.innerText : null;
  })()`);
}

/** Reads the Agents tab's Team table row of one agent, in the Agents and tasks fold the
 * caller opened, or null when it has none. Inputs: the page and the agent's name. Output: the
 * row's text. */
function rowText(page, name) {
  return page.evaluate(`(() => {
    const row = [...document.querySelectorAll("table.pj-table tbody tr")].find((item) => item.querySelector("th")?.innerText.includes(${JSON.stringify(name)}));
    return row ? row.innerText : null;
  })()`);
}

/** Waits until what the owner reads passes the check, polling the reader. Inputs: the owner
 * on the page, what the owner waits for, the reader and the check. Output: the last read. */
async function shownTo(me, what, read, ok) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const text = await read();
    if (ok(text)) return text;
    if (Date.now() > deadline) {
      await me.capture(`timeout-${what.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`);
      assert.fail(`the owner never saw ${what}; it reads: ${JSON.stringify(text)}`);
    }
  }
}

/** Opens the agent's Message dialog from its card menu, as the owner does, checks its helper
 * and sends the note. Inputs: the owner on the page, the agent's name and the helper the dialog
 * must show. Output: none. */
async function message(me, name, helper) {
  await me.click(`Actions for ${name}`);
  await me.click("Message", "[role=menuitem]");
  await me.waitFor("the message dialog", (text) => text.includes(`Message ${name}`) && text.includes(helper));
  await me.type("dialog textarea", NOTE);
  await me.click("Send", "dialog button");
  await me.waitFor("the dialog closed", (text) => !text.includes(`Message ${name}`));
}

/** Waits until the owner's message reached the agent's stand-in and the engine marks it typed.
 * Inputs: the engine root, the project and what the owner waits for. Output: the rows then. */
async function typedIn(runtime, project, what) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS * 5)) {
    const row = engineRows(runtime, project);
    if (row?.reached && row.receipt === "typed") return row;
    assert.ok(Date.now() < deadline, `${what}: the message never reached the agent: ${JSON.stringify(row)}`);
  }
}

/** Runs one Delivery option that types into a tools-off Codex agent: the owner's message
 * reaches it, and neither its Agents row, its card nor the Message dialog says it can't
 * receive messages. Inputs: the test context, the prefix and the choice, or null for the
 * default. Output: none. */
function typedOption(t, prefix, choice) {
  return onEngine(t, prefix, async ({ project, runtime, me, page, status, graph }) => {
    if (choice) await deliver(me, page, project, status, choice);
    const done = await handOff(me, page, project, runtime);
    assert.equal(done.tools, false, "the sub-agent runs with tools off");
    const name = (await graph()).nodes.find((node) => node.agent === done.agent)?.label ?? assert.fail("no card for the sub-agent");
    await shownTo(me, "the sub-agent's card", () => cardText(page, name), (text) => text !== null);
    assert.ok(!(await cardText(page, name)).includes(NO_CHANNEL), "the card does not say it can't receive messages");
    await me.unfold("Agents and tasks");
    assert.ok(!((await rowText(page, name)) ?? "").includes(NO_CHANNEL), "the Agents row does not say it can't receive messages");
    assert.equal((await status()).agents.find((agent) => agent.id === done.agent)?.message_note ?? null, null);
    await message(me, name, "It goes to the agent's mailbox.");
    await typedIn(runtime, project, `${choice ?? "auto"} typing`);
    await me.capture(`${choice ?? "auto"}-typed`);
  });
}

test("Messages to Codex agents Auto: a tools-off Codex sub-agent gets the owner's message typed in and nothing says it can't receive messages", { timeout: 600000 },
  (t) => typedOption(t, "ca-nod-auto-", null));

test("Messages to Codex agents Typing only: a tools-off Codex sub-agent gets the owner's message typed in and nothing says it can't receive messages", { timeout: 600000 },
  (t) => typedOption(t, "ca-nod-typing-", "typing_only"));

test("Messages to Codex agents Native only: a tools-off Codex sub-agent's row, card and Message dialog say it can't receive messages, the message stays Sent, and Auto then types it in", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-nod-native-", async ({ project, runtime, me, page, status, graph }) => {
    await deliver(me, page, project, status, "native_only");
    const done = await handOff(me, page, project, runtime);
    assert.equal(done.tools, false, "the sub-agent runs with tools off");
    const name = (await graph()).nodes.find((node) => node.agent === done.agent)?.label ?? assert.fail("no card for the sub-agent");
    assert.equal((await status()).agents.find((agent) => agent.id === done.agent)?.message_note, NO_CHANNEL, "the engine says it");
    await shownTo(me, "the sentence on the sub-agent's card", () => cardText(page, name), (text) => (text ?? "").includes(NO_CHANNEL));
    await me.unfold("Agents and tasks");
    await shownTo(me, "the sentence on the sub-agent's Agents row", () => rowText(page, name), (text) => (text ?? "").includes(NO_CHANNEL));
    await me.capture("native-only-says-so");
    await message(me, name, WAITS);
    for (const deadline = Date.now() + HELD_MS; Date.now() < deadline; await pause(POLL_MS * 5)) {
      const row = engineRows(runtime, project);
      assert.equal(row.receipt, "sent", `the message stays Sent: ${JSON.stringify(row)}`);
      assert.equal(row.reached, false, "Native only never types into the agent");
    }
    await me.capture("native-only-held");
    await deliver(me, page, project, status, "auto");
    await typedIn(runtime, project, "the switch to Auto");
    await shownTo(me, "the sentence gone from the card", () => cardText(page, name), (text) => text !== null && !text.includes(NO_CHANNEL));
    await me.unfold("Agents and tasks");
    await shownTo(me, "the sentence gone from the Agents row", () => rowText(page, name), (text) => text !== null && !text.includes(NO_CHANNEL));
    assert.equal((await status()).agents.find((agent) => agent.id === done.agent)?.message_note ?? null, null);
    await me.capture("native-only-then-auto");
  });
});
