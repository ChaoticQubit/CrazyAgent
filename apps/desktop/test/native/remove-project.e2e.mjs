import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir, userInfo } from "node:os";
import { basename, isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, setUpProject, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const HDIUTIL = "/usr/bin/hdiutil";
const SQLITE = "/usr/bin/sqlite3";
const CHFLAGS = "/usr/bin/chflags";
const KEEP = "Remove from dashboard (keep files)";
const DELETE = "Delete everything";
const CUT_SHORT = "A delete was cut short. Some folders may already be in the Trash.";
const JOB_MS = 90000;
/** How long after a delete starts a row may still change its stand-in agents: inside the engine's
 * 30 s DELETE_STOP_WAIT, with room left for the job's own start, so a slow run fails as timing. */
const STOP_GUARD_MS = 25000;
const run = { generation: 10 };

/** Counts the entries of the owner's own Trash, never listing them, so a run can prove it put
 * nothing there. Output: the count. */
function realTrashCount() {
  const trash = join(userInfo().homedir, ".Trash");
  try {
    return readdirSync(trash).length;
  } catch (failure) {
    throw new Error(`PREREQUISITE: this test counts the entries of ~/.Trash to prove it never touches it, and reading it failed with ${failure.code}; give the terminal Full Disk Access and rerun`);
  }
}

/** Sets up one project inside the projects home through the bridge. Input: its name. Output:
 * its id and folder. */
async function made(name) {
  const id = await setUpProject(run.bridge, { name, repositories: [] }, {
    about: `${name} for the remove dialog`, tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [],
  });
  const setup = await value(run.bridge, "desktop_project_setup", { id });
  const folder = setup.folder.endsWith("/.crazyagent") ? setup.folder.slice(0, -"/.crazyagent".length) : setup.folder;
  return { id, folder };
}

async function listed(id) {
  return (await value(run.bridge, "desktop_project_list")).items.some((item) => item.id === id);
}

/** Opens a project's remove dialog from its Settings as the owner does. Inputs: the page, its
 * helpers, the project and its name. Output: none. */
async function openRemove(page, me, id, name) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/projects/${encodeURIComponent(id)}/settings?section=remove` });
  await me.waitFor(`${name}'s Remove project section`, (text) => text.includes(name) && text.includes("Remove project"));
  await me.click("Remove project", "button.danger");
  await me.waitFor("the remove dialog with its choices", (text) => text.includes(KEEP));
}

/** Reads the open remove dialog: its words, which choice is checked and whether its Delete
 * button can be pressed. Input: the page. Output: the reading. */
function dialog(page) {
  return page.evaluate(`(() => {
    const box = document.querySelector("dialog[open]");
    if (!box) return null;
    const checked = box.querySelector("input[type=radio]:checked");
    const remove = [...box.querySelectorAll("button")].find((button) => button.innerText.trim() === ${JSON.stringify(DELETE)});
    return { text: box.innerText, checked: checked ? checked.value : null, deleteEnabled: remove ? !remove.disabled : null };
  })()`);
}

/** Reads whether the open dialog's top choice, loss list and Delete everything button each lie
 * wholly inside the window and inside every box up to the dialog that clips them. Input: the
 * page. Output: each answer (null when not shown), the window height and the dialog's visible
 * and full heights. */
