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
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const QUESTION = "Which page should the menu open on?";

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Writes one sub-agent as the engine keeps it while it rests between turns: idle, so it
 * counts as running and has a terminal name, and run by no worker of this engine. Inputs: the
 * engine root and the project. Output: its id. */
function restingAgent(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,turns,parent) VALUES (?,'sub','codex',1,'idle',?,?,1,NULL)")
      .run(project, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    return id;
  } finally {
    store.close();
  }
}

/** Writes the stand-in Codex executable the engine is started with. The engine runs its team
 * runtime only when it has a harness executable; this one exits at once and answers nothing,
 * so no model ever runs. Input: the test root. Output: its absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** The terminal panels on the page, by name. Input: the page. Output: their names. */
function panels(page) {
  return page.evaluate(`[...document.querySelectorAll("section.tg-terminal")].map((panel) => panel.getAttribute("aria-label"))`);
}

/** Waits until the named agent's panel is past connecting: its terminal attached, or it shows
 * why not, as Not ready on the bridge, which has no terminals. Inputs: the page and the agent's
 * name. Output: none. */
async function settled(page, label) {
  const done = `(() => {
    const panel = [...document.querySelectorAll("section.tg-terminal")].find((section) => section.getAttribute("aria-label") === ${JSON.stringify(`${label} terminal`)});
    return panel !== undefined && !panel.textContent.includes("Connecting to the terminal");
  })()`;
  for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(done)); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `${label}'s terminal never got past connecting: ${JSON.stringify(await panels(page))}`);
  }
}

/** Clicks the graph card of the named agent, waits for the one panel to show it and, unless
 * told not to, for that panel to settle as the owner sees it before clicking on. Inputs: the
 * page, the agent's name and whether to wait for the panel to settle. */
async function openCard(page, label, settle = true) {
  const clicked = await page.evaluate(`(() => {
    const card = [...document.querySelectorAll(".tg-card-open")].find((button) => button.querySelector(".tg-card-title")?.textContent.includes(${JSON.stringify(label)}));
    card?.click();
    return card !== undefined;
  })()`);
  assert.ok(clicked, `the graph draws a card for ${label}`);
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const shown = await panels(page);
    if (shown.includes(`${label} terminal`)) break;
    assert.ok(Date.now() < deadline, `${label}'s terminal never opened: ${JSON.stringify(shown)}`);
  }
  if (settle) await settled(page, label);
}

test("on the Agents tab one terminal panel follows the clicked agent, from Open terminal and from the graph", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-one-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "menu-shop");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
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
    const agents = [restingAgent(runtime, project), restingAgent(runtime, project), restingAgent(runtime, project)];
    const graph = await value(bridge, "desktop_team_graph", { id: project });
    const labels = agents.map((agent) => (graph.nodes.find((item) => item.id === `team-${agent}`) ?? assert.fail(`no card for agent ${agent}: ${JSON.stringify(graph.nodes)}`)).label);
    assert.equal(new Set(labels).size, 3, `each agent has its own name: ${labels.join(", ")}`);

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
        await me.waitFor("the Agents tab with the team graph", (text) => text.includes("Team graph") && labels.every((label) => text.includes(label)));

        const attached = () => log.trail.filter((call) => call.cmd === "desktop_terminal_attach").map((call) => call.args.agent);
        const held = [];
        let holding = true;
        page.on("Fetch.requestPaused", (event) => {
          if (holding) held.push(event.requestId);
          else page.send("Fetch.continueRequest", { requestId: event.requestId }).catch(() => {});
        });
        await page.send("Fetch.enable", { patterns: [{ urlPattern: "*/assets/xterm-*", requestStage: "Request" }] });
        await openCard(page, labels[2], false);
        for (const deadline = Date.now() + STEP_MS; held.length === 0; await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, "the first panel never asked for its terminal code");
        }
        await openCard(page, labels[0], false);
        holding = false;
        for (const id of held.splice(0)) await page.send("Fetch.continueRequest", { requestId: id });
        await page.send("Fetch.disable");
        await settled(page, labels[0]);
        assert.deepEqual(attached(), [`team-${agents[0]}`], "a panel closed before its terminal code loaded asks no attach; the panel that replaced it asks once");
        for (const label of labels.slice(1)) await openCard(page, label);
        assert.deepEqual(await panels(page), [`${labels[2]} terminal`], "three graph clicks leave one terminal panel, showing the last agent clicked");
        await me.capture("one-terminal-graph");

        const asked = spawnSync(bridgeBinary, ["ask-owner", runtime, project, String(agents[0]), "normal", QUESTION], { encoding: "utf8", env: { ...env } });
        assert.equal(asked.status, 0, `the agent's question reaches the engine: ${asked.stderr}`);
        await me.asks();
        await me.waitFor("the question in the inbox", (text) => text.includes(QUESTION), STEP_MS);
        await me.click("Open terminal", "li.tg-question button");
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
          const shown = await panels(page);
          if (shown.includes(`${labels[0]} terminal`)) break;
          assert.ok(Date.now() < deadline, `Open terminal never showed ${labels[0]}: ${JSON.stringify(shown)}`);
        }
        await settled(page, labels[0]);
        assert.deepEqual(await panels(page), [`${labels[0]} terminal`], "Open terminal on a question replaces the graph's panel, it opens no second one");
        await openCard(page, labels[1]);
        assert.deepEqual(await panels(page), [`${labels[1]} terminal`], "a graph click after Open terminal still leaves one panel");
        await me.capture("one-terminal-question");

        assert.deepEqual(attached(), [...agents, agents[0], agents[1]].map((agent) => `team-${agent}`), "each open asks to attach the clicked agent's own terminal, once");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });

    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.ok(native.has("desktop_terminal_attach"), "terminal attach is native-only, so this run proves the requests and the one panel, not a live attach");
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
