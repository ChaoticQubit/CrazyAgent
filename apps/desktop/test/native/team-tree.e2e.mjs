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
const REPO = "tree-shop";
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const LEAD_TASK = "Run a native agent team";
const WORKER_TASKS = ["Write greet.py", "Write the README"];

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Writes a lead and its two workers as the engine leaves them after Stop all: stopped, no
 * task held, each with the done task it ran, the workers' handed out by the lead. Inputs: the
 * engine root and the project. Output: the lead and worker ids. */
function stoppedTeam(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const agent = (parent) => Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,stopped_ms,turns,parent) VALUES (?,'sub','codex',1,'stopped',?,?,?,1,?)")
      .run(project, now, now, now, parent).lastInsertRowid);
    const task = (prompt, id, parent) => store.prepare("INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,attempts,result,delivered,created_ms,started_ms,finished_ms,parent) VALUES (?,'master','codex',1,?,'done',?,1,'Done',1,?,?,?,?)")
      .run(project, prompt, id, now, now, now, parent);
    const lead = agent(null);
    const workers = [agent(lead), agent(lead)];
    task(LEAD_TASK, lead, null);
    WORKER_TASKS.forEach((prompt, index) => task(prompt, workers[index], lead));
    return { lead, workers };
  } finally {
    store.close();
  }
}

test("after every agent stops, each task keeps its agent's name and lead and only the master card is drawn", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-tt-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Tree shop\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the tree shop");
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
    assert.ok(existsSync(join(runtime, "engine.db")), "the engine keeps its database in its root");
    const { lead, workers } = stoppedTeam(runtime, project);

    const graph = await value(bridge, "desktop_team_graph", { id: project });
    assert.ok(graph.nodes.every((node) => node.kind === "master"), `Finished agents is Hide, so only the master is drawn: ${JSON.stringify(graph.nodes)}`);
    const edgeOf = (id) => graph.edges.find((edge) => edge.to === `team-${id}`);
    for (const id of [lead, ...workers]) assert.ok(edgeOf(id)?.agent_label, `a stopped agent's task keeps its node and name: ${JSON.stringify(graph.edges)}`);
    const leadName = edgeOf(lead).agent_label;
    const workerNames = workers.map((id) => edgeOf(id).agent_label);
    for (const id of workers) assert.equal(edgeOf(id).from, "master", "with its lead hidden a worker's task comes from the master, never the lead's own card");

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
        await me.waitFor("the Agents tab with the team graph", (text) => text.includes("Master") && text.includes("Running") && text.includes("Finished (3)"), STEP_MS);
        await me.click("All", "[role=radio]");
        await me.unfold("Agents and tasks");
        const all = await me.waitFor("every team task, done ones too", (text) => text.includes(LEAD_TASK) && WORKER_TASKS.every((task) => text.includes(task)), STEP_MS);
        await me.capture("team-tree-stopped");
        const rows = await page.evaluate("[...document.querySelectorAll('[aria-label=\"Team tasks\"] li')].map((row) => row.innerText)");
        const rowOf = (task) => rows.find((row) => row.includes(task)) ?? assert.fail(`no task row for ${task}:\n${all}`);
        assert.ok(rowOf(LEAD_TASK).includes(`From the master to ${leadName}`), `the lead's task names the master and the lead: ${rowOf(LEAD_TASK)}`);
        WORKER_TASKS.forEach((task, index) => {
          assert.ok(rowOf(task).includes(`From ${leadName} to ${workerNames[index]}`), `a worker's task names its lead and itself: ${rowOf(task)}`);
        });
        const cards = await page.evaluate("[...document.querySelectorAll('.tg-card')].map((card) => card.innerText)");
        assert.equal(cards.length, 1, `only the master card is drawn: ${JSON.stringify(cards)}`);
        for (const task of [LEAD_TASK, ...WORKER_TASKS]) {
          assert.ok(!cards[0].includes(task), `the master card shows no stopped agent's task: ${cards[0]}`);
        }
        const finished = await page.evaluate("[...document.querySelectorAll('details[aria-label=\"Finished agents\"] li')].map((row) => row.textContent)");
        assert.equal(finished.length, 3, `the Finished fold-out lists the three stopped agents: ${JSON.stringify(finished)}`);
        for (const name of [leadName, ...workerNames]) assert.ok(finished.some((row) => row.includes(name)), `${name} is in Finished: ${JSON.stringify(finished)}`);
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
});