function framed(page) {
  return page.evaluate(`(() => {
    const box = document.querySelector("dialog[open]");
    const seen = (node) => {
      if (!node) return null;
      const rect = node.getBoundingClientRect();
      const inside = (outer) => rect.top >= outer.top - 0.5 && rect.bottom <= outer.bottom + 0.5 && rect.left >= outer.left - 0.5 && rect.right <= outer.right + 0.5;
      if (!inside({ top: 0, left: 0, bottom: innerHeight, right: innerWidth })) return false;
      for (let up = node.parentElement; up && up !== box.parentElement; up = up.parentElement) {
        const style = getComputedStyle(up);
        if ((style.overflowY !== "visible" || style.overflowX !== "visible") && !inside(up.getBoundingClientRect())) return false;
      }
      return true;
    };
    const remove = [...box.querySelectorAll("button")].find((button) => button.innerText.trim() === ${JSON.stringify(DELETE)});
    return {
      window: innerHeight, client: box.clientHeight, scroll: box.scrollHeight,
      card: seen(box.querySelector(".pj-radio-card")), losses: seen(box.querySelector("[aria-label='What would be lost']")), remove: seen(remove),
    };
  })()`);
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

/** Runs one statement on the test engine's database, for a stand-in no client can make, such as
 * an agent slow to stop or a delete an engine stop cut short. Input: the statement. Output: what
 * sqlite3 printed, trimmed. */
function sql(statement) {
  return execFileSync(SQLITE, [run.database, statement], { encoding: "utf8" }).trim();
}

/** Adds a stand-in team agent of a project that reads working until the row flips it, as an
 * agent slow to stop. Inputs: the project and the agent's number. Output: none. */
function working(id, agent) {
  sql(`INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms) VALUES (${agent},'${id}','team','codex',1,'${join(run.root, `agent-${agent}`)}','working',0,0);`);
}

/** The mark a Delete everything job leaves when an engine stop cuts it short, newer than any
 * mark an earlier row left, so the next start shows this one. Inputs: the project, its name, the
 * exchange and how many folders the delete moves. Output: the statements. */
function cutShortMark(id, name, exchange, total) {
  const payload = JSON.stringify({ project: id, exchange, request: `delete-cut-${exchange}`, name, total });
  return `INSERT INTO projects_delete_pending(exchange) VALUES (${exchange});
    INSERT INTO ledger_actions(kind,action_key,payload,started_ms) VALUES ('project_delete','${id}','${payload}',${Date.now()});`;
}

/** Starts the engine's delete job of a project through the bridge, as Delete everything in the
 * Remove dialog does once the name is typed. Inputs: the project id and its exact name. Output:
 * the started job. */
async function startDelete(id, name) {
  const job = await value(run.bridge, "desktop_delete_project_job", { id, name });
  assert.equal(job.kind, "delete", JSON.stringify(job));
  assert.equal(job.project_id, id, JSON.stringify(job));
  assert.equal(job.source, name, JSON.stringify(job));
  return job;
}

/** Opens Home in the page, where the engine's job is followed. Input: the page. Output: none. */
async function openHome(page) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/` });
}

/** Picks Delete everything in the open remove dialog, types the project's exact name and presses
 * Delete, as the owner does. Inputs: the page, its helpers and the name. Output: none. */
async function confirmDelete(page, me, name) {
  await me.click(DELETE, "dialog[open] input[type=radio]");
  await me.waitFor("the name box", (text) => text.includes("Type"));
  await me.type("dialog[open] input[aria-label='Type the project name']", name);
  for (let shown, deadline = Date.now() + STEP_MS; !(shown = await dialog(page)).deleteEnabled; await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `Delete never enabled after the exact name: ${JSON.stringify(shown)}`);
  }
  await me.click(DELETE, "dialog[open] button");
}

/** Reads the buttons of Home's job row, never those of a found folder's row. Input: the page.
 * Output: their labels. */
function rowButtons(page) {
  return page.evaluate(`[...document.querySelectorAll("div.in-row button")].map((button) => button.innerText.trim())`);
}

/** Tells whether the test volume's Trash holds a folder moved there from the projects folder.
 * Input: the folder. Output: the answer. */
function inVolumeTrash(folder) {
  const volumeTrash = join(realpathSync(run.volume), ".Trashes", String(process.getuid()));
  return readdirSync(volumeTrash).some((entry) => entry.startsWith(basename(folder)));
}

before(async () => {
  run.engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  run.bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  run.trashBefore = realTrashCount();
  run.root = realpathSync(mkdtempSync(join(tmpdir(), "ca-rm-")));
  run.evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(run.root, "evidence");
  const runtime = join(run.root, "engine");
  const home = join(run.root, "home");
  run.outside = join(run.root, "outside", "field-notes");
  for (const folder of [runtime, run.evidence, home, join(home, ".codex"), run.outside]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(run.outside, "notes.md"), "kept where it is\n");
  run.image = join(run.root, "projects.dmg");
  run.volume = join(run.root, "volume");
  mkdirSync(run.volume, { mode: 0o700 });
  execFileSync(HDIUTIL, ["create", "-quiet", "-size", "64m", "-fs", "APFS", "-volname", "CARemoveE2E", run.image]);
  execFileSync(HDIUTIL, ["attach", "-quiet", "-nobrowse", "-noautoopen", "-mountpoint", run.volume, run.image]);
  run.attached = true;
  run.projects = join(realpathSync(run.volume), "projects");
  mkdirSync(run.projects, { mode: 0o700 });
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
  run.log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  run.env = env;
  run.engineArgs = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"];
  run.socket = join(runtime, "engine.sock");
  ({ child: run.engine } = await launch(run.engineBinary, run.engineArgs, { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await launch(run.bridgeBinary, [run.socket], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = started.child;
  run.bridge = { port: Number(started.match[1]), token: started.match[2] };
  assert.equal((await value(run.bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
  assert.equal((await value(run.bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: run.projects, generation: 2 })).outcome, "applied");
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
  if (run.attached) {
    try {
      execFileSync(HDIUTIL, ["detach", "-quiet", "-force", run.volume]);
    } catch (failure) {
      console.error(`hdiutil detach ${run.volume} failed: ${failure.message}`);
    }
  }
  if (run.trashBefore !== undefined) assert.equal(realTrashCount(), run.trashBefore, "the owner's own Trash holds as many entries as before the run");
  if (run.root && !process.env.E2E_EVIDENCE) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});

test("Remove from dashboard is the preselected choice: the project leaves the list and its folder stays on disk", { timeout: 300000 }, async () => {
  const name = "Dashboard Only";
  const { id, folder } = await made(name);
  await onPage("dashboard", async (page, me) => {
    await openRemove(page, me, id, name);
    const shown = await dialog(page);
    assert.equal(shown.checked, "dashboard", `${KEEP} is preselected: ${JSON.stringify(shown)}`);
    assert.ok(shown.text.includes(DELETE), "a project inside the projects folder also offers Delete everything");
    await me.capture("dashboard-dialog");
    await me.click("Remove project", "dialog[open] button");
    await me.waitFor("Home after the removal", (text) => text.includes(`${name} was removed`));
  });
  assert.equal(await listed(id), false, "the engine no longer lists the project");
  assert.ok(existsSync(join(folder, ".crazyagent")), "the project folder stays with its files");
});

test("Delete everything lists the loss, waits for the typed name, then moves the folder to the volume's Trash and keeps the tokens under Removed projects", { timeout: 300000 }, async () => {
  const name = "Trash Me";
  const { id, folder } = await made(name);
  assert.ok(folder.startsWith(`${run.projects}/`), `the project sits on the test disk image: ${folder}`);
  await value(run.bridge, "desktop_usage_metrics", { projectId: null, sinceMs: 0 });
  execFileSync(SQLITE, [run.database, `INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,input,output,cache_read,cache_write) VALUES ((strftime('%s','now')/3600)*3600000,'agent','${id}','local-pool','local-account','1','rcli','s1',1200,300,8000,0)`]);
  await onPage("delete", async (page, me) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 760, height: 560, deviceScaleFactor: 1, mobile: false });
    await openRemove(page, me, id, name);
    await me.click(DELETE, "dialog[open] input[type=radio]");
    await me.waitFor("the loss list and the name box", (text) => text.includes(folder) && text.includes("Type"));
    let shown = await dialog(page);
    assert.equal(shown.checked, "everything");
    assert.equal(shown.deleteEnabled, false, "Delete stays disabled before the name is typed");
    await me.type("dialog[open] input[aria-label='Type the project name']", `${name} `);
    shown = await dialog(page);
    assert.equal(shown.deleteEnabled, false, "a trailing space is not the name");
    await me.type("dialog[open] input[aria-label='Type the project name']", name);
    for (const deadline = Date.now() + STEP_MS; !(shown = await dialog(page)).deleteEnabled; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `Delete never enabled after the exact name: ${JSON.stringify(shown)}`);
    }
    const fit = await framed(page);
    assert.ok(fit.window === 560 && fit.scroll <= fit.client && fit.card === true && fit.remove === true,
      `at the app's smallest window the dialog fits its frame and the top choice and Delete are in view: ${JSON.stringify(fit)}`);
    await me.capture("delete-ready");
    await me.click(DELETE, "dialog[open] button");
    await me.waitFor("Home's delete job after the delete", (text) => text.includes(`${name} is deleted. Its files are in the Trash.`), 90000);
    await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/metrics` });
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS * 10)) {
      const rows = await page.evaluate(`[...document.querySelectorAll(".mx-table tbody tr")].map((row) => row.children[0]?.textContent.trim())`).catch(() => []);
      if (rows.includes("Removed projects")) break;
      assert.ok(Date.now() < deadline, `app Metrics never showed a Removed projects row: ${JSON.stringify(rows)}`);
    }
    await me.capture("delete-metrics");
  });
  assert.equal(await listed(id), false, "the engine no longer lists the project");
  assert.ok(!existsSync(folder), "the folder is gone from the projects folder");
  const volumeTrash = join(realpathSync(run.volume), ".Trashes", String(process.getuid()));
  assert.ok(readdirSync(volumeTrash).some((entry) => entry.startsWith(basename(folder))), "the folder is in the test volume's Trash");
});

test("a repository that cannot move leaves the project folder in the volume's Trash, keeps the project and says so", { timeout: 300000 }, async () => {
  const name = "Half Moved";
  const { id, folder } = await made(name);
  const repository = join(run.projects, "half-moved-lib");
  mkdirSync(repository, { mode: 0o700 });
  writeFileSync(join(repository, "notes.md"), "stays where it is\n");
  execFileSync(SQLITE, [run.database, `UPDATE projects SET body=json_set(body,'$.spec.repositories',json_array('${repository}')) WHERE id='${id}'`]);
  execFileSync(CHFLAGS, ["uchg", repository]);
  try {
    await onPage("partial", async (page, me) => {
      await openRemove(page, me, id, name);
      await me.click(DELETE, "dialog[open] input[type=radio]");
      await me.waitFor("both folders and the name box", (text) => text.includes(folder) && text.includes(repository) && text.includes("Type"));
      await me.type("dialog[open] input[aria-label='Type the project name']", name);
      for (const deadline = Date.now() + STEP_MS; !(await dialog(page)).deleteEnabled; await pause(POLL_MS)) {
        assert.ok(Date.now() < deadline, "Delete never enabled after the exact name");
      }
      await me.click(DELETE, "dialog[open] button");
      await me.waitFor("Home's delete job ending on the partial move", (text) => text.includes(`${name}: Some of its folders are in the Trash and the rest couldn't be moved.`), 90000);
      await me.capture("delete-partial");
    });
  } finally {
    execFileSync(CHFLAGS, ["nouchg", repository]);
  }
  assert.equal(await listed(id), true, "a partial move keeps the project's records");
  assert.ok(!existsSync(folder), "the project folder moved first");
  assert.equal(readFileSync(join(repository, "notes.md"), "utf8"), "stays where it is\n", "the folder that could not move is untouched");
  const volumeTrash = join(realpathSync(run.volume), ".Trashes", String(process.getuid()));
  assert.ok(readdirSync(volumeTrash).some((entry) => entry.startsWith(basename(folder))), "the project folder is in the test volume's Trash");
});

