import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";
import { SUB_LABEL, engineMasterWorktree, masterConflictFix } from "./master-lease.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const MASTER = "master-1";
const MASTER_LABEL = "Master";
const PROMPT = "Fix the seed service conflict";
const SETTING = "When the master resolves conflicts";
const MERGES = "Merge into main itself";
const WAITS = "Wait for my Accept";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const README = "# Seed service\n\nServes seeds over HTTP.\n";

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Makes the owner's project: a git repository on main with one commit and the owner's Git
 * identity stored, so a merge into main lands. Input: the folder. Output: the commit. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Seed service\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "add", ".");
  git(folder, ...OWNER, "commit", "-q", "-m", "Start the seed service");
  git(folder, "config", "user.name", "E2E Owner");
  git(folder, "config", "user.email", "owner@example.invalid");
  return git(folder, "rev-parse", "HEAD");
}

/** Boots a test engine and the app's bridge, makes the owner's project, optionally at Full
 * access, has the engine lease the master's worktree through one master turn, serves the
 * built app and runs the case's steps on a page. Inputs: the test, the access level
 * (sandboxed or full) and the steps, called with the case's handles. Output: none; every
 * leftover is removed unless E2E_EVIDENCE keeps it. */
async function withProject(t, access, steps) {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-ml-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? join(process.env.E2E_EVIDENCE, access) : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "seed-service");
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
    assert.equal((await value(bridge, "desktop_open_project", { id: project })).project_id, project, "the new project opens");
    const policy = await value(bridge, "desktop_project_agent_policy", { id: project });
    if (policy.access !== access) {
      const saved = await value(bridge, "desktop_set_project_agent_policy", {
        id: project, version: policy.version, generation: 4,
        policy: { access, network: policy.network, toolchains: [], ide_git_hooks: policy.ide_git_hooks },
      });
      assert.equal(saved.outcome, "applied", `the owner sets the project's access to ${access}: ${JSON.stringify(saved)}`);
    }
    assert.equal((await value(bridge, "desktop_project_agent_policy", { id: project })).access, access, `the project runs at ${access}`);
    let generation = 4;
    const lease = await engineMasterWorktree(bridge, runtime, project, () => (generation += 1));

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
        const open = async (tab, words) => {
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/${tab}` });
          await me.waitFor(words, (text) => text.includes(words));
        };
        const masterTurn = (text) => {
          if (text !== null) writeFileSync(join(lease.worktree, "README.md"), text);
          const made = spawnSync(bridgeBinary, ["master-turn-end", runtime, project, MASTER, MASTER_LABEL, repo, lease.worktree, lease.gitDir, PROMPT], { encoding: "utf8", env: { ...env } });
          assert.equal(made.status, 0, `the engine ends the master's turn: ${made.stderr}`);
          const [id, how] = made.stdout.trim().split(" ");
          assert.match(id, /^[0-9a-f]{32}$/, "an edited master worktree makes a change set");
          return { id, how };
        };
        const changes = () => value(bridge, "desktop_changes", { projectId: project });
        const masterFix = () => masterConflictFix({ bridgeBinary, env, runtime, project, root, repo, start, lease, changes, masterTurn });
        const status = () => value(bridge, "desktop_team_status", { id: project });
        const activity = async () => (await value(bridge, "desktop_project_activity", { id: project, limit: 50 })).items;
        await steps({ me, page, open, masterTurn, masterFix, changes, status, activity, repo, start });
        assert.equal(await page.evaluate("document.visibilityState"), "visible", "the page stayed visible");
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

/** Clicks one choice of the master merge setting on the Agents tab, as the owner does, and
 * waits until the engine keeps it and the tab shows it picked. Inputs: the case's handles,
 * the choice's label and the engine's name for it. Output: none. */
