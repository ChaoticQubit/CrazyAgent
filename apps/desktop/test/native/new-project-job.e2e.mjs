import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, setUpProject, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SQLITE = "/usr/bin/sqlite3";
const KEPT = "Couldn't create the project. Your answers are kept.";
const FAILED = "The project folder couldn't be created. Nothing was kept.";
const RESEARCH_ON = ["market_research", "competitor_research", "design_research"];
const JOB_MS = 90000;
const run = { generation: 10, homes: 0 };

/** Runs one statement on the test engine's database, for a stand-in no client can make, such as
 * an agent slow to stop or a New Project an engine stop cut short. Input: the statement. Output:
 * what sqlite3 printed, trimmed. */
function sql(statement) {
  return execFileSync(SQLITE, [run.database, statement], { encoding: "utf8" }).trim();
}

/** Reads a project's folder from its saved setup. Input: the project id. Output: the folder. */
async function projectFolder(id) {
  const setup = await value(run.bridge, "desktop_project_setup", { id });
  return setup.folder.endsWith("/.crazyagent") ? setup.folder.slice(0, -"/.crazyagent".length) : setup.folder;
}

/** Reads the engine's projects by name. Input: none. Output: the names. */
async function projectNames() {
  return (await value(run.bridge, "desktop_project_list")).items.map((item) => item.name);
}

/** Reads the projects the test pool serves. Input: none. Output: their ids. */
async function poolProjects() {
  const page = await value(run.bridge, "desktop_model_access_page", { collection: "pools", next: false });
  const pool = page.items.find((record) => record.kind === "pool" && record.pool.id === "local-pool");
  assert.ok(pool, `the test pool is listed: ${JSON.stringify(page.items)}`);
  return pool.pool.projects;
}

/** Reads the engine's job until `done` says it is the one wanted, as Home's poll does. Inputs:
 * what is waited for and the check. Output: the job read last. */
async function jobUntil(what, done) {
  const deadline = Date.now() + JOB_MS;
  for (;;) {
    const job = await value(run.bridge, "desktop_intake_status");
    if (done(job)) return job;
    assert.ok(Date.now() < deadline, `the engine's job never reached ${what}: ${JSON.stringify(job)}`);
    await pause(POLL_MS);
  }
}

/** Opens a path of the app in the page. Inputs: the page and the hash path. Output: none. */
async function open(page, path) {
  await page.send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#${path}` });
}

/** Reads the page's hash route. Input: the page. Output: the hash. */
function hash(page) {
  return page.evaluate("location.hash");
}

/** Waits until the page's hash route passes `check`. Inputs: the page, what is waited for and the
 * check. Output: the hash. */
async function hashUntil(page, what, check) {
  const deadline = Date.now() + JOB_MS;
  for (;;) {
    const now = await hash(page);
    if (check(now)) return now;
    assert.ok(Date.now() < deadline, `the page never reached ${what}: ${now}`);
    await pause(POLL_MS);
  }
}

/** Reads the buttons of Home's job row, never those of a found folder's row. Input: the page.
 * Output: their labels. */
function rowButtons(page) {
  return page.evaluate(`[...document.querySelectorAll("div.in-row button")].map((button) => button.innerText.trim())`);
}

/** Fills New Project as the owner does, publishing on GitHub, research and roadmap planning
 * off so no agent starts and nothing needs a GitHub sign-in, the test pool kept, then presses
 * Create project. Inputs: the page, its helpers, the name and whether the brief is attached
 * through Choose files. Output: none. */