test("with agents running and a change set open, Delete everything at the app's smallest window shows the whole loss list while its Delete button is in view", { timeout: 300000 }, async () => {
  const name = "Losses Shown";
  const { id } = await made(name);
  const now = Date.now();
  execFileSync(SQLITE, [run.database, `INSERT INTO team_agents (project, role, harness, tools, status, created_ms, heartbeat_ms) VALUES ('${id}','sub','codex',0,'working',${now},${now}),('${id}','sub','claude',0,'idle',${now},${now});
    INSERT INTO landing_change_sets (id, project, agent_kind, agent_id, label, repository, worktree, git_dir, base, head, state, last_outcome, turn, prompt_line, files, added, removed, conflicts, created_ms, updated_ms)
    VALUES ('e2e-open-set','${id}','sub','1','Open set','/nonexistent','/nonexistent','/nonexistent','base','head','pending','succeeded','turn-1','Change the header',1,1,0,'[]',${now},${now});`]);
  await onPage("losses", async (page, me) => {
    await page.send("Emulation.setDeviceMetricsOverride", { width: 760, height: 560, deviceScaleFactor: 1, mobile: false });
    await openRemove(page, me, id, name);
    await me.click(DELETE, "dialog[open] input[type=radio]");
    await me.waitFor("the loss list and the name box", (text) => text.includes("will be stopped") && text.includes("not accepted or discarded") && text.includes("Type"));
    await me.type("dialog[open] input[aria-label='Type the project name']", name);
    for (const deadline = Date.now() + STEP_MS; !(await dialog(page)).deleteEnabled; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "Delete never enabled after the exact name");
    }
    const lines = await page.evaluate(`[...document.querySelectorAll("dialog[open] [aria-label='What would be lost'] li")].map((line) => line.innerText.trim())`);
    assert.equal(lines.length, 3, `two running agents and one open change set: ${JSON.stringify(lines)}`);
    const fit = await framed(page);
    assert.ok(fit.window === 560 && fit.losses === true && fit.remove === true && fit.card === true,
      `the whole loss list, the top choice and Delete are in view and no box clips them: ${JSON.stringify(fit)}`);
    await me.capture("delete-losses");
    await me.click("Keep project", "dialog[open] button");
  });
  assert.equal(await listed(id), true, "Keep project changes nothing");
});

