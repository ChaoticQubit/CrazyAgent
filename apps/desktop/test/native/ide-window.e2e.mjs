import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { go, until } from "./trial-kit.mjs";

const root = process.env.MS_ROOT;
const evidence = process.env.MS_EVIDENCE;
const windowIdTool = process.env.IDE_WINDOW_ID_TOOL;
const app = process.env.WD_APP;
const GIT = "/usr/bin/git";
const POOL = "local-pool";
const SETUP_MS = 120000;
const BUILD_MS = 8 * 60 * 1000;
const BACK_MS = 90000;
const POLL_MS = 1000;
const SHOWN_WIDTH = 1040;
const SHOWN_HEIGHT = 720;
const PANEL_SHIFT = "96px";
let generation = 0;

/** Calls one desktop command through the app's own bridge. Inputs: the command and its
 * arguments. Output: its answer; throws with the failure the app returned. */
async function invoke(command, args) {
  const answer = await browser.executeAsync((command, args, done) => {
    window.__TAURI_INTERNALS__.invoke(command, args).then((value) => done({ value }), (failure) => done({ failed: JSON.stringify(failure) }));
  }, command, args);
  if (answer.failed !== undefined) throw new Error(`${command} failed: ${answer.failed}`);
  return answer.value;
}

/** Makes an owner repository with one commit and no system or user git config. Input: its
 * name. Output: its absolute path. */
function ownerRepo(name) {
  const repo = join(root, "repos", name);
  mkdirSync(repo, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), `# ${name}\n`);
  const env = { PATH: "/usr/bin:/bin", HOME: repo, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" };
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "Start"]]) {
    const result = spawnSync(GIT, args, { cwd: repo, encoding: "utf8", env });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
  return repo;
}

/** Adds a repository as a project on the pool and waits for onboarding. Input: the
 * repository name. Output: the project id. */
async function newProject(name) {
  const receipt = await invoke("desktop_start_onboarding", { path: ownerRepo(name), pool: POOL, generation: ++generation });
  assert.equal(receipt.outcome, "applied", `${name} becomes a project: ${JSON.stringify(receipt)}`);
  for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS)) {
    const view = await invoke("desktop_onboarding", { id: receipt.project_id });
    if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
    assert.ok(Date.now() < deadline, `onboarding of ${name} still running: ${JSON.stringify(view)}`);
  }
  await invoke("desktop_open_project", { id: receipt.project_id });
  return receipt.project_id;
}

/** Downloads the pinned IDE build into the test root. Output: once the build is ready;
 * throws when it failed or took too long. */
async function buildReady() {
  await invoke("desktop_ide_state", { retry: true });
  for (const deadline = Date.now() + BUILD_MS; ; await pause(POLL_MS * 5)) {
    const { build } = (await invoke("desktop_ide_state", { retry: false })).state;
    if (build.state === "ready") return;
    assert.notEqual(build.state, "failed", `the IDE build failed: ${JSON.stringify(build)}`);
    assert.ok(Date.now() < deadline, `the IDE build is still ${JSON.stringify(build)}`);
  }
}

/** The app's window number, found by its process. Output: the number as text; throws when
 * the window is not on screen. */
function windowNumber() {
  const found = spawnSync("/usr/bin/pgrep", ["-f", `^${app}`], { encoding: "utf8" }).stdout.trim().split("\n")[0];
  assert.ok(found, "the test app has a process");
  const listed = spawnSync(windowIdTool, [found], { encoding: "utf8" });
  const number = listed.stdout.trim().split("\n")[0];
  assert.ok(number, `the test app has a window on screen: ${listed.stderr}`);
  return number;
}

/** Saves a picture of the app's window alone, with the IDE webview in it. Input: the name. */
function shot(name) {
  const taken = spawnSync("/usr/sbin/screencapture", ["-x", "-o", `-l${windowNumber()}`, join(evidence, `${name}.png`)], { encoding: "utf8" });
  assert.equal(taken.status, 0, `the ${name} picture: ${taken.stderr}`);
}

/** Resizes the app's window through the system. Output: the command's status and error. */
function resize() {
  const found = spawnSync("/usr/bin/pgrep", ["-f", `^${app}`], { encoding: "utf8" }).stdout.trim().split("\n")[0];
  const script = `tell application "System Events" to tell (first process whose unix id is ${found}) to set size of front window to {${SHOWN_WIDTH}, ${SHOWN_HEIGHT}}`;
  const done = spawnSync("/usr/bin/osascript", ["-e", script], { encoding: "utf8" });
  return { status: done.status, error: done.stderr.trim() };
}