async function choose({ me, page, status }, label, mode) {
  await me.click(label, `[role=radiogroup][aria-label="${SETTING}"] [role=radio]`);
  const picked = `document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"] [role=radio][aria-checked=true]`)})?.innerText.trim() ?? null`;
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).master_lands;
    if (held === mode && (await page.evaluate(picked)) === label) return;
    assert.ok(Date.now() < deadline, `the engine never kept ${mode}; it holds ${JSON.stringify(held)}`);
  }
}

/** Says whether a change set waits for the owner's Accept in Changes. Inputs: the listed
 * change sets and the set's id. Output: true when it is listed pending. */
function waiting(sets, id) {
  return sets.some((set) => set.id === id && set.state === "pending");
}

test("below Full access the master merge setting is off, the master's edit waits for the owner's Accept and its merge of another agent's set is refused", { timeout: 600000 }, async (t) => {
  await withProject(t, "sandboxed", async (run) => {
    const { me, page, open, masterTurn, masterFix, changes, status, activity, repo } = run;
    await open("agents", SETTING);
    const row = await page.evaluate(`[...document.querySelectorAll(".pj-row")].find((node) => node.textContent.includes(${JSON.stringify(SETTING)}))?.innerText ?? ""`);
    assert.match(row, /Only at Full access\./, "the row says the setting needs Full access");
    assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"]`)})`), null, "no choice is offered below Full access");
    assert.equal((await status()).master_lands, null, "the engine offers no choice below Full access");
    await me.capture("sandboxed-agents");

    const plain = masterTurn(README);
    assert.equal(plain.how, "waits", "below Full access the master's edit waits for the owner's Accept");
    const before = (await changes()).find((set) => set.id === plain.id);
    assert.equal(before?.state, "pending", "the master's edit waits in Changes");
    await assert.rejects(masterFix(), /changes_outside_merge/, "below Full access the engine refuses the master's merge of another agent's set (DEC-117 Q2)");
    assert.equal(git(repo, "log", "-1", "--format=%s"), "Plant the pear seed", "main holds the owner's commit only");
    const after = (await changes()).find((set) => set.id === plain.id);
    assert.deepEqual([after?.state, after?.head], ["pending", before.head], "the master's set is unchanged");
    assert.deepEqual((await activity()).filter((item) => item.change_set === plain.id), [], "nothing was merged, so no activity row");
  });
});

test("at Full access Merge into main itself lands the master's conflict fix on main with no Accept and Activity lists it, while an ordinary master edit waits", { timeout: 600000 }, async (t) => {
  await withProject(t, "full", async (run) => {
    const { me, open, masterTurn, masterFix, changes, status, activity, repo, start } = run;
    await open("agents", SETTING);
    assert.equal((await status()).master_lands, "merges", "Full access starts with Merge into main itself");
    await choose(run, WAITS, "waits");
    await choose(run, MERGES, "merges");
    await me.capture("full-merges-agents");

    const plain = masterTurn(README);
    assert.equal(plain.how, "waits", "an edit that fixes no conflict waits for the owner's Accept");
    assert.equal(git(repo, "rev-parse", "HEAD"), start, "main did not move");
    assert.ok(waiting(await changes(), plain.id), "the master's ordinary edit waits in Changes");
    assert.deepEqual((await activity()).filter((item) => item.change_set === plain.id), [], "nothing was merged, so no activity row");

    const { id, how, sub, tip } = await masterFix();
    assert.equal(how, "landed", "the master merged its own conflict fix");
    assert.notEqual(git(repo, "rev-parse", "HEAD"), tip, "main moved");
    assert.equal(git(repo, "show", "HEAD:seeds.txt"), "pear\napple", "the fix is on main");
    const sets = await changes();
    assert.ok(!waiting(sets, id), "the master's fix does not wait in Changes");
    assert.equal(sets.find((set) => set.id === sub)?.state, "accepted", "the conflicted sub-agent set closes with the fix");
    assert.deepEqual(sets.filter((set) => set.state === "pending").map((set) => set.id), [], "Changes stays empty");

    const rows = (await activity()).filter((item) => item.change_set === id);
    assert.deepEqual(rows.map((item) => [item.kind, item.actor, item.text]), [["merged_into_main", "Master", `The master merged its changes into main: ${PROMPT}. It closed the changes of ${SUB_LABEL}`]],
      "the project's activity, which Overview reads, holds one merge row by the master and no Accept by the owner");
  });
});

test("at Full access Wait for my Accept leaves the master's fix in Changes", { timeout: 600000 }, async (t) => {
  await withProject(t, "full", async (run) => {
    const { me, open, masterFix, changes, activity, repo } = run;
    await open("agents", SETTING);
    await choose(run, WAITS, "waits");
    await me.capture("full-waits-agents");

    const { id, how, tip } = await masterFix();
    assert.equal(how, "waits", "under Wait for my Accept the master does not merge");
    assert.equal(git(repo, "rev-parse", "HEAD"), tip, "main did not move");
    assert.ok(waiting(await changes(), id), "the master's fix waits in Changes");
    assert.deepEqual((await activity()).filter((item) => item.change_set === id), [], "nothing was merged, so no activity row");
  });
});
