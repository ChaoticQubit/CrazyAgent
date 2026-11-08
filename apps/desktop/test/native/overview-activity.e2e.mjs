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
import { INTERNALS, POLL_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const SHOWN_MS = 10000;
const GIT = "/usr/bin/git";
const AGENT = "agent-builder";
const AGENT_LABEL = "Ada (@builder) · Codex";
const GOAL = "Build the service";
const PROMPT = "Add the service endpoint";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Makes the owner's project: a git repository on main with one commit and the owner's Git
 * identity stored, so an Accept lands. Input: the folder. Output: the commit. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Seed service\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "add", ".");
  git(folder, ...OWNER, "commit", "-q", "-m", "Start the seed service");
  git(folder, "config", "user.name", "E2E Owner");
  git(folder, "config", "user.email", "owner@example.invalid");
  return git(folder, "rev-parse", "HEAD");
}

/** Puts a stand-in sub-agent on the team the way the engine keeps one, with its worktree, and
 * gives it active work toward a goal, as the master's assignment does. Inputs: the engine
 * root, the project, the owner repository, the worktree and the goal. Output: the agent's
 * engine id. */
function agentAtWork(runtime, project, repo, worktree, goal) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?,'sub','codex',1,'stopped',?,?)").run(project, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(id, project, repo, worktree, join(repo, ".git", "worktrees", AGENT));
    store.prepare("INSERT INTO coord_work(project,title,owner_agent,state,version,evidence,created,updated) VALUES (?,?,?,'active',1,NULL,?,?)").run(project, goal, id, now, now);
    return String(id);
  } finally {
    store.close();
  }
}

test("an accepted change set of an agent's work shows on Overview under its goal within 10 s", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-oa-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "seed-service");
  const worktree = join(root, "agents", AGENT);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, join(root, "agents")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const start = ownerRepo(repo);
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
    git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
    const member = agentAtWork(runtime, project, repo, worktree, GOAL);

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const group = (goal) => page.evaluate(`document.querySelector(${JSON.stringify(`section[aria-label="${goal}"]`)})?.innerText ?? null`);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/overview` });
        await me.waitFor("Recent activity read from the engine", (text) => /Recent activity/.test(text) && !/Loading activity\.\.\./.test(text));
        await me.capture("overview-before");
        assert.equal(await page.evaluate("document.visibilityState"), "visible", "the page is visible, so Overview keeps reading");
        assert.ok(log.calls.includes("desktop_project_activity"), "Overview read the engine's activity");
        assert.equal(await group(GOAL), null, "nothing happened toward the goal yet");

        writeFileSync(join(worktree, "README.md"), "# Seed service\n\nServes seeds over HTTP.\n");
        const made = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, member, AGENT_LABEL, repo, worktree, join(repo, ".git", "worktrees", AGENT), PROMPT], { encoding: "utf8", env: { ...env } });
        assert.equal(made.status, 0, `the engine records the agent's turn: ${made.stderr}`);
        const id = made.stdout.trim();
        assert.match(id, /^[0-9a-f]{32}$/, "an edited worktree makes a change set");
        const set = (await value(bridge, "desktop_changes", { projectId: project })).find((item) => item.id === id);
        assert.ok(set, "the change set is listed");
        const landed = await value(bridge, "desktop_accept_change", { id, expectedHead: set.head, message: PROMPT, target: null });
        assert.equal(landed.kind, "landed", `the owner accepts the change set: ${JSON.stringify(landed)}`);
        assert.notEqual(git(repo, "rev-parse", "HEAD"), start, "the Accept landed on main");

        const accepted = Date.now();
        let shown = null;
        for (;;) {
          shown = await group(GOAL);
          if (shown?.includes(`Accepted the changes of ${AGENT_LABEL}: ${PROMPT}`)) break;
          if (Date.now() - accepted > SHOWN_MS) {
            await me.capture("timeout-accepted-under-goal");
            assert.fail(`Overview never showed the Accept under ${GOAL} within ${SHOWN_MS} ms; the group shows: ${shown}`);
          }
          await pause(POLL_MS);
        }
        t.diagnostic(`the Accept showed under its goal after ${Date.now() - accepted} ms`);
        await me.capture("overview-accepted");
        assert.match(shown, /\bYou\b/, "the row names you as who accepted");
        assert.doesNotMatch(shown, /\bOwner\b/, "the owner is You, never Owner");
        assert.match(shown, /\d+ items? · last just now/, "the goal says how many events it has and when the last was");
        const first = await page.evaluate("document.querySelector('.pj-activity-goal')?.getAttribute('aria-label') ?? null");
        assert.equal(first, GOAL, "the goal with the newest event comes first");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
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
