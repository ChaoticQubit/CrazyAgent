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
import { INTERNALS, POLL_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";
import { SUB_LABEL, engineMasterWorktree, masterConflictFix } from "./master-lease.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SHOWN_MS = 10000;
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const MASTER = "master-1";
const MASTER_LABEL = "Master";
const PROMPT = "Fix the seed service conflict";
const escaped = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];

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
 * access, where the master merges its own conflict fixes by default, has the engine lease
 * the master's worktree through one master turn, serves the built app and runs the case's
 * steps on a page. Inputs: the test, the access level (sandboxed or full) and the steps,
 * called with the case's handles. Output: none; every leftover is removed unless
 * E2E_EVIDENCE keeps it. */
async function withProject(t, access, steps) {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-um-")));
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
        await steps({ me, page, open, masterFix, changes, status, activity, repo });
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

/** Reads the Overview activity row that names a merge. Inputs: the page and the row's words.
 * Output: { text, button }, the row's text and its button's label, or null when no row has
 * the words. */
function mergeRow(page, words) {
  return page.evaluate(`(() => { const row = [...document.querySelectorAll(".pj-activity-row")].find((node) => node.innerText.includes(${JSON.stringify(words)})); return row ? { text: row.querySelector(".pj-activity-text")?.innerText ?? "", button: row.querySelector("button")?.textContent ?? null } : null; })()`);
}

test("Undo on Overview asks naming the branch, reverts the master's merge into main and the row shows Undone", { timeout: 600000 }, async (t) => {
  await withProject(t, "full", async ({ me, page, open, masterFix, changes, status, activity, repo }) => {
    assert.equal((await status()).master_lands, "merges", "at Full access the master merges its own conflict fixes");
    const { id, how, tip } = await masterFix();
    assert.equal(how, "landed", "the master merged its conflict fix into main");
    const merged = git(repo, "rev-parse", "HEAD");
    assert.notEqual(merged, tip, "main moved");
    assert.ok(!(await changes()).some((set) => set.id === id && set.state === "pending"), "nothing waits in Changes");

    const words = "Master merged change set #";
    await open("overview", words);
    assert.equal((await mergeRow(page, words))?.button, "Undo", "the master's merge row offers Undo");
    assert.match((await mergeRow(page, words))?.text ?? "", new RegExp(`^Master merged change set #\\d+ \\(Master: ${escaped(PROMPT)}, \\d+ files?\\) into main`),
      "the merge row names the master's set by its number");
    await me.capture("undo-before");
    await me.click("Undo", ".pj-activity-row button");
    await me.waitFor("the confirm naming the branch", (text) => text.includes("Undo this merge on main?") && text.includes("Later commits stay."));
    await me.capture("undo-confirm");
    await me.click("Undo merge", "dialog[open] button");
    const clicked = Date.now();
    for (;;) {
      const row = await mergeRow(page, words);
      if (row?.text.includes("Undone") && row.button === null) break;
      assert.ok(Date.now() - clicked < SHOWN_MS, `the merge row never showed Undone within ${SHOWN_MS} ms; it shows: ${JSON.stringify(row)}`);
      await pause(POLL_MS);
    }
    t.diagnostic(`the row showed Undone after ${Date.now() - clicked} ms`);
    await me.waitFor("the undo note", (text) => text.includes("Undid the merge with one new commit on main. Later commits stay."));
    await me.capture("undo-after");

    assert.equal(git(repo, "show", "HEAD:seeds.txt"), "pear", "main is back as it was before the merge");
    assert.equal(readFileSync(join(repo, "seeds.txt"), "utf8"), "pear\n", "the owner's files are back too");
    assert.equal(git(repo, "rev-parse", "HEAD^"), merged, "the undo is one commit on top of the merge");
    assert.equal(git(repo, "log", "-1", "--format=%s"), `Undo: ${PROMPT}`);
    assert.equal(git(repo, "status", "--porcelain"), "", "the checkout is clean");
    const rows = (await activity()).filter((item) => item.change_set === id);
    assert.deepEqual(rows.map((item) => [item.kind, item.undone]), [["undone", false], ["merged_into_main", true]], "one Undone row and the merge marked undone");
    assert.ok(rows[0].text.endsWith(`This also reverted the changes of ${SUB_LABEL}`), `the Undone row says the closed set's work was reverted too: ${rows[0].text}`);
  });
});