test("a project set up from a folder outside the projects folder offers only Remove from dashboard", { timeout: 300000 }, async () => {
  const receipt = await value(run.bridge, "desktop_start_onboarding", { path: run.outside, pool: "local-pool", generation: run.generation++ });
  assert.equal(receipt.outcome, "applied", `the outside folder is set up where it is: ${JSON.stringify(receipt)}`);
  const id = receipt.project_id;
  const name = (await value(run.bridge, "desktop_project_list")).items.find((item) => item.id === id).name;
  await onPage("outside", async (page, me) => {
    await openRemove(page, me, id, name);
    const shown = await dialog(page);
    assert.equal(shown.checked, "dashboard");
    assert.ok(!shown.text.includes(DELETE), `a project outside the projects folder has no Delete option: ${shown.text}`);
    assert.ok(shown.text.includes("Only a project whose folders are all inside your projects folder can be deleted here."), `the dialog says why there is no Delete option: ${shown.text}`);
    await me.capture("outside-dialog");
    await me.click("Keep project", "dialog[open] button");
  });
  assert.equal(await listed(id), true, "Keep project changes nothing");
  assert.equal(readFileSync(join(run.outside, "notes.md"), "utf8"), "kept where it is\n");
});

test("a delete an engine stop cut short shows on the project with Keep the project and Delete again, and Keep clears it", { timeout: 300000 }, async () => {
  const name = "Cut Short";
  const { id, folder } = await made(name);
  await value(run.bridge, "desktop_recovery_banner", { projectId: id });
  await restart(() => sql(`INSERT INTO projects_delete_pending(exchange) VALUES (900);
    INSERT INTO ledger_actions(kind,action_key,payload,started_ms) VALUES ('project_delete','${id}','{"project":"${id}","exchange":900}',${Date.now()});`));
  assert.equal(sql("SELECT count(*) FROM projects_delete_pending"), "0", "the start still forgets the plan, so a home move may go");
  const held = await value(run.bridge, "desktop_intake_status");
  assert.ok(held === null || held.project_id !== id, `a 2.4.1 mark names no job, so Home shows none for it: ${JSON.stringify(held)}`);
  await onPage("cut-short", async (page, me) => {
    await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/projects/${encodeURIComponent(id)}/overview` });
    await me.waitFor("the delete cut short on the project", (text) => text.includes(CUT_SHORT) && text.includes("its agents stay stopped until you start them again"));
    await me.capture("cut-short-banner");
    await me.click("Delete again", "button");
    await me.waitFor("the remove dialog on Delete everything", (text) => text.includes(KEEP) && text.includes(folder) && text.includes("Type"));
    const shown = await dialog(page);
    assert.equal(shown.checked, "everything", `Delete again opens on Delete everything: ${JSON.stringify(shown)}`);
    assert.equal(shown.deleteEnabled, false, "the delete still waits for the typed name");
    await me.capture("cut-short-delete-again");
    await me.click("Keep project", "dialog[open] button");
    await me.click("Keep the project", "button");
    await me.waitFor("the banner without the delete", (text) => !text.includes(CUT_SHORT));
    await me.capture("cut-short-kept");
  });
  assert.equal(await listed(id), true, "Keep the project keeps it");
  assert.ok(existsSync(join(folder, ".crazyagent")), "its folder stays where it is");
  assert.equal(sql(`SELECT count(*) FROM ledger_actions WHERE kind='project_delete' AND action_key='${id}' AND outcome IS NULL`), "0", "Keep the project clears the mark");
});

test("a delete waits on Home for the project's agents to stop, refuses another project's delete naming it, is found again by the same project's start and a new window, and moves both its folders to the Trash once they stopped (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Slow Stop";
  const { id, folder } = await made(name);
  const other = await made("Waits Its Turn");
  writeFileSync(join(folder, "notes.md"), "kept until the agents stop\n");
  const library = join(run.projects, "slowstop-library");
  mkdirSync(library, { mode: 0o700 });
  writeFileSync(join(library, "notes.md"), "moves with the project\n");
  sql(`UPDATE projects SET body=json_set(body,'$.spec.repositories',json_array('${library}')) WHERE id='${id}'`);
  working(id, 81);
  const waiting = `Waiting for 1 agent of ${name} to stop...`;
  let asked = 0;
  await onPage("delete-stopping", async (page, me) => {
    await openRemove(page, me, id, name);
    await confirmDelete(page, me, name);
    asked = Date.now();
    await me.waitFor("Home's waiting line", (text) => text.includes(waiting), JOB_MS);
    assert.deepEqual(await rowButtons(page), ["Stop"], "Stop is offered while the agents stop");
    assert.equal(readFileSync(join(folder, "notes.md"), "utf8"), "kept until the agents stop\n", "nothing moves while it waits");
    assert.equal(readFileSync(join(library, "notes.md"), "utf8"), "moves with the project\n", "its repository waits too");
    await me.capture("delete-job-stopping");
    await openRemove(page, me, other.id, "Waits Its Turn");
    await confirmDelete(page, me, "Waits Its Turn");
    await me.waitFor("the busy note in the dialog", (text) => text.includes(`Deleting ${name} is running. Wait for it to finish on Home.`), JOB_MS);
    await me.capture("delete-job-busy");
  });
  const running = await value(run.bridge, "desktop_intake_status");
  assert.equal(running?.kind, "delete", JSON.stringify(running));
  assert.equal((await startDelete(id, name)).id, running.id, "the same project's start finds the running delete");
  await onPage("delete-reopened", async (page, me) => {
    await openHome(page);
    await me.waitFor("the running delete in a new window", (text) => text.includes(waiting), JOB_MS);
    const spent = Date.now() - asked;
    assert.ok(asked > 0 && spent < STOP_GUARD_MS, `the agents stop ${spent} ms after Delete, past ${STOP_GUARD_MS} ms inside the engine's 30 s agent wait: this run was too slow, not the delete`);
    sql("UPDATE team_agents SET status='stopped' WHERE id=81");
    await me.waitFor("the deleted line", (text) => text.includes(`${name} is deleted. Its files are in the Trash.`), JOB_MS);
    assert.deepEqual(await rowButtons(page), ["Dismiss"]);
    await me.capture("delete-job-done");
    await me.click("Dismiss", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(`${name} is deleted`), JOB_MS);
  });
  assert.equal(await listed(id), false, "the engine no longer lists the project");
  assert.equal(await listed(other.id), true, "the refused delete left the other project");
  assert.ok(!existsSync(folder), "the folder left the projects folder");
  assert.ok(inVolumeTrash(folder), "the folder is in the test volume's Trash");
  assert.ok(!existsSync(library), "its repository left the projects folder");
  assert.ok(inVolumeTrash(library), "its repository is in the test volume's Trash too: a delete of 2 folders ends Done");
  assert.equal(await value(run.bridge, "desktop_intake_status"), null, "Dismiss forgets the ended delete");
});