async function create(page, me, name, documents = false) {
  await open(page, "/projects/new");
  await me.waitFor("New Project's Basics", (text) => text.includes("Basics") && text.includes("Project name"));
  await me.type("#np-name", name);
  await me.type("#np-about", `${name} for the New Project job`);
  await me.follow("#np-publish");
  assert.equal(await page.evaluate(`document.querySelector("#np-publish")?.getAttribute("aria-checked") ?? null`), "false", "publishing on GitHub is off for this run");
  await me.click("Next", "button");
  await me.waitFor("the Documents step", (text) => text.includes("Choose files"));
  if (documents) {
    await me.click("Choose files", "button");
    await me.waitFor("the attached brief", (text) => text.includes("brief.md"));
  }
  await me.click("Next", "button");
  await me.waitFor("the Work step", (text) => text.includes("What should agents start on?"));
  for (const kind of RESEARCH_ON) await me.follow(`#np-research-${kind}`);
  await me.follow("#np-roadmap");
  const on = await page.evaluate(`[...document.querySelectorAll("button[role=switch]")].filter((item) => item.getAttribute("aria-checked") === "true").map((item) => item.id)`);
  assert.deepEqual(on, [], "no research agent and no roadmap planning start with the project");
  await me.click("Next", "button");
  await me.waitFor("the Pool step", (text) => text.includes("local-pool"));
  assert.equal(await page.evaluate(`document.querySelector('input[name=np-pool][value="local-pool"]')?.checked ?? null`), true, "the test pool is chosen");
  await me.click("Next", "button");
  await me.waitFor("the Team step", (text) => text.includes("Team") && text.includes("Create project"));
  await me.click("Create project", "button");
}

/** Reads the project name kept on Basics after Create came back, by going to Basics. Inputs:
 * the page and its helpers. Output: the name box's value. */
async function keptName(page, me) {
  await me.follow("nav[aria-label=Steps] li:first-child button");
  await me.waitFor("Basics again", (text) => text.includes("Project name"));
  return page.evaluate(`document.querySelector("#np-name")?.value ?? null`);
}

/** Starts the engine's New Project job through the bridge, as Create project does, with no page
 * following it. Input: the name. Output: the started job. */
async function startSetup(name) {
  const job = await value(run.bridge, "desktop_setup_project_job", {
    spec: { name, repositories: [] },
    setup: { about: `${name} for the New Project job`, tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [] },
  });
  assert.equal(job.kind, "setup", JSON.stringify(job));
  assert.equal(job.source, name, JSON.stringify(job));
  return job;
}

/** Makes the projects folder read-only for `steps`, so a New Project passes its check and then
 * fails to build its folder, and gives the folder its mode back after. Input: the steps.
 * Output: none. */
