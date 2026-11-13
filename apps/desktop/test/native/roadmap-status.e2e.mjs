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
import { INTERNALS, POLL_MS, STEP_MS, owner, required, serveApp, silentProvider, start as launch, stop, value } from "./e2e-harness.mjs";
import { answering } from "./codex-task-engine.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const QUIET_MS = 10 * 60 * 1000;
const CHECK_MS = 60000;
const GIT = "/usr/bin/git";
const AGENT = "agent-planner";
const AGENT_LABEL = "Pat (@planner) · Codex";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const ROADMAP = "# Roadmap\n\n## V1\n\n- Sign in\n- Search\n";
const DUE_LINE = "A new check is waiting to run.";
const STALE_LINE = `${DUE_LINE} After a landed change set it starts about 10 minutes after the last one.`;
const OFF_LINE = "Roadmap status is off in Project settings.";
const FAILED = "The last check failed:";
const NO_STATUS = "The agent's reply held no roadmap status";
const CODEX = answering("The roadmap check stand-in has no item states to give.");

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Puts a stopped sub-agent with a worktree on the project's team, the way the engine keeps
 * one between turns. Inputs: the engine root, the project, the owner repository and the
 * worktree. Output: the agent's engine id. */
function teamAgent(runtime, project, repo, worktree) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?,'sub','codex',1,'stopped',?,?)").run(project, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(id, project, repo, worktree, join(repo, ".git", "worktrees", AGENT));
    return String(id);
  } finally {
    store.close();
  }
}

/** Reads what the engine keeps of one project's roadmap status. Inputs: the engine root and
 * the project. Output: the stored row and setting, null fields before the first mark. */
function stored(runtime, project) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    const row = store.prepare("SELECT stale, due_ms, running_ms, problem, checked_ms FROM roadmap_status WHERE project=?").get(project);
    const setting = store.prepare("SELECT refresh FROM roadmap_status_settings WHERE project=?").get(project);
    return { stale: row?.stale ?? 0, due: row?.due_ms ?? null, running: row?.running_ms ?? null, problem: row?.problem ?? null, checked: row?.checked_ms ?? null, refresh: setting?.refresh ?? null };
  } finally {
    store.close();
  }
}

/** Waits until the engine's stored status passes a check. Inputs: the engine root, the
 * project, what is waited for, the check and the longest wait. Output: the stored status. */
async function until(runtime, project, what, check, ms) {
  assert.ok(ms > 0 && ms <= QUIET_MS + 2 * CHECK_MS, "a bounded wait");
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS * 4)) {
    const now = stored(runtime, project);
    if (check(now)) return now;
    assert.ok(Date.now() < deadline, `${what}: ${JSON.stringify(now)}`);
  }
}

