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
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const REPO = "menu-shop";
const LEAD_TREE = "agent-menu-lead";
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const NOTE = "Please keep the README short.";
const OWNER_STOPPED = "The owner stopped this teammate.";

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Writes the team as the engine keeps it while every agent rests between turns: a lead
 * with its own worktree, a worker it handed work to, and a second lead with nothing to
 * merge, all idle and run by no worker of this engine. Inputs: the engine root, the project,
 * the owner repository and the lead's worktree. Output: the three agent ids. */
function restingTeam(runtime, project, repo, worktree) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const agent = (parent) => Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,turns,parent) VALUES (?,'sub','codex',1,'idle',?,?,1,?)")
      .run(project, now, now, parent).lastInsertRowid);
    const lead = agent(null);
    const worker = agent(lead);
    const idle = agent(null);
    for (const id of [lead, worker, idle]) assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers each team agent");
    store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(lead, project, repo, worktree, join(repo, ".git", "worktrees", LEAD_TREE));
    return { lead, worker, idle };
  } finally {
    store.close();
  }
}

/** Writes the stand-in Codex executable the engine is started with. The engine runs its team
 * runtime, which ends an owner-stopped agent, only when it has a harness executable; this one
 * exits at once and answers nothing, so no model ever runs. Input: the test root. Output: its
 * absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** Reads what the engine keeps for the owner's messages to one agent and for each team
 * agent's state. Inputs: the engine root, the project and the agent ids. Output: the
 * messages to the first agent and every agent's status, error and owner stop. */
function engineRows(runtime, project, [first, ...rest]) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    const messages = store.prepare("SELECT from_kind, to_kind, to_agent, kind, body, receipt FROM coord_messages WHERE project=? AND to_agent=? ORDER BY id").all(project, first);
    const agents = new Map([first, ...rest].map((id) => [id, store.prepare("SELECT status, error, EXISTS(SELECT 1 FROM team_owner_stops WHERE agent=team_agents.id) AS stopped_by_owner FROM team_agents WHERE id=?").get(id)]));
    return { messages: messages.map((row) => ({ ...row })), agents };
  } finally {
    store.close();
  }
}