async function readOnlyHome(steps) {
  chmodSync(run.projects, 0o500);
  try {
    await steps();
  } finally {
    chmodSync(run.projects, 0o700);
  }
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

before(async () => {
  run.engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  run.bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  run.root = realpathSync(mkdtempSync(join(tmpdir(), "ca-np-")));
  run.evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(run.root, "evidence");
  const runtime = join(run.root, "engine");
  const home = join(run.root, "home");
  const papers = join(run.root, "papers");
  run.projects = join(run.root, "projects");
  for (const folder of [runtime, run.evidence, home, join(home, ".codex"), papers, run.projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  run.brief = join(papers, "brief.md");
  writeFileSync(run.brief, "# Brief\n\nWhat the project is for.\n");
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
  run.site = await serveApp(dist, run.bridge, [], run.log, { desktop_pick_documents: () => ({ value: [run.brief] }) });
});

after(async () => {
  run.log && (run.log.closing = true);
  if (run.evidence && run.log) writeFileSync(join(run.evidence, "calls.json"), JSON.stringify(run.log.calls, null, 1));
  run.site?.server.close();
  await stop(run.bridgeProcess);
  await stop(run.engine);
  if (run.projects && existsSync(run.projects)) chmodSync(run.projects, 0o700);
  if (run.root && !process.env.E2E_EVIDENCE) rmSync(run.root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});

test("Create project runs the engine's New Project job: the brief is copied, the pool assigned, the job dismissed and the project opens (DEC-177 step 3)", { timeout: 300000 }, async () => {
  const name = "Job Made";
  let id = null;
  await onPage("np-done", async (page, me) => {
    await create(page, me, name, true);
    const opened = await hashUntil(page, "the new project's overview", (now) => /^#\/projects\/[^/?]+\/overview/.test(now));
    id = decodeURIComponent(opened.split("/")[2]);
    await me.capture("np-done-overview");
  });
  assert.ok((await projectNames()).includes(name), "the engine lists the new project");
  const folder = await projectFolder(id);
  assert.ok(folder.startsWith(`${run.projects}/`), `the project sits in the projects folder: ${folder}`);
  const files = readdirSync(folder, { recursive: true });
  assert.ok(files.some((file) => file.endsWith(join("context", "documents", "brief.md"))), `the brief is copied into context/documents: ${JSON.stringify(files)}`);
  assert.deepEqual(await poolProjects(), [id], "Create project's follow-up assigns the chosen pool");
  assert.equal(await jobUntil("no job", (job) => job === null), null, "the page dismisses the job it followed once done");
  assert.ok(run.log.calls.includes("desktop_setup_project_job"), "Create project starts the engine's job");
  assert.ok(!run.log.calls.includes("desktop_setup_project"), "Create project never calls the direct setup");
});

test("a New Project whose folder cannot be built fails on the open page with New Project's words, keeps the answers, leaves no folder and dismisses the job (DEC-177 step 3)", { timeout: 300000 }, async () => {
  const name = "Read Only Fail";
  const before = readdirSync(run.projects);
  await readOnlyHome(() => onPage("np-failed-page", async (page, me) => {
    await create(page, me, name);
    await me.waitFor("the failure under the form", (text) => text.includes(`${KEPT} ${FAILED}`), JOB_MS);
    await me.capture("np-failed-page");
    assert.equal(await keptName(page, me), name, "the answers are kept");
    assert.equal(await jobUntil("no job", (job) => job === null), null, "the page dismisses the failed job");
  }));
  assert.deepEqual(readdirSync(run.projects), before, "nothing is left in the projects folder");
  assert.ok(!(await projectNames()).includes(name), "the engine has no such project");
});

test("a New Project that failed with no page following shows on Home with Try again, which opens New Project, and Dismiss (DEC-177 step 3)", { timeout: 300000 }, async () => {
  const name = "Home Retry";
  await readOnlyHome(async () => {
    const started = await startSetup(name);
    const ended = await jobUntil("the failed New Project", (job) => job?.id === started.id && job.state.state !== "building");
    assert.deepEqual(ended.state, { state: "failed", code: "home_new_project_failed", phase: "building" }, JSON.stringify(ended));
  });
  const line = `${name}: ${FAILED}`;
  await onPage("np-failed-home", async (page, me) => {
    await open(page, "/");
    await me.waitFor("Home's failed New Project line", (text) => text.includes(line), JOB_MS);
    assert.deepEqual(await rowButtons(page), ["Try again", "Dismiss"]);
    await me.capture("np-failed-home");
    await me.click("Try again", "div.in-row button");
    await hashUntil(page, "New Project", (now) => now.startsWith("#/projects/new"));
    await me.waitFor("New Project's Basics", (text) => text.includes("Project name"));
    await me.capture("np-failed-try-again");
    await open(page, "/");
    await me.waitFor("the failed line again", (text) => text.includes(line), JOB_MS);
    await me.click("Dismiss", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(line), JOB_MS);
  });
  assert.equal(await value(run.bridge, "desktop_intake_status"), null, "Dismiss forgets the failed job");
  assert.ok(!(await projectNames()).includes(name), "the engine has no such project");
});

test("a name whose folder is already in the projects folder is refused at the start with the answers kept and no job (DEC-177 step 3)", { timeout: 300000 }, async () => {
  const name = "Folder Taken";
  const taken = join(run.projects, "folder-taken");
  mkdirSync(taken, { mode: 0o700 });
  writeFileSync(join(taken, "notes.md"), "already here\n");
  await onPage("np-refused", async (page, me) => {
    await create(page, me, name);
    await me.waitFor("the refusal under the form", (text) => text.includes(`${KEPT} Your projects folder already has a folder with this name. Choose another project name.`), JOB_MS);
    await me.capture("np-refused");
    assert.equal(await keptName(page, me), name, "the answers are kept");
  });
  assert.equal(await value(run.bridge, "desktop_intake_status"), null, "a refused start holds no job");
  assert.equal(readFileSync(join(taken, "notes.md"), "utf8"), "already here\n", "the folder already there is untouched");
  assert.ok(!(await projectNames()).includes(name), "the engine has no such project");
});

test("while another project's move waits for its agents, Create project is refused naming the running move, and the move stops from Home (DEC-177 step 3)", { timeout: 300000 }, async () => {
  const holder = await setUpProject(run.bridge, { name: "Busy Holder", repositories: [] }, {
    about: "Busy Holder for the New Project job", tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [],
  });
  const folder = await projectFolder(holder);
  const elsewhere = join(run.root, `home-${++run.homes}`);
  mkdirSync(elsewhere, { mode: 0o700 });
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: elsewhere, generation: run.generation++ })).outcome, "applied");
  sql(`INSERT OR IGNORE INTO team_paused_projects(project,since_ms) VALUES ('${holder}',1);
       INSERT INTO team_agents(id,project,role,harness,tools,worktree,status,created_ms,heartbeat_ms) VALUES (91,'${holder}','team','codex',1,'${join(run.root, "agent-91")}','working',0,0);`);
  const move = await value(run.bridge, "desktop_move_project_home", { id: holder, mode: "move", overrides: [] });
  assert.equal(move.kind, "move_project", JSON.stringify(move));
  await jobUntil("the move waiting for its agent", (job) => job?.id === move.id && job.state.state === "stopping");
  const name = "Waits For Move";
  await onPage("np-busy", async (page, me) => {
    await create(page, me, name);
    await me.waitFor("the busy note naming the move", (text) => text.includes(`${KEPT} Moving busy-holder is running. Wait for it to finish on Home.`), JOB_MS);
    await me.capture("np-busy");
    await open(page, "/");
    await me.waitFor("Home's waiting line", (text) => text.includes("Waiting for 1 agent of busy-holder to stop..."), JOB_MS);
    await me.click("Stop", "div.in-row button");
    await me.waitFor("the stopped move", (text) => text.includes("Moving busy-holder stopped. Nothing was moved or copied."), JOB_MS);
    await me.click("Dismiss", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes("Moving busy-holder stopped"), JOB_MS);
  });
  sql("UPDATE team_agents SET status='stopped' WHERE id=91");
  assert.equal(await projectFolder(holder), folder, "the stopped move leaves its project where it was");
  assert.ok(!(await projectNames()).includes(name), "the refused New Project made no project");
  assert.deepEqual(readdirSync(elsewhere), [], "nothing reached the other projects folder");
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: run.projects, generation: run.generation++ })).outcome, "applied");
});

