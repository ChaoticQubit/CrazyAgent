import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statfsSync, statSync, writeFileSync } from "node:fs";
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
const DISK_MS = 120000;
const FREE_BYTES_MIN = 2 * 1024 ** 3;
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

/** Opens Home in the page, where the engine's job is followed. Inputs: the page. Output: none. */
async function openHome(page) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/` });
}

/** Fills a folder with a tree of small files, so a copy of it takes some seconds. Inputs: the
 * folder, the part folders and the files in each. Output: none. */
function tree(folder, parts, files) {
  for (let part = 0; part < parts; part++) {
    mkdirSync(join(folder, `p${part}`), { recursive: true });
    for (let file = 0; file < files; file++) writeFileSync(join(folder, `p${part}`, `f${file}.txt`), `${part} ${file}\n`);
  }
}

/** Starts the engine's move job for a project through the bridge, as the dialog does once the team
 * stopped. Inputs: the project id and move or copy. Output: the started job. */
async function startMove(id, mode) {
  const job = await value(run.bridge, "desktop_move_project_home", { id, mode, overrides: [] });
  assert.equal(job.kind, "move_project", JSON.stringify(job));
  assert.equal(job.project_id, id, JSON.stringify(job));
  return job;
}

/** Stops the bridge and the engine, runs `between` while neither runs, then starts both again on
 * the same data folder, as quitting and opening the app does. Input: what to do in between.
 * Output: none. */
async function restart(between) {
  run.starts = (run.starts ?? 0) + 1;
  await stop(run.bridgeProcess);
  await stop(run.engine);
  between();
  ({ child: run.engine } = await launch(run.engineBinary, run.engineArgs, { ...run.env }, run.evidence, `engine-${run.starts}`, /^ready$/m));
  const again = await launch(run.bridgeBinary, [run.socket], { ...run.env }, run.evidence, `bridge-${run.starts}`, /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = again.child;
  run.bridge.port = Number(again.match[1]);
  run.bridge.token = again.match[2];
  assert.equal((await value(run.bridge, "desktop_connect")).connected, true, "the bridge connects to the engine started again");
}

/** Whether a folder is a mount point now. Input: the mount folder. Output: the answer. */
function mounted(mount) {
  return execFileSync("/sbin/mount", { encoding: "utf8" }).split("\n").some((line) => line.includes(` on ${mount} (`));
}

/** Makes and attaches a small sparse APFS disk image under the test's temp root, unseen in Finder,
 * so a project on it sits on another disk than the projects folder. Input: the temp root. Output:
 * the image file and its mount point; throws when free space is short. */
function attachDisk(root) {
  const { bavail, bsize } = statfsSync(root);
  const free = Number(bavail) * Number(bsize);
  assert.ok(free >= FREE_BYTES_MIN, `PREREQUISITE: the move across disks needs ${FREE_BYTES_MIN} bytes free on the disk holding ${root}, which has ${free}`);
  const image = join(root, "other-disk.sparseimage");
  const mount = join(root, "other-disk");
  mkdirSync(mount);
  const made = spawnSync("hdiutil", ["create", "-quiet", "-size", "1g", "-type", "SPARSE", "-fs", "APFS", "-volname", "CA-E2E-MOVE", image], { encoding: "utf8", timeout: DISK_MS });
  assert.equal(made.status, 0, `hdiutil create ${image}: ${made.stderr}`);
  const attached = spawnSync("hdiutil", ["attach", "-quiet", "-nobrowse", "-noautoopen", "-mountpoint", mount, image], { encoding: "utf8", timeout: DISK_MS });
  assert.equal(attached.status, 0, `hdiutil attach ${image}: ${attached.stderr}`);
  const disk = { image, mount: realpathSync(mount) };
  assert.ok(mounted(disk.mount), `${disk.mount} is not mounted after attach`);
  run.disk = disk;
  return disk;
}

/** Detaches the scratch disk, with -force when a plain detach fails, then deletes its image.
 * Input: the disk. Output: what is left behind, none when the mount is gone and the image deleted. */
function releaseDisk(disk) {
  if (mounted(disk.mount)) {
    const plain = spawnSync("hdiutil", ["detach", "-quiet", disk.mount], { encoding: "utf8", timeout: DISK_MS });
    if (plain.status !== 0) spawnSync("hdiutil", ["detach", "-quiet", "-force", disk.mount], { encoding: "utf8", timeout: DISK_MS });
  }
  rmSync(disk.image, { force: true });
  const left = [];
  if (mounted(disk.mount)) left.push(`${disk.mount} is still mounted`);
  if (existsSync(disk.image)) left.push(`${disk.image} is still on disk`);
  if (left.length === 0) run.disk = null;
  return left;
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
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "e2e-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
  run.database = join(runtime, "engine.db");
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  run.env = env;
  run.engineArgs = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"];
  run.socket = join(runtime, "engine.sock");
  run.log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  ({ child: run.engine } = await launch(run.engineBinary, run.engineArgs, { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await launch(run.bridgeBinary, [run.socket], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
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
  if (run.disk) assert.deepEqual(releaseDisk(run.disk), [], "the scratch disk is gone");
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
    await me.waitFor("the started line", (text) => text.includes("Follow it on Home; you can keep working."));
    await me.waitFor("the moved line", (text) => text.includes(`${basename(folder)} is now in your projects folder`), MOVE_MS);
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
    await me.waitFor("the copied line", (text) => text.includes(`${basename(folder)} is now in your projects folder`), MOVE_MS);
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
    await me.waitFor("the moved line", (text) => text.includes(`${basename(folder)} is now in your projects folder`), MOVE_MS);
  });
  assert.ok(!existsSync(folder), "the old folder is gone");
  assert.equal(git(moved, ["rev-parse", "HEAD"]).trim(), head, "the agent's commit is still the head of its copy");
  assert.equal(git(moved, ["rev-parse", "HEAD~1"]).trim(), first, "its parent is read from the moved project's objects");
  assert.equal(readFileSync(join(moved, ".git", "objects", "info", "alternates"), "utf8"), `${target}/.git/objects\n`, "the copy borrows the moved project's objects");
  assert.equal(sql(`SELECT worktree FROM team_worktrees WHERE agent=71`), moved, "the engine records the copy at its new place");
});

test("a move runs on Home as the engine's job, and Stop there leaves the project where it was (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Stop Me";
  const { id, folder, home } = await keptOutside(name);
  for (let part = 0; part < 200; part++) {
    mkdirSync(join(folder, `p${part}`), { recursive: true });
    for (let file = 0; file < 200; file++) writeFileSync(join(folder, `p${part}`, `f${file}.txt`), `${part} ${file}\n`);
  }
  const target = join(home, basename(folder));
  await onPage("stop", async (page, me) => {
    await openMove(page, me, id, name);
    await me.click("Copy", WAYS);
    await me.click("Stop the team and copy", "dialog[open] button");
    await me.waitFor("the started line", (text) => text.includes("Follow it on Home; you can keep working."));
    await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/` });
    await me.waitFor("the job row copying", (text) => text.includes(`Copying ${basename(folder)}`), MOVE_MS);
    await me.capture("move-job-running");
    await me.click("Stop", ".in-row button");
    await me.waitFor("the stopped line", (text) => text.includes(`Moving ${basename(folder)} stopped. Nothing was moved or copied.`), MOVE_MS);
    await me.capture("move-job-stopped");
  });
  assert.ok(!existsSync(target), "nothing was copied into the projects folder");
  assert.equal(readFileSync(join(folder, "p199", "f199.txt"), "utf8"), "199 199\n", "the folder stays whole");
  assert.equal(await projectFolder(id), folder, "the project still names its own folder");
  assert.equal(sql("SELECT COUNT(*) FROM projects_home_project_move"), "0", "no move stays pending");
  const starts = run.log.trail.filter((entry) => entry.cmd === "desktop_move_project_home");
  assert.ok(starts.length > 0, "the dialog started the job");
});