/** Runs the plan in the page on its own timers, since the webdriver session cannot see the
 * main window while an IDE webview is in it. Input: the plan rows. Output: none. */
function startPlan(plan) {
  return browser.execute((rows) => {
    const log = [];
    window.__ideWindowLog = log;
    const answer = (command) => window.__TAURI_INTERNALS__.invoke(command).then(() => "ok", (failure) => `failed: ${JSON.stringify(failure)}`);
    const note = async (step) => {
      const box = document.querySelector(".ide-panel")?.getBoundingClientRect();
      log.push({ step, status: await answer("desktop_status"), alerts: [...document.querySelectorAll("[role=alert]")].map((node) => node.textContent), panel: box ? { x: box.x, y: box.y, width: box.width, height: box.height } : null, inner: [innerWidth, innerHeight], hash: location.hash });
    };
    for (const [at, kind, value] of rows) {
      setTimeout(() => {
        if (kind === "go") location.hash = value;
        if (kind === "note") void note(value);
        if (kind === "shift") document.querySelector(".pj-tab-panel").style.marginTop = value;
      }, at * 1000);
    }
  }, plan);
}

describe("In the real window the IDE webview stays in its panel and leaves with its tab", () => {
  before(async () => {
    await until(() => browser.execute(() => Boolean(window.__TAURI_INTERNALS__)), 120000, "the app window never loaded");
    await until(async () => (await invoke("desktop_connect").catch(() => ({}))).connected === true, 60000, "the app never connected to the test engine");
    assert.equal((await invoke("desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: ++generation })).outcome, "applied");
    mkdirSync(join(root, "projects"), { mode: 0o700 });
    const homed = await invoke("desktop_set_projects_home", { path: join(root, "projects"), generation: ++generation });
    assert.equal(homed.outcome, "applied", `the projects home is set: ${JSON.stringify(homed)}`);
    await browser.execute(() => location.reload());
    await until(() => browser.execute(() => document.readyState === "complete" && Boolean(window.__TAURI_INTERNALS__)), 60000, "the app window never loaded again");
  });

  it("opens, leaves, returns, follows its panel, and goes away with its project", async () => {
    const first = await newProject("ide-one");
    const second = await newProject("ide-two");
    await buildReady();
    const page = (id, tab) => `#/projects/${encodeURIComponent(id)}/${tab}`;
    await go(page(first, "overview"));
    await until(() => browser.execute(() => Boolean(document.querySelector("#tab-ide"))), 60000, "the project page never listed the IDE tab");
    await browser.saveScreenshot(join(evidence, "before-open.png"));
    await startPlan([
      [0, "go", page(first, "ide")],
      [90, "note", "open"],
      [95, "go", page(first, "overview")],
      [105, "note", "left"],
      [110, "go", page(first, "ide")],
      [125, "note", "back"],
      [130, "shift", PANEL_SHIFT],
      [140, "note", "moved"],
      [143, "shift", ""],
      [155, "note", "resized"],
      [160, "go", page(second, "overview")],
      [170, "go", page(second, "ide")],
      [215, "note", "second-open"],
      [220, "go", "#/"],
      [232, "note", "closed"],
    ]);
    const started = Date.now();
    const at = (seconds) => pause(Math.max(0, started + seconds * 1000 - Date.now()));
    const actions = [
      [30, () => shot("open-30s")], [60, () => shot("open-60s")], [92, () => shot("open-92s")],
      [107, () => shot("left")], [127, () => shot("back")], [142, () => shot("moved")],
      [145, () => writeFileSync(join(evidence, "resize.json"), JSON.stringify(resize()))],
      [157, () => shot("resized")], [217, () => shot("second-open")], [234, () => shot("closed")],
    ];
    for (const [seconds, action] of actions) {
      await at(seconds);
      action();
    }
    let log = null;
    for (const deadline = Date.now() + BACK_MS; log === null; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "the webdriver session never saw the main window again after the IDE closed");
      log = await browser.execute(() => (window.__ideWindowLog?.some((row) => row.step === "closed") ? window.__ideWindowLog : null)).catch(() => null);
    }
    writeFileSync(join(evidence, "log.json"), JSON.stringify(log, null, 2));
    for (const row of log) {
      assert.equal(row.status, "ok", `${row.step}: the app page's commands answer while the IDE is loaded`);
      assert.deepEqual(row.alerts, [], `${row.step}: no failed place or hide`);
    }
    const panel = (step) => log.find((row) => row.step === step).panel;
    assert.equal(panel("left"), null, "the IDE panel is gone from the page when the tab is left");
    assert.ok(panel("moved").y > panel("back").y, "the panel really moved");
    assert.equal(panel("closed"), null, "the page left the project");
  });
});