test("Roadmap status: each Refresh option decides when a check runs, after a landed change set, on Refresh only, or never", { timeout: 1500000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-rs-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "plan-shop");
  const worktree = join(root, "agents", AGENT);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, join(repo, "docs"), join(root, "agents")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Plan shop\n");
  writeFileSync(join(repo, "docs", "roadmap.md"), ROADMAP);
  git(repo, "init", "-q", "-b", "main");
  git(repo, "add", ".");
  git(repo, ...OWNER, "commit", "-q", "-m", "Start the plan shop");
  const main = git(repo, "rev-parse", "HEAD");
  git(repo, "checkout", "-q", "-b", "owner-work");
  git(repo, "config", "user.name", "E2E Owner");
  git(repo, "config", "user.email", "owner@example.invalid");
  const provider = await silentProvider();
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider.port}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const codex = join(root, "bin", "codex");
  writeFileSync(codex, CODEX, { mode: 0o700 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let engine;
  let bridgeProcess;
  let site;
  try {
    ({ child: engine } = await launch(engineBinary,
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings", "--codex-executable", codex],
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
    assert.equal((await value(bridge, "desktop_open_project", { id: project })).project_id, project, "the new project opens");
    const pools = await value(bridge, "desktop_model_access_page", { collection: "pools", next: false });
    assert.equal(pools.writable, true, "the test engine's settings are writable");
    const assigned = await value(bridge, "desktop_edit_model_access", { ticket: pools.ticket, edit: { kind: "assign_project_pool", project, pool: "local-pool" }, generation: 4 });
    assert.equal(assigned.outcome, "applied", `the project gets the pool its checks run on, as the app's onboarding does: ${JSON.stringify(assigned)}`);
    const context = join(repo, ".crazyagent", "context");
    assert.ok(existsSync(context), "onboarding made the project's context folder, where a planning job writes the roadmap");
    writeFileSync(join(context, "roadmap.md"), ROADMAP, { mode: 0o600 });
    const first = await value(bridge, "desktop_roadmap_status", { id: project });
    assert.deepEqual([first.items, first.checked_ms, first.running, first.stale, first.refresh], [[], null, false, false, "after_landing"], "a new project has no status and follows landings");

    git(repo, "worktree", "add", "-q", "--detach", worktree, main);
    const member = teamAgent(runtime, project, repo, worktree);
    let edits = 0;
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
        const open = (tab) => page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/${tab}` });
        const land = async () => {
          edits += 1;
          writeFileSync(join(worktree, "README.md"), `# Plan shop\n${"Sign in works.\n".repeat(edits)}`);
          const turn = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, member, AGENT_LABEL, repo, worktree, join(repo, ".git", "worktrees", AGENT), `Add sign in, part ${edits}`], { encoding: "utf8", env: { ...env } });
          assert.equal(turn.status, 0, `the engine records the agent's turn: ${turn.stderr}`);
          await open("changes");
          await me.waitFor("the agent's change set", (text) => text.includes(AGENT_LABEL) && /Ready to review/.test(text));
          await me.follow(".ch-row");
          await me.waitFor("the change set's diff", (text) => /Sign in works\./.test(text));
          await me.click("Accept");
          await me.waitFor("the accept question", (text) => /Accept these changes\?/.test(text));
          await me.click("Accept", "dialog button");
          await me.waitFor("the landed change set", (text) => /Accepted as [0-9a-f]{7} on /.test(text));
          return Date.now();
        };
        const pick = async (label, name) => {
          await open("settings");
          await me.waitFor("the Roadmap status setting", (text) => text.includes("Roadmap status") && text.includes("Each check is one agent run on this project's pool."));
          await me.click(label);
          await until(runtime, project, `the engine never saved ${name}`, (now) => now.refresh === name, STEP_MS);
        };

        await open("overview");
        await me.click("Activity", "[role=tab]");
        await me.waitFor("the roadmap status before any check", (text) => text.includes("Roadmap status") && text.includes("Not checked yet"));
        const refreshOff = () => page.evaluate(`[...document.querySelectorAll("section[aria-labelledby=pj-roadmap-status] button")].find((button) => button.textContent.trim() === "Refresh")?.disabled ?? null`);
        assert.equal(await refreshOff(), false, "Refresh can be pressed while the status follows landings");
        await me.capture("roadmap-status-empty");

        const landed = await land();
        const marked = await until(runtime, project, "the landing never marked the status stale", (now) => now.stale === 1, STEP_MS);
        assert.ok(marked.due > landed - STEP_MS && marked.due <= Date.now() + QUIET_MS, `the check waits the quiet period after the landing: ${JSON.stringify(marked)}`);
        assert.equal(marked.problem, null, "no check ran inside the quiet period");
        await open("overview");
        await me.click("Activity", "[role=tab]");
        await me.waitFor("the line saying a check is due after the landing", (text) => text.includes(STALE_LINE));
        await me.capture("roadmap-status-after-landing-due");
        const ran = await until(runtime, project, "no check ran after the quiet period", (now) => now.stale === 0 && now.running === null && now.problem !== null, QUIET_MS + 2 * CHECK_MS);
        assert.ok(Date.now() >= marked.due, "the one check ran only once the quiet period was over");
        assert.equal(ran.problem, NO_STATUS, "the check ran on the pool's account and its stand-in answered no states");
        assert.equal(ran.checked, null, "a check that answered no states stores none");
        await me.waitFor("the failed check in the view", (text) => text.includes(FAILED));
        await me.capture("roadmap-status-after-landing-ran");

        await pick("Refresh only", "refresh_only");
        await land();
        await pause(CHECK_MS / 4);
        assert.equal(stored(runtime, project).stale, 0, "under Refresh only a landing asks for no check");
        await open("overview");
        await me.click("Activity", "[role=tab]");
        await me.waitFor("the roadmap status under Refresh only", (text) => text.includes("Roadmap status") && !text.includes(DUE_LINE));
        const before = log.trail.filter((call) => call.cmd === "desktop_refresh_roadmap_status").length;
        await me.click("Refresh", "section[aria-labelledby=pj-roadmap-status] button");
        for (const deadline = Date.now() + STEP_MS; log.trail.filter((call) => call.cmd === "desktop_refresh_roadmap_status").length === before; await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, "the Refresh button never reached the bridge");
        }
        await until(runtime, project, "the Refresh check never ran", (now) => now.stale === 0 && now.running === null && now.problem !== null, CHECK_MS);
        await me.capture("roadmap-status-refresh-only");

        await pick("Off", "off");
        await land();
        await pause(CHECK_MS / 4);
        assert.equal(stored(runtime, project).stale, 0, "under Off a landing asks for no check");
        await open("overview");
        await me.click("Activity", "[role=tab]");
        await me.waitFor("the line saying roadmap status is off", (text) => text.includes(OFF_LINE));
        assert.equal(await refreshOff(), true, "Refresh is disabled under Off");
        assert.equal((await value(bridge, "desktop_roadmap_status", { id: project })).refresh, "off", "the engine reports the Off setting");
        await me.capture("roadmap-status-off");
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
    provider.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