test("Stop on Home while a delete waits for its agents keeps the project and its folder, says nothing went to the Trash, and Try again opens the remove ask on Delete everything (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Stopped Delete";
  const { id, folder } = await made(name);
  working(id, 82);
  const asked = Date.now();
  await startDelete(id, name);
  const stopped = `Deleting ${name} stopped. Nothing was moved to the Trash.`;
  await onPage("delete-cancelled", async (page, me) => {
    await openHome(page);
    await me.waitFor("Home's waiting line", (text) => text.includes(`Waiting for 1 agent of ${name} to stop...`), JOB_MS);
    const spent = Date.now() - asked;
    assert.ok(spent < STOP_GUARD_MS, `Stop comes ${spent} ms after the delete, past ${STOP_GUARD_MS} ms inside the engine's 30 s agent wait: this run was too slow, not the delete`);
    await me.click("Stop", "div.in-row button");
    await me.waitFor("the stopped line", (text) => text.includes(stopped), JOB_MS);
    assert.deepEqual(await rowButtons(page), ["Try again", "Dismiss"]);
    await me.capture("delete-job-cancelled");
    await me.click("Try again", "div.in-row button");
    await me.waitFor("the remove dialog on Delete everything", (text) => text.includes(KEEP) && text.includes(folder) && text.includes("Type"), JOB_MS);
    const shown = await dialog(page);
    assert.equal(shown.checked, "everything", `Try again opens on Delete everything: ${JSON.stringify(shown)}`);
    assert.equal(shown.deleteEnabled, false, "nothing deletes in one click: the name is typed again");
    await me.capture("delete-job-try-again");
    await me.click("Keep project", "dialog[open] button");
    await openHome(page);
    await me.waitFor("the stopped line again", (text) => text.includes(stopped), JOB_MS);
    await me.click("Dismiss", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(stopped), JOB_MS);
  });
  assert.equal(await listed(id), true, "a stopped delete keeps the project");
  assert.ok(existsSync(join(folder, ".crazyagent")), "its folder stays where it is");
  sql("UPDATE team_agents SET status='stopped' WHERE id=82");
});

