import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, isAbsolute, join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";
import { git } from "./trial4-projects.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SQLITE = "/usr/bin/sqlite3";
const MOVE_MS = 120000;
const ROW = "Move into CrazyAgent folder";
const WAYS = '[role=radiogroup][aria-label="Move or copy"] button';
const SAID = "The team stops first; work not landed yet stays in the agents' copies, which follow the move.";
const run = { generation: 10, homes: 0 };

/** Sets up one project in the current projects folder, then picks a new, empty projects folder,
 * so the project is one kept outside it. Input: its name. Output: its id, its folder and the new
 * projects folder. */
async function keptOutside(name) {
  const receipt = await value(run.bridge, "desktop_setup_project", {
    spec: { name, repositories: [] },
    setup: { about: `${name} for the move dialog`, tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [] },
    generation: run.generation++,
  });
  assert.equal(receipt.outcome, "applied", `${name} is made: ${JSON.stringify(receipt)}`);
  const folder = await projectFolder(receipt.project_id);
  const home = join(run.root, `home-${++run.homes}`);
  mkdirSync(home, { mode: 0o700 });
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: home, generation: run.generation++ })).outcome, "applied");
  assert.ok(!folder.startsWith(`${home}/`), `${folder} is outside ${home}`);
  return { id: receipt.project_id, folder, home };
}

/** Reads a project's folder from its saved setup. Input: the project id. Output: the folder. */
async function projectFolder(id) {
  const setup = await value(run.bridge, "desktop_project_setup", { id });
  return setup.folder.endsWith("/.crazyagent") ? setup.folder.slice(0, -"/.crazyagent".length) : setup.folder;
}

/** Opens a project's move dialog from its Settings as the owner does. Inputs: the page, its
 * helpers, the project and its name. Output: none. */
async function openMove(page, me, id, name) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/projects/${encodeURIComponent(id)}/settings?section=remove` });
  await me.waitFor(`${name}'s move row`, (text) => text.includes(ROW) && text.includes("Remove project"));
  await me.click(ROW, "button");
  await me.waitFor("the move dialog", (text) => text.includes(SAID) && text.includes("Stop the team and move"));
}

/** Runs one case in a fresh page against the shared engine, keeping a failure capture.
 * Inputs: the evidence label and the steps. Output: none. */
async function onPage(label, steps) {
  await withPage(async (page) => {
    const me = owner(page, run.evidence);
    try {
      await page.send("Page.enable");
      await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
      await steps(page, me);
      assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
    } catch (failure) {
      await me.capture(`${label}-failure`).catch(() => {});
      throw failure;
    }
  });
}

/** Reads one value from the test engine's database. Input: the query. Output: its text. */
function sql(query) {
  return execFileSync(SQLITE, [run.database, query], { encoding: "utf8" }).trim();
}

