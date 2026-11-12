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
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const AGENT = ["-c", "user.name=E2E Agent", "-c", "user.email=agent@example.invalid"];
const SETTING = "Landing";
const AGENTS = [
  { name: "agent-north", label: "Ada (@north) · Codex" },
  { name: "agent-south", label: "Bo (@south) · Codex" },
];

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Puts one stand-in agent on the project's team as the engine keeps a stopped sub-agent:
 * its row and its worktree. Inputs: the engine root, the project, the owner repository, the
 * worktree and its name. Output: the agent's engine id. */
function teamAgent(runtime, project, repo, worktree, name) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?,'sub','codex',1,'stopped',?,?)").run(project, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(id, project, repo, worktree, join(repo, ".git", "worktrees", name));
    return String(id);
  } finally {
    store.close();
  }
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * puts two stopped stand-in agents in linked worktrees on its team, serves the built app to a
 * browser page on the project's Agent settings, runs the steps and stops everything. Inputs:
 * the test context, the temporary folder prefix and the steps, given the owner on the page,
 * the page, a page reload, the team status reader, the owner repository, the two agents and
 * a turn recorder. Output: none. */
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
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, join(root, "agents")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Seed shop\n\nSells seeds.\n");
  git(repo, "init", "-q", "-b", "main");
  git(repo, "config", "user.name", "E2E Owner");
  git(repo, "config", "user.email", "owner@example.invalid");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the seed shop");
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
    const status = () => value(bridge, "desktop_team_status", { id: project });

    const agents = AGENTS.map((agent) => {
      const worktree = join(root, "agents", agent.name);
      git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
      return { ...agent, worktree, member: teamAgent(runtime, project, repo, worktree, agent.name) };
    });
    const recordTurn = (agent, prompt) => {
      const result = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, agent.member, agent.label, repo, agent.worktree, join(repo, ".git", "worktrees", agent.name), prompt], { encoding: "utf8", env: { ...env } });
      assert.equal(result.status, 0, `the engine records the agent's turn: ${result.stderr}`);
      const id = result.stdout.trim();
      assert.match(id, /^[0-9a-f]{32}$/, "an agent commit makes a change set");
      return id;
    };

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);
    await withPage(async (page) => {
      const me = owner(page, evidence);
      const settings = `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=settings`;
      const group = `document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"]`)})`;
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: settings });
        await me.waitFor("the Landing setting in Agent settings", (text) => text.includes("Merge commit"));
        const reload = async () => {
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && ${group} !== null`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, "the reloaded page never showed the Landing setting");
          }
          await page.evaluate(`${group}.scrollIntoView({ block: "center" })`);
        };
        const changes = async () => {
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/changes` });
          await me.waitFor("the Changes tab", (text) => /Changes/.test(text), STEP_MS);
        };
        await steps({ me, page, reload, status, repo, agents, recordTurn, changes });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("landing-failure").catch(() => {});
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

/** Clicks one choice of the Landing setting, as the owner does. Inputs: the owner on the
 * page and the choice's label. Output: none. */
function pick(me, label) {
  return me.click(label, `[role=radiogroup][aria-label="${SETTING}"] [role=radio]`);
}

/** Reads which choice of the Landing setting the Agents tab shows as picked. Input: the
 * page. Output: the picked choice's label, or null. */
function shown(page) {
  return page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"] [role=radio][aria-checked=true]`)})?.innerText.trim() ?? null`);
}

/** Waits until the engine's team status holds the Landing setting as given. Inputs: the
 * status reader and the mode. Output: none. */
async function kept(status, mode) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).landing_mode;
    if (held === mode) return;
    assert.ok(Date.now() < deadline, `the engine never kept Landing ${mode}; it holds ${JSON.stringify(held)}`);
  }
}

/** Has the first agent commit one line in its own worktree, records its turn, and accepts
 * the set from the Changes tab as the owner does. Inputs: what the steps get. Output: the
 * branch tip before the accept, the agent's commit and the landed commit. */
async function landOne({ me, page, repo, agents, recordTurn, changes }) {
  const [north] = agents;
  const before = git(repo, "rev-parse", "HEAD");
  writeFileSync(join(north.worktree, "README.md"), "# Seed shop for gardens\n\nSells seeds.\n");
  git(north.worktree, "add", "README.md");
  git(north.worktree, ...AGENT, "commit", "-q", "-m", "Name the shop for gardens");
  const agentHead = git(north.worktree, "rev-parse", "HEAD");
  recordTurn(north, "Name the shop for gardens");
  await changes();
  await me.waitFor("the agent's set ready to review", (text) => text.includes(north.label) && /Ready to review/.test(text));
  const at = await page.evaluate(`[...document.querySelectorAll(".ch-row")].findIndex((row) => row.innerText.includes(${JSON.stringify(north.label)}))`);
  assert.ok(at >= 0, `no change set row for ${north.label}`);
  await me.follow(`ul[aria-label="Change sets"] > li:nth-child(${at + 1}) .ch-row`);
  await me.click("Accept");
  await me.click("Accept", "dialog button");
  await me.waitFor("the set landed", (text) => /Accepted as [0-9a-f]{7} on main\./.test(text));
  const landed = git(repo, "rev-parse", "HEAD");
  assert.notEqual(landed, before, "the accept moved main");
  assert.equal(readFileSync(join(repo, "README.md"), "utf8"), "# Seed shop for gardens\n\nSells seeds.\n", "the agent's line is on main");
  return { before, agentHead, landed };
}

/** Waits until the second, resting agent's worktree sits on the landed tip, as the engine
 * moves an agent with no unlanded work after an accept (DEC-147.6). Inputs: the agents and
 * the landed commit. Output: none. */
async function movedTo(agents, landed) {
  const [, south] = agents;
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const head = git(south.worktree, "rev-parse", "HEAD");
    if (head === landed) break;
    assert.ok(Date.now() < deadline, `the resting agent stayed on ${head}, not the landed ${landed}`);
  }
  assert.equal(git(south.worktree, "status", "--porcelain"), "", "the moved worktree has no edits");
  assert.equal(readFileSync(join(south.worktree, "README.md"), "utf8"), "# Seed shop for gardens\n\nSells seeds.\n", "the moved worktree holds the landed line");
}

test("Landing Squash: the default is kept, an accept lands one commit on the tip, and a resting agent moves onto it", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-land-squash-", async (run) => {
    const { me, page, reload, status, repo, agents } = run;
    assert.equal((await status()).landing_mode, "squash", "Squash is the default");
    await reload();
    assert.equal(await shown(page), "Squash", "the tab shows the default");
    await pick(me, "Merge commit");
    await kept(status, "merge");
    await pick(me, "Squash");
    await kept(status, "squash");
    await reload();
    assert.equal(await shown(page), "Squash", "Squash is shown again after a reload");
    await me.capture("landing-squash");
    const { before, landed } = await landOne(run);
    assert.equal(git(repo, "rev-list", "--parents", "-n", "1", landed), `${landed} ${before}`, "a squash commit has the old tip as its only parent");
    await movedTo(agents, landed);
    await me.capture("landing-squash-landed");
  });
});

test("Landing Merge commit: the owner's pick is kept and an accept lands a merge commit that keeps the agent's commit", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-land-merge-", async (run) => {
    const { me, page, reload, status, repo, agents } = run;
    await reload();
    await pick(me, "Merge commit");
    await kept(status, "merge");
    await reload();
    assert.equal(await shown(page), "Merge commit", "Merge commit is shown again after a reload");
    await me.capture("landing-merge");
    const { before, agentHead, landed } = await landOne(run);
    assert.equal(git(repo, "rev-list", "--parents", "-n", "1", landed), `${landed} ${before} ${agentHead}`, "the merge commit keeps the old tip and the agent's commit as parents");
    assert.equal(git(repo, "log", "-1", "--format=%s", agentHead), "Name the shop for gardens", "the agent's own commit is in the owner's history");
    await movedTo(agents, landed);
    await me.capture("landing-merge-landed");
  });
});