test("each card's menu messages, stops and merges its own agent through the running engine", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-tm-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  const worktree = join(root, "worktrees", LEAD_TREE);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, join(root, "worktrees")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Menu shop\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the menu shop");
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
    assert.ok(existsSync(join(runtime, "engine.db")), "the engine keeps its database in its root");
    git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
    const { lead, worker, idle } = restingTeam(runtime, project, repo, worktree);

    const graph = await value(bridge, "desktop_team_graph", { id: project });
    const nodeOf = (id) => graph.nodes.find((node) => node.id === `team-${id}`) ?? assert.fail(`no card for agent ${id}: ${JSON.stringify(graph.nodes)}`);
    assert.deepEqual([lead, worker, idle].map((id) => [nodeOf(id).kind, nodeOf(id).agent, nodeOf(id).status]),
      [["lead", lead, "idle"], ["sub", worker, "idle"], ["lead", idle, "idle"]], "two idle leads and the lead's idle worker are drawn");
    assert.equal(nodeOf(worker).parent, `team-${lead}`, "the worker hangs under its lead");
    const [leadName, workerName, idleName] = [lead, worker, idle].map((id) => nodeOf(id).label);
    assert.equal(new Set([leadName, workerName, idleName]).size, 3, `each agent has its own name: ${leadName}, ${workerName}, ${idleName}`);

    writeFileSync(join(worktree, "README.md"), "# Menu shop\n\nNow with a menu.\n");
    const recorded = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, String(lead), leadName, repo, worktree, join(repo, ".git", "worktrees", LEAD_TREE), "Add a menu"], { encoding: "utf8", env: { ...env } });
    assert.equal(recorded.status, 0, `the engine records the lead's turn: ${recorded.stderr}`);
    assert.match(recorded.stdout.trim(), /^[0-9a-f]{32}$/, "the lead's edit makes a change set");

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const cardOf = (name) => page.evaluate(`(() => {
        const card = [...document.querySelectorAll(".tg-card")].find((item) => item.querySelector(${JSON.stringify(`[aria-label="Actions for ${name}"]`)}));
        return card ? card.innerText : null;
      })()`);
      const choose = async (name, item) => {
        await me.click(`Actions for ${name}`);
        await me.click(item, "[role=menuitem]");
      };
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
        await me.waitFor("the Agents tab with the three cards", (text) => text.includes("Team graph") && text.includes(leadName) && text.includes(workerName) && text.includes(idleName), STEP_MS);
        await me.capture("team-menu-resting");

        await choose(leadName, "Message");
        await me.waitFor("the message dialog", (text) => text.includes(`Message ${leadName}`) && text.includes("one tick sent, two delivered"));
        await me.type("dialog textarea", NOTE);
        await me.click("Send", "dialog button");
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const card = await cardOf(leadName);
          if (card?.includes("From you 1 ✓")) break;
          if (Date.now() > deadline) {
            await me.capture("timeout-message-chip");
            assert.fail(`the lead's card never showed one sent message: ${card}`);
          }
        }
        await me.capture("team-menu-messaged");
        const sent = engineRows(runtime, project, [lead]).messages;
        assert.deepEqual(sent, [{ from_kind: "owner", to_kind: "agent", to_agent: lead, kind: "prompt", body: NOTE, receipt: "sent" }], "the engine keeps the one prompt from the owner to the lead");
        const edges = (await value(bridge, "desktop_team_graph", { id: project })).messages.filter((edge) => edge.from === "owner");
        assert.deepEqual(edges, [{ from: "owner", to: `team-${lead}`, unread: 1, receipt: "sent" }], "the graph draws one unread message from the owner to the lead");

        await choose(workerName, "Stop");
        await me.waitFor("the stop question", (text) => text.includes(`Stop ${workerName}?`) && text.includes("you can wake it later from Finished agents"));
        await me.click("Stop", "dialog button");
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const state = engineRows(runtime, project, [worker]).agents.get(worker);
          if (state.status === "stopped") {
            assert.deepEqual({ ...state }, { status: "stopped", error: OWNER_STOPPED, stopped_by_owner: 1 }, "the runtime ends the worker no worker of its own runs, as the owner asked");
            break;
          }
          assert.ok(Date.now() < deadline, `the engine never stopped the worker: ${JSON.stringify(state)}`);
        }
        await me.waitFor("the worker in Finished agents and off the graph", (text) => text.includes("Finished (1)"));
        for (const deadline = Date.now() + STEP_MS; (await cardOf(workerName)) !== null; await pause(POLL_MS)) {
          if (Date.now() > deadline) {
            await me.capture("timeout-worker-card");
            assert.fail("Finished agents is Hide, so the stopped worker's card leaves the graph on its next read");
          }
        }
        const finished = await page.evaluate("[...document.querySelectorAll('details[aria-label=\"Finished agents\"] li')].map((row) => row.textContent)");
        assert.equal(finished.length, 1, `Finished lists the one stopped agent: ${JSON.stringify(finished)}`);
        assert.ok(finished[0].includes(workerName), `${workerName} is in Finished: ${JSON.stringify(finished)}`);
        const others = engineRows(runtime, project, [lead, idle]).agents;
        assert.deepEqual([lead, idle].map((id) => ({ ...others.get(id) })), [{ status: "idle", error: null, stopped_by_owner: 0 }, { status: "idle", error: null, stopped_by_owner: 0 }], "stopping the worker leaves both leads resting");
        await me.capture("team-menu-stopped");

        await choose(idleName, "Merge");
        await me.waitFor("the plain note for a lead with nothing to merge", (text) => text.includes(`No changes from ${idleName} wait to be merged.`));
        assert.ok((await page.evaluate("location.hash")).endsWith("/agents"), "with nothing to merge the owner stays on the Agents tab");
        await me.click("Dismiss");

        await choose(leadName, "Merge");
        await me.waitFor("the lead's change set on the Changes tab", (text) => text.includes(leadName) && /Ready to review/.test(text));
        assert.ok((await page.evaluate("location.hash")).endsWith("/changes"), "Merge opens the project's Changes tab");
        await me.capture("team-menu-merge");
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
});