before(async () => {
  run.engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  run.bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  run.root = realpathSync(mkdtempSync(join(tmpdir(), "ca-mv-")));
  run.evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(run.root, "evidence");
  const runtime = join(run.root, "engine");
  const home = join(run.root, "home");
  const first = join(run.root, "kept");
  for (const folder of [runtime, run.evidence, home, join(home, ".codex"), first]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  run.first = first;
  const provider = await closedPort();
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "e2e-model" }, credential = { kind = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  run.database = join(runtime, "engine.db");
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  run.log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  ({ child: run.engine } = await launch(run.engineBinary,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
    { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await launch(run.bridgeBinary, [join(runtime, "engine.sock")], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = started.child;
  run.bridge = { port: Number(started.match[1]), token: started.match[2] };
  assert.equal((await value(run.bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
  assert.equal((await value(run.bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
  const dist = join(run.root, "dist");
  await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
  assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
  run.site = await serveApp(dist, run.bridge, [], run.log);
});

after(async () => {
  run.log && (run.log.closing = true);
  if (run.evidence && run.log) writeFileSync(join(run.evidence, "calls.json"), JSON.stringify(run.log.calls, null, 1));
  run.site?.server.close();
  await stop(run.bridgeProcess);
  await stop(run.engine);
  if (run.root && !process.env.E2E_EVIDENCE) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});

test("Move takes a project kept outside the projects folder into it, stops the team first and hides the row after", { timeout: 300000 }, async () => {
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: run.first, generation: run.generation++ })).outcome, "applied");
  const name = "Move Me";
  const { id, folder, home } = await keptOutside(name);
  writeFileSync(join(folder, "notes.md"), "moved with the project\n");
  const target = join(home, basename(folder));
  await onPage("move", async (page, me) => {
    await openMove(page, me, id, name);
    await me.capture("move-dialog");
    await me.click("Stop the team and move", "dialog[open] button");
    await me.waitFor("the moved line", (text) => text.includes(`${name} is now in your CrazyAgent folder`), MOVE_MS);
    await me.waitFor("Settings without the move row", (text) => text.includes("Remove project") && !text.includes(ROW));
    await me.capture("move-done");
  });
  assert.ok(!existsSync(folder), "the old folder is gone");
  assert.equal(readFileSync(join(target, "notes.md"), "utf8"), "moved with the project\n");
  assert.equal(await projectFolder(id), target, "the project's setup names the folder in the projects folder");
  assert.equal(sql(`SELECT COUNT(*) FROM team_paused_projects WHERE project='${id}'`), "1", "the team was stopped before the move");
  assert.equal(sql("SELECT COUNT(*) FROM projects_home_project_move"), "0", "no move stays pending");
  const trail = run.log.trail.filter((entry) => entry.cmd === "desktop_team_pause" || entry.cmd === "desktop_move_project_home").map((entry) => entry.cmd);
  assert.deepEqual(trail.slice(-2), ["desktop_team_pause", "desktop_move_project_home"], "the stop is sent before the move");
});

test("Copy leaves the old folder whole and the project uses the copy", { timeout: 300000 }, async () => {
  const name = "Copy Me";
  const { id, folder, home } = await keptOutside(name);
  writeFileSync(join(folder, "notes.md"), "copied with the project\n");
  const target = join(home, basename(folder));
  await onPage("copy", async (page, me) => {
    await openMove(page, me, id, name);
    await me.click("Copy", WAYS);
    await me.waitFor("the copy wording", (text) => text.includes("Stop the team and copy") && text.includes("The copy becomes the project."));
    await me.capture("copy-dialog");
    await me.click("Stop the team and copy", "dialog[open] button");
    await me.waitFor("the copied line", (text) => text.includes(`${name} now uses the copy in your CrazyAgent folder`), MOVE_MS);
  });
  assert.equal(readFileSync(join(folder, "notes.md"), "utf8"), "copied with the project\n", "the old folder stays whole");
  assert.equal(readFileSync(join(target, "notes.md"), "utf8"), "copied with the project\n");
  assert.equal(await projectFolder(id), target, "the project's setup names the copy");
});

test("an agent's commit that has not landed is still in its copy after the project moves", { timeout: 300000 }, async () => {
  const name = "Unlanded Work";
  const { id, folder, home } = await keptOutside(name);
  git(folder, ["init", "-b", "main"]);
  writeFileSync(join(folder, "README.md"), "# Unlanded Work\n");
  git(folder, ["add", "README.md"]);
  git(folder, ["-c", "user.name=E2E", "-c", "user.email=e2e@example.invalid", "commit", "-m", "first"]);
  const first = git(folder, ["rev-parse", "HEAD"]).trim();
  const worktree = join(folder, ".crazyagent", "worktrees", "agent-71");
  mkdirSync(join(folder, ".crazyagent", "worktrees"), { recursive: true, mode: 0o700 });
  git(folder, ["clone", "--shared", "--no-hardlinks", folder, worktree]);
  assert.equal(readFileSync(join(worktree, ".git", "objects", "info", "alternates"), "utf8"), `${folder}/.git/objects\n`, "the stand-in copy borrows the project's objects as an agent clone does");
  writeFileSync(join(worktree, "page.txt"), "not landed yet\n");
  git(worktree, ["add", "page.txt"]);
  git(worktree, ["-c", "user.name=E2E", "-c", "user.email=e2e@example.invalid", "commit", "-m", "unlanded work"]);
  const head = git(worktree, ["rev-parse", "HEAD"]).trim();
  sql(`INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms) VALUES (71,'${id}','team','codex',1,'${worktree}','stopped',0,0);
       INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (71,'${id}','${folder}','${worktree}','${worktree}/.git');`);
  const target = join(home, basename(folder));
  const moved = join(target, ".crazyagent", "worktrees", "agent-71");
  await onPage("unlanded", async (page, me) => {
    await openMove(page, me, id, name);
    await me.click("Stop the team and move", "dialog[open] button");
    await me.waitFor("the moved line", (text) => text.includes(`${name} is now in your CrazyAgent folder`), MOVE_MS);
  });
  assert.ok(!existsSync(folder), "the old folder is gone");
  assert.equal(git(moved, ["rev-parse", "HEAD"]).trim(), head, "the agent's commit is still the head of its copy");
  assert.equal(git(moved, ["rev-parse", "HEAD~1"]).trim(), first, "its parent is read from the moved project's objects");
  assert.equal(readFileSync(join(moved, ".git", "objects", "info", "alternates"), "utf8"), `${target}/.git/objects\n`, "the copy borrows the moved project's objects");
  assert.equal(sql(`SELECT worktree FROM team_worktrees WHERE agent=71`), moved, "the engine records the copy at its new place");
});
