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
const REPO = "label-shop";
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const HANDOFF = "Write the label notes";
const RESTING = [["delegated to", false], ["leads", false], ["teammate of", false]];

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Runs one change against the engine's own database, as the team runtime and the
 * coordination layer write it. Inputs: the engine root and the change. Output: what the
 * change returns. */
function inStore(runtime, change) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    return change(store);
  } finally {
    store.close();
  }
}

/** Writes a team resting between turns: a lead with the worker it leads, and an agent that
 * leads none, all idle and run by no worker of this engine. Inputs: the engine root and the
 * project. Output: the three agent ids. */
function restingTeam(runtime, project) {
  return inStore(runtime, (store) => {
    const now = Date.now();
    const agent = (parent) => Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,turns,parent) VALUES (?,'sub','codex',1,'idle',?,?,1,?)")
      .run(project, now, now, parent).lastInsertRowid);
    const lead = agent(null);
    const worker = agent(lead);
    const leaf = agent(null);
    for (const id of [lead, worker, leaf]) assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers each team agent");
    return { lead, worker, leaf };
  });
}

test("each graph line names its relationship, and a live handoff and a message change the label and revert after", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-gl-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Label shop\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the label shop");
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
    const { lead, worker, leaf } = restingTeam(runtime, project);
    const [leadNode, workerNode, leafNode] = [lead, worker, leaf].map((id) => `team-${id}`);
    const relations = async () => (await value(bridge, "desktop_team_graph", { id: project })).relations
      .map((edge) => [edge.from, edge.to, edge.relation, edge.live])
      .sort((a, b) => a[1].localeCompare(b[1]));
    const standing = [["master", leadNode, "leads", null], [leadNode, workerNode, "teammate_of", null], ["master", leafNode, "delegated_to", null]]
      .sort((a, b) => a[1].localeCompare(b[1]));
    assert.deepEqual(await relations(), standing, "the engine names each pair as the approved mockup does, with nothing live");

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const labels = async () => (await page.evaluate(`[...document.querySelectorAll(".tg-canvas .tg-badges .tg-edge-label")]
        .map((label) => [label.textContent, label.classList.contains("live")])`)).sort((a, b) => a[0].localeCompare(b[0]));
      const liveLines = () => page.evaluate(`document.querySelectorAll(".tg-lines .tg-link.live").length`);
      const shows = async (what, want, lines) => {
        for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS * 5)) {
          const got = await labels();
          if (JSON.stringify(got) === JSON.stringify(want) && await liveLines() === lines) return;
          if (Date.now() >= deadline) {
            const hidden = await page.evaluate("document.visibilityState");
            assert.fail(`${what}: the graph lines read ${JSON.stringify(got)} with ${await liveLines()} live, not ${JSON.stringify(want)} with ${lines} (page ${hidden})`);
          }
        }
      };
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents` });
        await me.waitFor("the Agents tab with the team graph", (text) => text.includes("Team graph") && text.includes("Edges:"), STEP_MS);
        await shows("the resting team", RESTING, 0);
        const legend = await page.evaluate(`document.querySelector(".tg-legend")?.innerText ?? ""`);
        for (const word of ["leads", "delegated to", "teammate of", "live action, while it happens"]) assert.ok(legend.includes(word), `the legend names ${word}: ${legend}`);
        await me.capture("graph-labels-resting");

        const task = inStore(runtime, (store) => Number(store.prepare("INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms) VALUES (?,'master','codex',1,?,'queued',?,?)")
          .run(project, HANDOFF, leaf, Date.now()).lastInsertRowid));
        assert.deepEqual((await relations()).find((edge) => edge[1] === leafNode), ["master", leafNode, "delegated_to", { action: "handing_off", task: HANDOFF }],
          "the engine marks the master's open task on its line");
        await shows("a live handoff", [[`handing off: ${HANDOFF}`, true], ["leads", false], ["teammate of", false]], 1);
        await me.capture("graph-labels-handoff");
        inStore(runtime, (store) => store.prepare("UPDATE team_tasks SET status='done', result='Wrote them', attempts=1, delivered=1, started_ms=?, finished_ms=? WHERE id=?")
          .run(Date.now(), Date.now(), task));
        await shows("the handoff once its task is done", RESTING, 0);

        const message = inStore(runtime, (store) => Number(store.prepare("INSERT INTO coord_messages(project,from_kind,from_agent,to_kind,to_agent,kind,body,receipt,created,receipt_at) VALUES (?,'agent',?,'agent',?,'prompt','The labels are in notes.md','sent',?,?)")
          .run(project, worker, lead, Date.now(), Date.now()).lastInsertRowid));
        assert.deepEqual((await relations()).find((edge) => edge[1] === workerNode), [leadNode, workerNode, "teammate_of", { action: "messaging" }],
          "the engine marks the unread message on the pair's line, with no message text");
        await shows("an unread message", [["delegated to", false], ["leads", false], ["messaging", true]], 1);
        await me.capture("graph-labels-message");
        inStore(runtime, (store) => store.prepare("UPDATE coord_messages SET receipt='read', receipt_at=? WHERE id=?").run(Date.now(), message));
        await shows("the message once it is read", RESTING, 0);
        assert.deepEqual(await relations(), standing, "the engine is back to the standing relations");
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
