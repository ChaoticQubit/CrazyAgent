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
const AGENTS = [
  { name: "agent-north", label: "Ada (@north) · Codex", title: "# Seed shop for gardens" },
  { name: "agent-south", label: "Bo (@south) · Codex", title: "# Seed shop for balconies" },
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

/** Counts the resolve tasks the engine pinned to a team agent. Inputs: the engine root and
 * the agent's id. Output: the count. */
function resolveTasks(runtime, agent) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    return Number(store.prepare("SELECT count(*) AS n FROM team_task_pins WHERE agent=?").get(Number(agent)).n);
  } finally {
    store.close();
  }
}

test("a set that clashes with main goes back to its agent, never to the owner as a conflict, and lands once fixed (OS-8)", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-cu-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
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

    const agents = AGENTS.map((agent) => {
      const worktree = join(root, "agents", agent.name);
      git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
      return { ...agent, worktree, member: teamAgent(runtime, project, repo, worktree, agent.name) };
    });
    const recordTurn = (agent, prompt) => {
      const result = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, agent.member, agent.label, repo, agent.worktree, join(repo, ".git", "worktrees", agent.name), prompt], { encoding: "utf8", env: { ...env } });
      assert.equal(result.status, 0, `the engine records the agent's turn: ${result.stderr}`);
      const id = result.stdout.trim();
      assert.match(id, /^[0-9a-f]{32}$/, "an edited worktree makes a change set");
      return id;
    };
    const head = () => git(repo, "rev-parse", "HEAD");
    const [north, south] = agents;

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const seen = [];
      const pick = async (label) => {
        const at = await page.evaluate(`[...document.querySelectorAll(".ch-row")].findIndex((row) => row.innerText.includes(${JSON.stringify(label)}))`);
        assert.ok(at >= 0, `no change set row for ${label}`);
        await me.follow(`ul[aria-label="Change sets"] > li:nth-child(${at + 1}) .ch-row`);
      };
      const watch = async () => {
        const text = await page.evaluate("document.body.innerText");
        seen.push(text);
        return text;
      };
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/changes` });
        await me.waitFor("the Changes tab with no change sets yet", (text) => /No changes yet/.test(text), STEP_MS);

        for (const agent of agents) writeFileSync(join(agent.worktree, "README.md"), readFileSync(join(agent.worktree, "README.md"), "utf8").replace("# Seed shop", agent.title));
        recordTurn(north, "Name the shop for gardens");
        await me.waitFor("the first agent's set ready to review", (text) => text.includes(north.label) && /Ready to review/.test(text));
        await pick(north.label);
        await me.click("Accept");
        await me.click("Accept", "dialog button");
        await me.waitFor("the first set landed", (text) => /Accepted as [0-9a-f]{7} on main\./.test(text));
        const landed = head();
        assert.equal(readFileSync(join(repo, "README.md"), "utf8").split("\n")[0], north.title, "the first agent's line is on main");

        const second = recordTurn(south, "Name the shop for balconies");
        const fixing = await me.waitFor("the second set with its agent", (text) => text.includes(south.label) && /Agent is fixing conflicts with main/.test(text));
        await me.capture("catch-up-fixing");
        assert.doesNotMatch(fixing, /Conflicts with your branch/, "the owner is never shown the clash as a conflict");
        assert.equal(resolveTasks(runtime, south.member), 1, "the engine handed the second agent one resolve task");
        const merged = readFileSync(join(south.worktree, "README.md"), "utf8");
        assert.match(merged, /^<<<<<<< /m, "main is merged into the second agent's worktree with conflict markers");
        assert.equal(head(), landed, "sending the set back never moves main");

        writeFileSync(join(south.worktree, "README.md"), `# Seed shop for gardens and balconies\n\nSells seeds.\n`);
        assert.equal(recordTurn(south, "Resolve the conflicts with main"), second, "the fix lands on the same change set");
        await me.waitFor("the fixed set ready to review", (text) => text.includes(south.label) && /Ready to review/.test(text) && !/Agent is fixing conflicts with main/.test(text));
        await me.capture("catch-up-fixed");
        for (let at = 0; at < 3; at += 1) {
          await watch();
          await pause(POLL_MS);
        }
        assert.ok(seen.every((text) => !/Conflicts with your branch/.test(text)), "no read of Changes showed the clash as a conflict");

        await pick(south.label);
        await me.click("Accept");
        await me.click("Accept", "dialog button");
        await me.waitFor("the fixed set landed", (text) => /Accepted as [0-9a-f]{7} on main\./.test(text));
        assert.equal(git(repo, "rev-parse", `${head()}^`), landed, "the fixed set lands on top of the first");
        assert.equal(readFileSync(join(repo, "README.md"), "utf8"), "# Seed shop for gardens and balconies\n\nSells seeds.\n");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("catch-up-failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the page sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(log.trail, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