test("a move whose name is taken in the projects folder fails on Home with the reason, offers Try again and leaves the project where it was (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Taken Name";
  const { id, folder, home } = await keptOutside(name);
  const target = join(home, basename(folder));
  mkdirSync(target, { mode: 0o700 });
  writeFileSync(join(target, "other.txt"), "another folder's file\n");
  await onPage("failed", async (page, me) => {
    await openMove(page, me, id, name);
    await me.click("Stop the team and move", "dialog[open] button");
    await me.waitFor("the started line", (text) => text.includes("Follow it on Home; you can keep working."));
    await openHome(page);
    await me.waitFor("the failed line", (text) => text.includes(`${basename(folder)}: Your projects folder already has a folder with this name. Rename one of them first.`), MOVE_MS);
    await me.waitFor("Try again and Dismiss", (text) => text.includes("Try again") && text.includes("Dismiss"));
    await me.capture("move-job-failed");
    await me.click("Dismiss", ".in-row button");
    await me.waitFor("the row gone", (text) => !text.includes("Try again"), MOVE_MS);
  });
  assert.equal(await projectFolder(id), folder, "the project still names its own folder");
  assert.equal(readFileSync(join(target, "other.txt"), "utf8"), "another folder's file\n", "the folder already there is untouched");
  assert.equal(sql("SELECT COUNT(*) FROM projects_home_project_move"), "0", "no move stays pending");
});

