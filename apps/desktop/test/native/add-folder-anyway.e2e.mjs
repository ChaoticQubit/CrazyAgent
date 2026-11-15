import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { closeSync, existsSync, ftruncateSync, mkdirSync, mkdtempSync, openSync, readFileSync, readSync, realpathSync, rmSync, statSync, statfsSync, writeFileSync, writeSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start, stop, value } from "./e2e-harness.mjs";
import { git, linkedWorktrees, scratchRepo, seedRepo } from "./trial4-projects.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const MOVE_MS = 300000;
const SETUP_MS = 120000;
const DISK_MS = 120000;
const LARGE_BYTES = Math.round(1.1 * 1024 ** 3);
const FREE_BYTES_MIN = 5 * 1024 ** 3;
const MARK_BYTES = 64;
const MARKS = [0, Math.floor(LARGE_BYTES / 2), LARGE_BYTES - MARK_BYTES].map((at) => ({ at, bytes: Buffer.from(`CA-E2E archive.bin mark at ${at} `.padEnd(MARK_BYTES, "#")) }));
assert(MARKS.every(({ at, bytes }) => bytes.length === MARK_BYTES && at >= 0 && at + MARK_BYTES <= LARGE_BYTES), "every mark fits inside the large file");
const SETUP_ROUTE = /^#\/projects\/([^/]+)\/setup$/;
const WAYS = '[role=radiogroup][aria-label="How to add it"] button';
const SAID = {
  close_apps: "Close editors, terminals and apps that use this folder first. Files saved while it moves can be lost.",
  old_place: "Anything that points at the old place (terminals, editors, scripts, shortcuts) stops working.",
  across_disks: "This copies every file to the other disk, then removes the original. It can take a long time and needs as much free space again. If the Mac sleeps, loses power or the disk fills while it copies, your original stays whole and a part copy may be left in your projects folder to delete. If it stops while it removes the original, the full copy stays in your projects folder (keep it) and part of the original is left.",
  same_disk: "This is a rename on the same disk; it is quick.",
  large: "This folder is very large.",
  measured: "It is about 1.1 GB.",
  worktrees: "Git worktrees linked to this folder stop working until you run git worktree repair in each. Their files stay, but git there fails until repaired.",
  linked_worktree: "This folder is a git worktree of another repository, and that repository keeps this folder's staged changes and branch link. Right after the move, go to that repository and run git worktree repair with the moved folder's path. Until then git here may fail or seem to work, and a git worktree prune or git gc there drops those staged changes and the branch link for good; commits here that are on no branch can be lost too. The files stay.",
  no_git: "This folder has no git history. Agents' changes can't be shown or undone with git until you run git init here.",
};

const run = { picks: [], log: { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false } };

/** Makes a plain folder with no git: a README and one nested file. Input: the new folder.
 * Output: its real path and the nested file's text. */
function plainFolder(folder) {
  assert.ok(!existsSync(folder), `${folder} already exists`);
  mkdirSync(join(folder, "notes"), { recursive: true });
  writeFileSync(join(folder, "README.md"), `# ${basename(folder)}\n\nNo git history.\n`);
  const notes = `Keep ${basename(folder)} safe.\n`;
  writeFileSync(join(folder, "notes", "todo.md"), notes);
  return { real: realpathSync(folder), notes };
}

/** Says whether a folder is a mount point now, read from the system's mount table.
 * Input: the real path. Output: true while something is mounted there. */
function mounted(mount) {
  return execFileSync("/sbin/mount", { encoding: "utf8" }).split("\n").some((line) => line.includes(` on ${mount} (`));
}

/** Makes and attaches a sparse APFS disk image under the test's temp root, unseen in Finder,
 * so a folder on it sits on another disk than the projects folder. Input: the temp root.
 * Output: the image file and its mount point; throws when free space is short. */