test("a delete whose agents do not stop within the wait fails on Home with the reason, keeps the project and offers Try again (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Stuck Agents";
  const { id, folder } = await made(name);
  working(id, 83);
  await startDelete(id, name);
  const failed = `${name}: Its agents didn't stop in time, so nothing was deleted. They stay stopped; try again.`;
  await onPage("delete-agents-running", async (page, me) => {
    await openHome(page);
    await me.waitFor("the failed line after the agents' wait", (text) => text.includes(failed), 120000);
    assert.deepEqual(await rowButtons(page), ["Try again", "Dismiss"]);
    await me.capture("delete-job-agents-running");
    await me.click("Dismiss", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(failed), JOB_MS);
  });
  assert.equal(await listed(id), true, "a failed delete keeps the project");
  assert.ok(existsSync(join(folder, ".crazyagent")), "its folder stays where it is");
  sql("UPDATE team_agents SET status='stopped' WHERE id=83");
});

test("a delete an engine stop cut short after 1 of its 2 folders shows on Home after the next start, never goes on by itself, and Finish delete asks for the name again and moves the rest to the Trash (DEC-177)", { timeout: 300000 }, async () => {
  const name = "Half Gone";
  const { id, folder } = await made(name);
  const library = join(run.projects, "half-gone-lib");
  mkdirSync(library, { mode: 0o700 });
  sql(`UPDATE projects SET body=json_set(body,'$.spec.repositories',json_array('${library}')) WHERE id='${id}'`);
  await restart(() => {
    rmSync(library, { recursive: true });
    sql(cutShortMark(id, name, 901, 2));
  });
  const cut = `Deleting ${name} was cut short with 1 of 2 folders in the Trash. Finish delete or keep the project.`;
  await onPage("delete-cut-short", async (page, me) => {
    await openHome(page);
    await me.waitFor("the cut-short line", (text) => text.includes(cut) && text.includes("Cut short"), JOB_MS);
    assert.deepEqual(await rowButtons(page), ["Finish delete", "Keep the project"]);
    assert.ok(existsSync(join(folder, ".crazyagent")), "the start never finishes a delete by itself");
    await me.capture("delete-job-cut-short");
    await me.click("Finish delete", "div.in-row button");
    await me.waitFor("the remove dialog on Delete everything", (text) => text.includes(KEEP) && text.includes(folder) && text.includes("Type"), JOB_MS);
    const shown = await dialog(page);
    assert.equal(shown.checked, "everything", `Finish delete opens on Delete everything: ${JSON.stringify(shown)}`);
    assert.equal(shown.deleteEnabled, false, "Finish delete waits for the name typed again");
    await confirmDelete(page, me, name);
    await me.waitFor("the deleted line", (text) => text.includes(`${name} is deleted. Its files are in the Trash.`), JOB_MS);
    await me.capture("delete-job-finished");
  });
  assert.equal(await listed(id), false, "Finish delete removes the project");
  assert.ok(!existsSync(folder), "the folder left the projects folder");
  assert.ok(inVolumeTrash(folder), "the rest went to the test volume's Trash");
});