test("a move of a project with a linked worktree outside it waits on Home for the owner's answer, and Move anyway there moves it (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Linked Work";
  const { id, folder, home } = await keptOutside(name);
  git(folder, ["init", "-b", "main"]);
  writeFileSync(join(folder, "README.md"), "# Linked Work\n");
  git(folder, ["add", "README.md"]);
  git(folder, ["-c", "user.name=E2E", "-c", "user.email=e2e@example.invalid", "commit", "-m", "first"]);
  git(folder, ["worktree", "add", "-b", "side", join(run.root, `${basename(folder)}-side`)]);
  const target = join(home, basename(folder));
  await startMove(id, "move");
  await onPage("answer", async (page, me) => {
    await openHome(page);
    await me.waitFor("the question line", (text) => text.includes(`${basename(folder)} needs your answer before it is moved.`), MOVE_MS);
    await me.click("Answer", ".in-row button");
    await me.waitFor("the question", (text) => text.includes("Keep it here") && text.includes("Move anyway"), MOVE_MS);
    const asked = await page.evaluate("document.querySelector('dialog[open]')?.innerText ?? ''");
    assert.ok(!asked.includes("Use where it is"), `a set up project is never set up again where it is: ${asked}`);
    await me.capture("move-job-question");
    await me.click("Move anyway", "dialog[open] button");
    await me.waitFor("the moved line", (text) => text.includes(`${basename(folder)} is now in your projects folder`), MOVE_MS);
  });
  assert.ok(!existsSync(folder), "the old folder is gone");
  assert.equal(await projectFolder(id), target, "the project's setup names the folder in the projects folder");
  const starts = run.log.trail.filter((entry) => entry.cmd === "desktop_move_project_home");
  assert.ok(starts.length > 0, "the answer started the move again from the app");
});

test("a move waits on Home for the project's agents to stop, says how many are left, and moves once they stopped (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Slow Stop";
  const { id, folder, home } = await keptOutside(name);
  writeFileSync(join(folder, "notes.md"), "kept\n");
  const target = join(home, basename(folder));
  sql(`INSERT OR IGNORE INTO team_paused_projects(project,since_ms) VALUES ('${id}',1);
       INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms) VALUES (72,'${id}','team','codex',1,'${join(run.root, "agent-72")}','working',0,0);`);
  await startMove(id, "move");
  await onPage("stopping", async (page, me) => {
    await openHome(page);
    await me.waitFor("the waiting line", (text) => text.includes(`Waiting for 1 agent of ${basename(folder)} to stop...`), MOVE_MS);
    await me.capture("move-job-stopping");
    assert.equal(readFileSync(join(folder, "notes.md"), "utf8"), "kept\n", "nothing moves while it waits");
    assert.ok(!existsSync(target), "nothing is in the projects folder while it waits");
    sql("UPDATE team_agents SET status='stopped' WHERE id=72");
    await me.waitFor("the moved line", (text) => text.includes(`${basename(folder)} is now in your projects folder`), MOVE_MS);
  });
  assert.ok(!existsSync(folder), "the old folder is gone");
  assert.equal(readFileSync(join(target, "notes.md"), "utf8"), "kept\n");
});