function attachDisk(root) {
  const { bavail, bsize } = statfsSync(root);
  const free = Number(bavail) * Number(bsize);
  assert.ok(free >= FREE_BYTES_MIN,
    `PREREQUISITE: the cross-disk Move anyway check needs ${FREE_BYTES_MIN} bytes free on the disk holding ${root}, which has ${free}; free space and rerun`);
  const image = join(root, "other-disk.sparseimage");
  const mount = join(root, "other-disk");
  mkdirSync(mount);
  const made = spawnSync("hdiutil", ["create", "-quiet", "-size", "4g", "-type", "SPARSE", "-fs", "APFS", "-volname", "CA-E2E", image], { encoding: "utf8", timeout: DISK_MS });
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

/** Reads the bytes a file holds at each mark's offset. Inputs: the file and [{ at, bytes }].
 * Output: one Buffer per mark, as long as its bytes; fails when the file ends before one. */
function bytesAt(file, marks) {
  const fd = openSync(file, "r");
  try {
    return marks.map(({ at, bytes }) => {
      const read = Buffer.alloc(bytes.length);
      assert.equal(readSync(fd, read, 0, read.length, at), read.length, `${file} holds ${read.length} bytes at ${at}`);
      return read;
    });
  } finally {
    closeSync(fd);
  }
}

/** Checks that each line shows in the question, in the given order. Inputs: the question's text,
 * the lines and what is checked. Output: none; fails naming the first line missing or out of order. */
function inOrder(body, lines, what) {
  let at = -1;
  for (const line of lines) {
    const next = body.indexOf(line);
    assert.ok(next > at, `${what}: "${line}" is missing or out of order in the question:\n${body}`);
    at = next;
  }
}

/** The intake job starts the page sent for one folder. Input: the folder. Output: their trail entries. */
function addsOf(source) {
  return run.log.trail.filter((entry) => entry.cmd === "desktop_intake_start" && entry.args?.path === source);
}

/** Opens the Add folder page in a fresh browser context on the served app, runs the owner's steps,
 * then requires no page errors; a failure keeps a screenshot. Inputs: the label and the steps.
 * Output: none. */
async function onAddFolder(label, steps) {
  await withPage(async (page) => {
    const me = owner(page, run.evidence);
    try {
      await page.send("Page.enable");
      await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
      await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/projects/add/folder` });
      await me.waitFor("the Add folder page", (text) => text.includes("Add an existing folder"));
      await steps(page, me);
      assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "no page errors");
    } catch (failure) {
      await me.capture(`${label}-failure`).catch(() => {});
      throw failure;
    }
  });
}

/** Picks a folder through Choose folder, or Choose another folder after a first pick, answered
 * with that folder as the macOS chooser would, then chooses Move and asks to move it. A new pick
 * keeps the way chosen before, so the page reads either line for it. Inputs: the page helpers and
 * the real folder. Output: none, once the page took the pick and Move was pressed. */
async function askMove(me, folder) {
  const picked = run.log.picked;
  run.picks.push(folder);
  await me.click((await me.seen()).includes("Choose another folder") ? "Choose another folder" : "Choose folder");
  await me.waitFor(`the picked folder ${basename(folder)}`, (text) => text.includes(`${basename(folder)} stays where it is.`) || text.includes(`${basename(folder)} goes to `));
  assert.equal(run.log.picked, picked + 1, "the owner picked the folder once");
  await me.click("Move", WAYS);
  await me.click("Move into projects folder");
}

/** Waits for the Move anyway question about a folder and reads it. Inputs: the page, its helpers
 * and the folder name. Output: the question's text, once it opened on Use where it is. */
async function question(page, me, name) {
  await me.waitFor(`the Move ${name} anyway? question`, (text) => text.includes(`Move ${name} anyway?`));
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const focused = await page.evaluate("document.activeElement?.textContent?.trim() ?? ''");
    if (focused === "Use where it is") break;
    assert.ok(Date.now() < deadline, `the question starts on ${JSON.stringify(focused)}, not Use where it is`);
  }
  return page.evaluate(`document.querySelector("dialog[open]")?.innerText ?? ""`);
}

/** Waits for Home after a Move anyway and checks the moved folder's row and the engine's record.
 * Inputs: the page, its helpers, the moved folder's new path, whether it has git and the wait.
 * Output: none. */
async function landedOnHome(page, me, landed, hasGit, ms) {
  const name = basename(landed);
  await me.waitFor(`the toast that ${name} is in the projects folder`, (text) => text.includes(`${name} is in your projects folder. Set it up from Home.`), ms);
  assert.equal(await page.evaluate("location.hash"), "#/home", "Move anyway goes to Home");
  let row = "";
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    row = await page.evaluate(`[...document.querySelectorAll(".in-found .in-list .in-row")].map((item) => item.innerText).find((text) => text.includes(${JSON.stringify(landed)}) || text.includes(${JSON.stringify(name)})) ?? ""`);
    if (row) break;
    if (Date.now() > deadline) {
      await me.capture(`no-row-${name}`);
      assert.fail(`Home never listed ${name} in the projects folder`);
    }
  }
  assert.match(row, hasGit ? /Git repository\./ : /No git yet\./, `the Home row for ${name}: ${row}`);
  const found = (await value(run.bridge, "desktop_found_projects")).filter((entry) => entry.path === landed);
  assert.equal(found.length, 1, `the engine lists ${landed} once`);
  assert.equal(found[0].state, "found", `the engine reads ${landed} as found, ready to set up`);
  assert.equal(found[0].git, hasGit, `the engine reads git in ${landed}`);
}

/** Requires one intake job that moved a folder with exactly the overrides the question sent, and
 * that the engine reads that job as done. Inputs: the source folder and the overrides. Output:
 * none. */
async function movedOnce(source, overrides) {
  const adds = addsOf(source);
  assert.equal(adds.length, 1, `one intake job start for ${source}: ${JSON.stringify(adds)}`);
  assert.equal(adds[0].args.mode, "move", "the job moves the folder");
  assert.deepEqual(adds[0].args.overrides, overrides, "the start sends only the overrides for issues the engine refuses");
  const started = adds[0].answer?.value;
  assert.ok(started && started.source === source && started.mode === "move", `the engine started the move job: ${JSON.stringify(adds[0].answer)}`);
  const job = await value(run.bridge, "desktop_intake_status");
  assert.equal(job?.id, started.id, `the engine still shows the started job: ${JSON.stringify(job)}`);
  assert.equal(job.state.state, "done", `the engine reads the move job as done: ${JSON.stringify(job)}`);
}

before(async () => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  process.umask(0o077);
  process.env.GIT_CONFIG_GLOBAL = "/dev/null";
  process.env.GIT_CONFIG_NOSYSTEM = "1";
  run.root = realpathSync(mkdtempSync(join(tmpdir(), "ca-e2e-add-")));
  const runtime = join(run.root, "engine");
  run.evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(run.root, "evidence");
  mkdirSync(run.evidence, { recursive: true, mode: 0o700 });
  const home = join(run.root, "home");
  run.projects = join(run.root, "projects");
  run.repos = join(run.root, "repos");
  for (const folder of [runtime, home, join(home, ".codex"), run.projects, run.repos]) mkdirSync(folder, { recursive: true, mode: 0o700 });
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
  ({ child: run.engine } = await start(engineBinary,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
    { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await start(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = started.child;
  run.bridge = { port: Number(started.match[1]), token: started.match[2] };
  assert.equal((await value(run.bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
  const profiled = await value(run.bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 });
  assert.equal(profiled?.outcome, "applied", `the owner profile is saved: ${JSON.stringify(profiled)}`);
  const homed = await value(run.bridge, "desktop_set_projects_home", { path: run.projects, generation: 2 });
  assert.equal(homed?.outcome, "applied", `the projects home is set: ${JSON.stringify(homed)}`);
  assert.equal((await value(run.bridge, "desktop_projects_home")).path, run.projects);
  const dist = join(run.root, "dist");
  await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
  assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
  run.site = await serveApp(dist, run.bridge, run.picks, run.log);
});

after(async () => {
  run.log.closing = true;
  let left = [];
  try {
    if (run.evidence) {
      writeFileSync(join(run.evidence, "calls.json"), JSON.stringify(run.log.calls, null, 1));
      writeFileSync(join(run.evidence, "answers.json"), JSON.stringify(run.log.trail, null, 1));
    }
    left = run.disk ? releaseDisk(run.disk) : [];
    run.site?.server.close();
    await stop(run.bridgeProcess);
    await stop(run.engine);
  } finally {
    if (run.root) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
  assert.ok(!run.root || !existsSync(run.root), `the temp root ${run.root} is removed`);
  assert.deepEqual(left, [], "the scratch disk is detached and its image deleted");
  assert.deepEqual(run.log.invalid, [], "the bridge accepted every command's arguments");
  assert.deepEqual(run.log.dropped, [], "the bridge answered every call");
});

/** Move anyway on a folder with no git (DEC-115 addendum): the question shows
 * the data loss line first, then the old place, no git and the quick rename, starts on Use where
 * it is, and Move anyway moves every file into the projects folder with no override sent. */
test("Move anyway moves a folder with no git into the projects folder after the data loss warning", { timeout: 600000 }, async () => {
  const { real, notes } = plainFolder(join(run.repos, "seed-notes"));
  const name = basename(real);
  const landed = join(run.projects, name);
  await onAddFolder("nd35", async (page, me) => {
    await askMove(me, real);
    const body = await question(page, me, name);
    await me.capture("nd35-question");
    inOrder(body, [SAID.close_apps, SAID.old_place, SAID.no_git, SAID.same_disk], "a Move on the same disk of a folder with no git");
    for (const line of [SAID.large, SAID.across_disks, SAID.worktrees, SAID.linked_worktree]) assert.ok(!body.includes(line), `the question names no issue the folder does not have: ${line}`);
    await me.click("Move anyway", "dialog[open] button");
    await landedOnHome(page, me, landed, false, MOVE_MS);
    await me.capture("nd35-home");
  });
  assert.ok(!existsSync(real), `${real} is gone from its old place`);
  assert.equal(readFileSync(join(landed, "notes", "todo.md"), "utf8"), notes, "every file moved with its content");
  await movedOnce(real, []);
});

/** Use where it is in the Move anyway question (DEC-115 addendum): the owner
 * says no to the move, the Set up dialog opens for the folder where it is, setup starts there,
 * and nothing lands in the projects folder or is sent to Add folder. */
test("Use where it is in the Move anyway question sets the folder up where it is", { timeout: 600000 }, async () => {
  const { real, notes } = plainFolder(join(run.repos, "recipe-box"));
  const name = basename(real);
  let id = "";
  await onAddFolder("nd6", async (page, me) => {
    await askMove(me, real);
    const body = await question(page, me, name);
    inOrder(body, [SAID.close_apps, SAID.old_place, SAID.no_git], "a Move of a folder with no git");
    await me.click("Use where it is", "dialog[open] button");
    await me.waitFor(`the Set up ${name} dialog with the pool`, (text) => text.includes(`Set up ${name}`) && text.includes("local-pool"));
    await me.capture("nd6-set-up");
    await me.click("Start setup", "dialog[open] button");
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS)) {
      const route = await page.evaluate("location.hash");
      if (SETUP_ROUTE.test(route)) {
        id = decodeURIComponent(route.match(SETUP_ROUTE)[1]);
        break;
      }
      assert.ok(Date.now() < deadline, `setup of ${name} never opened; the page is on ${route}`);
    }
    for (const deadline = Date.now() + SETUP_MS; !existsSync(join(real, ".crazyagent")); await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `setup made no .crazyagent folder in ${real}`);
    }
    await me.capture("nd6-setup");
  });
  assert.equal(readFileSync(join(real, "notes", "todo.md"), "utf8"), notes, "the folder stays where it is with its files");
  assert.ok(!existsSync(join(run.projects, name)), `nothing named ${name} lands in the projects folder`);
  assert.deepEqual(addsOf(real), [], "Use where it is starts no intake job");
  const listed = (await value(run.bridge, "desktop_project_list")).items.filter((item) => item.id === id);
  assert.equal(listed.length, 1, `the engine lists the project ${id} the page set up once`);
  const setup = await value(run.bridge, "desktop_project_setup", { id });
  assert.equal(setup.folder, join(real, ".crazyagent"), `the engine sets ${name} up where it is: ${JSON.stringify(setup.folder)}`);
});

/** The linked worktree warnings (DEC-115 addendum, add-folder r2 close): a repository with a
 * linked worktree is asked about with that worktree listed, a folder that is itself a linked
 * worktree is asked about with the staged changes loss first, and closing either question
 * changes nothing on disk or in git. Close keeps Move chosen, and asking to move again asks the
 * same question again, with no Add folder call. */
test("the Move anyway question names linked worktrees, and closing it changes nothing", { timeout: 600000 }, async () => {
  const made = scratchRepo(join(run.repos, "garden-log"));
  const repoName = basename(made.repo);
  const treeName = basename(made.worktree);
  await onAddFolder("worktrees", async (page, me) => {
    await askMove(me, made.repo);
    const repoBody = await question(page, me, repoName);
    await me.capture("worktrees-repo-question");
    inOrder(repoBody, [SAID.close_apps, SAID.old_place, SAID.worktrees, made.worktree], "a Move of a repository with a linked worktree");
    assert.ok(!repoBody.includes(SAID.linked_worktree), "the repository is not itself a linked worktree");
    await me.click("Close", "dialog[open] button");
    await me.waitFor("the question closed with Move still chosen", (text) => !text.includes(`Move ${repoName} anyway?`) && text.includes(`${repoName} goes to `));
    await me.click("Move into projects folder");
    assert.equal(await question(page, me, repoName), repoBody, "asking to move again after Close asks the same question again");
    await me.click("Close", "dialog[open] button");
    await me.waitFor("the asked again question closed", (text) => !text.includes(`Move ${repoName} anyway?`) && text.includes(`${repoName} goes to `));
    assert.deepEqual(addsOf(made.repo), [], "asking to move again after Close starts no intake job");

    await askMove(me, made.worktree);
    const treeBody = await question(page, me, treeName);
    await me.capture("worktrees-tree-question");
    inOrder(treeBody, [SAID.linked_worktree, SAID.close_apps, SAID.old_place], "a Move of a folder that is a linked worktree");
    assert.ok(!treeBody.includes(SAID.worktrees), "a worktree has no linked worktrees of its own");
    await me.click("Close", "dialog[open] button");
    await me.waitFor("the second question closed", (text) => !text.includes(`Move ${treeName} anyway?`) && text.includes(`${treeName} goes to `));
    await me.capture("worktrees-closed");
  });
  assert.deepEqual(addsOf(made.repo), [], "closing the question starts no intake job for the repository");
  assert.deepEqual(addsOf(made.worktree), [], "closing the question starts no intake job for the worktree");
  assert.ok(existsSync(join(made.repo, ".git")), "the repository stays where it is with its git");
  assert.deepEqual(linkedWorktrees(made.repo), [made.worktree], "the repository keeps its linked worktree");
  assert.equal(git(made.worktree, ["rev-parse", "--abbrev-ref", "HEAD"]), "trial4-side", "git still works in the worktree on its branch");
  for (const name of [repoName, treeName]) assert.ok(!existsSync(join(run.projects, name)), `nothing named ${name} lands in the projects folder`);
});

/** Move anyway on a very large folder across disks (DEC-115 addendum):
 * a git folder holding a 1.1 GB file on a scratch disk image is asked about with apps still using
 * it first, then the size and the copy across disks, and Move anyway copies every byte into the
 * projects folder, keeps git working there and removes the original. */
test("Move anyway moves a very large folder from another disk after the copy warnings", { timeout: 900000 }, async (t) => {
  const disk = attachDisk(run.root);
  try {
    const folder = seedRepo(join(disk.mount, "photo-archive"), "# Photo archive\n\nA very large folder on another disk.\n");
    const head = git(folder, ["rev-parse", "HEAD"]);
    const big = openSync(join(folder, "archive.bin"), "w");
    try {
      ftruncateSync(big, LARGE_BYTES);
      for (const { at, bytes } of MARKS) writeSync(big, bytes, 0, bytes.length, at);
    } finally {
      closeSync(big);
    }
    assert.deepEqual(bytesAt(join(folder, "archive.bin"), MARKS), MARKS.map((mark) => mark.bytes), "the large file holds its marks before the move");
    assert.notEqual(statSync(folder).dev, statSync(run.projects).dev, "the folder sits on another disk than the projects folder");
    const name = basename(folder);
    const landed = join(run.projects, name);
    await onAddFolder("nd36", async (page, me) => {
      await askMove(me, folder);
      const body = await question(page, me, name);
      await me.capture("nd36-question");
      inOrder(body, [SAID.close_apps, SAID.large, SAID.measured, SAID.across_disks, SAID.old_place], "a Move across disks of a very large folder");
      for (const line of [SAID.same_disk, SAID.no_git, SAID.worktrees]) assert.ok(!body.includes(line), `the question names no issue the folder does not have: ${line}`);
      await me.click("Move anyway", "dialog[open] button");
      await landedOnHome(page, me, landed, true, MOVE_MS);
      await me.capture("nd36-home");
    });
    assert.ok(!existsSync(folder), `${folder} is removed from the other disk once the copy is checked`);
    assert.equal(statSync(join(landed, "archive.bin")).size, LARGE_BYTES, "the large file moved whole");
    assert.deepEqual(bytesAt(join(landed, "archive.bin"), MARKS), MARKS.map((mark) => mark.bytes), "the large file's bytes at its start, middle and end moved unchanged");
    assert.equal(git(landed, ["rev-parse", "HEAD"]), head, "git works in the moved folder at the same commit");
    await movedOnce(folder, []);
  } finally {
    const left = releaseDisk(disk);
    if (left.length > 0) t.diagnostic(`scratch disk left behind: ${left.join("; ")}`);
  }
  assert.equal(run.disk, null, "the scratch disk is detached and its image deleted");
});
