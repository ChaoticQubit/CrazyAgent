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
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const HDIUTIL = "/usr/bin/hdiutil";
const SQLITE = "/usr/bin/sqlite3";
const CHFLAGS = "/usr/bin/chflags";
const KEEP = "Remove from dashboard (keep files)";
const DELETE = "Delete everything";
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
  const receipt = await value(run.bridge, "desktop_setup_project", {
    spec: { name, repositories: [] },
    setup: { about: `${name} for the remove dialog`, tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [] },
    generation: run.generation++,
  });
  assert.equal(receipt.outcome, "applied", `${name} is made: ${JSON.stringify(receipt)}`);
  const setup = await value(run.bridge, "desktop_project_setup", { id: receipt.project_id });
  const folder = setup.folder.endsWith("/.crazyagent") ? setup.folder.slice(0, -"/.crazyagent".length) : setup.folder;
  return { id: receipt.project_id, folder };
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
  ({ child: run.engine } = await launch(run.engineBinary,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
    { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await launch(run.bridgeBinary, [join(runtime, "engine.sock")], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
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
  execFileSync(SQLITE, [run.database, `INSERT INTO usage_tokens VALUES ((strftime('%s','now')/3600)*3600000,'agent','${id}','local-pool','local-account','1','rcli','s1',1200,300,8000,0)`]);
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
    await me.waitFor("Home after the delete", (text) => text.includes(`${name} was deleted`), 90000);
    await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/metrics` });
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS * 10)) {
      const rows = await page.evaluate(`[...document.querySelectorAll(".mx-table tbody tr")].map((row) => row.children[0]?.innerText.trim())`).catch(() => []);
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
      await me.waitFor("the partial move's refusal", (text) => text.includes("Some of its folders are in the Trash and the rest couldn't be moved."), 90000);
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