test("one job at a time: another project's move is refused naming the running move, the same project's start finds it, and a new window shows it on Home (DEC-177)", { timeout: 300000 }, async () => {
  const first = await keptOutside("Busy First");
  tree(first.folder, 200, 200);
  const second = await keptOutside("Busy Second");
  const running = await startMove(first.id, "copy");
  assert.ok(["downloading", "stopping", "copying"].includes(running.state.state), JSON.stringify(running));
  const again = await startMove(first.id, "copy");
  assert.equal(again.id, running.id, "the same project's start finds the running move");
  const moving = basename(first.folder);
  await onPage("busy", async (page, me) => {
    await openMove(page, me, second.id, "Busy Second");
    await me.click("Stop the team and move", "dialog[open] button");
    await me.waitFor("the busy line", (text) => text.includes(`Moving ${moving} is running. Wait for it to finish on Home.`), MOVE_MS);
    await me.capture("move-job-busy");
  });
  await onPage("reopened", async (page, me) => {
    await openHome(page);
    await me.waitFor("the running move", (text) => text.includes(`Copying ${moving}`) || text.includes(`Checking the copy of ${moving}`), MOVE_MS);
    await me.capture("move-job-reopened");
    await me.click("Stop", ".in-row button");
    await me.waitFor("the stopped line", (text) => text.includes(`Moving ${moving} stopped. Nothing was moved or copied.`), MOVE_MS);
  });
  assert.equal(await projectFolder(first.id), first.folder, "the stopped move leaves its project where it was");
  assert.equal(await projectFolder(second.id), second.folder, "the refused move never started");
  assert.ok(!existsSync(join(second.home, basename(second.folder))), "nothing of the refused project reached the projects folder");
});

test("a move an engine stop cut short shows on Home after the next start, and Dismiss forgets it across another start (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Cut Short";
  const { id, folder, home } = await keptOutside(name);
  const target = join(home, basename(folder));
  await restart(() => sql(`INSERT INTO projects_home_project_move VALUES (1, 'move-cut-e2e', 7, '${id}', '${folder}', '${target}', 'move');`));
  const cut = `${basename(folder)}: The move stopped part way. Check the project folder before trying again.`;
  await onPage("interrupted", async (page, me) => {
    await openHome(page);
    await me.waitFor("the cut-short line", (text) => text.includes(cut), MOVE_MS);
    await me.capture("move-job-interrupted");
    await me.click("Dismiss", ".in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(cut), MOVE_MS);
  });
  assert.equal(sql("SELECT COUNT(*) FROM projects_home_project_move_ended"), "0", "Dismiss forgets the ended move");
  await restart(() => {});
  await onPage("dismissed", async (page, me) => {
    await openHome(page);
    await me.waitFor("Home with the project", (text) => text.includes(name), MOVE_MS);
    assert.ok(!(await page.evaluate("document.body.innerText")).includes(cut), "a dismissed move stays gone after another start");
  });
  assert.equal(await projectFolder(id), folder, "the project still names its own folder");
  assert.equal(sql("SELECT COUNT(*) FROM projects_home_project_move"), "0", "no move stays pending");
});

test("a move from another disk copies, then removes the old folder with no Stop while it finishes (DEC-177)", { timeout: 900000 }, async () => {
  const disk = attachDisk(run.root);
  const away = join(disk.mount, "projects");
  mkdirSync(away, { mode: 0o700 });
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: away, generation: run.generation++ })).outcome, "applied");
  const name = "Other Disk";
  const { id, folder, home } = await keptOutside(name);
  assert.ok(folder.startsWith(`${disk.mount}/`), `${folder} is on the scratch disk`);
  assert.notEqual(statSync(folder).dev, statSync(home).dev, "the project sits on another disk than the projects folder");
  tree(folder, 200, 100);
  const target = join(home, basename(folder));
  await startMove(id, "move");
  await onPage("clearing", async (page, me) => {
    await openHome(page);
    await me.waitFor("the finishing line", (text) => text.includes(`Removing ${basename(folder)} from its old place...`), MOVE_MS);
    const offered = await page.evaluate(`[...document.querySelectorAll(".in-row button")].map((button) => button.innerText.trim())`);
    assert.ok(!offered.includes("Stop"), `no Stop while the old folder is removed: ${JSON.stringify(offered)}`);
    await me.capture("move-job-clearing");
    await me.waitFor("the moved line", (text) => text.includes(`${basename(folder)} is now in your projects folder`), MOVE_MS);
  });
  assert.ok(!existsSync(folder), "the old folder is gone from the other disk");
  assert.equal(readFileSync(join(target, "p199", "f99.txt"), "utf8"), "199 99\n", "the last file arrived");
  assert.equal(await projectFolder(id), target);
  assert.deepEqual(releaseDisk(disk), [], "the scratch disk is gone");
});