test("a New Project an engine stop cut short shows on Home after the next start as stopped part way, Try again opens New Project and Dismiss clears it (DEC-177 step 3)", { timeout: 300000 }, async () => {
  const name = "Cut Short";
  const project = "np-cut-short-e2e";
  const payload = JSON.stringify({ project, request: "setup-cut-1", name });
  await restart(() => sql(`INSERT INTO ledger_actions(kind,action_key,payload,started_ms) VALUES ('project_setup','${project}','${payload}',${Date.now()});`));
  assert.equal(sql("SELECT count(*) FROM ledger_actions WHERE kind='project_setup' AND outcome IS NULL"), "0", "the start ends the mark");
  const held = await value(run.bridge, "desktop_intake_status");
  assert.deepEqual([held?.kind, held?.project_id, held?.source, held?.state], ["setup", project, name, { state: "failed", code: "home_new_project_interrupted", phase: "building" }], JSON.stringify(held));
  const line = `${name}: Setting up the project stopped part way. Nothing was kept; set it up again from New Project.`;
  await onPage("np-interrupted", async (page, me) => {
    await open(page, "/");
    await me.waitFor("Home's cut-short New Project line", (text) => text.includes(line), JOB_MS);
    assert.deepEqual(await rowButtons(page), ["Try again", "Dismiss"]);
    await me.capture("np-interrupted");
    await me.click("Try again", "div.in-row button");
    await hashUntil(page, "New Project", (now) => now.startsWith("#/projects/new"));
    await open(page, "/");
    await me.waitFor("the cut-short line again", (text) => text.includes(line), JOB_MS);
    await me.click("Dismiss", "div.in-row button");
    await me.waitFor("the row gone", (text) => !text.includes(line), JOB_MS);
  });
  assert.equal(await value(run.bridge, "desktop_intake_status"), null, "Dismiss forgets the cut-short job");
  assert.ok(!(await projectNames()).includes(name), "a cut-short New Project leaves no project");
});