test("Keep the project on Home clears the project's cut-short banner, and the banner's Keep the project clears Home's row (DEC-177)", { timeout: 300000 }, async () => {
  const first = await made("Kept From Home");
  await restart(() => sql(cutShortMark(first.id, "Kept From Home", 902, 2)));
  const fromHome = "Deleting Kept From Home was cut short with 1 of 2 folders in the Trash.";
  await onPage("delete-keep-home", async (page, me) => {
    await openHome(page);
    await me.waitFor("the cut-short line", (text) => text.includes(fromHome), JOB_MS);
    await me.click("Keep the project", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(fromHome), JOB_MS);
    await me.capture("delete-job-kept-home");
  });
  assert.equal(sql(`SELECT count(*) FROM ledger_actions WHERE kind='project_delete' AND action_key='${first.id}' AND outcome IS NULL`), "0", "Keep the project on Home ends the banner's mark");
  assert.equal(await value(run.bridge, "desktop_intake_status"), null, "Keep the project forgets the job");
  const second = await made("Kept From Banner");
  await restart(() => sql(cutShortMark(second.id, "Kept From Banner", 903, 2)));
  const restored = await value(run.bridge, "desktop_intake_status");
  assert.deepEqual([restored?.project_id, restored?.state], [second.id, { state: "cut_short", trashed: 1, total: 2 }], JSON.stringify(restored));
  await onPage("delete-keep-banner", async (page, me) => {
    await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/projects/${encodeURIComponent(second.id)}/overview` });
    await me.waitFor("the delete cut short on the project", (text) => text.includes(CUT_SHORT), JOB_MS);
    await me.click("Keep the project", "button");
    await me.waitFor("the banner without the delete", (text) => !text.includes(CUT_SHORT), JOB_MS);
    await me.capture("delete-job-kept-banner");
  });
  assert.equal(await value(run.bridge, "desktop_intake_status"), null, "the banner's Keep the project forgets Home's job");
  assert.equal(await listed(first.id), true);
  assert.equal(await listed(second.id), true);
});
