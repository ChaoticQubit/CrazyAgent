import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { go, until } from "./trial-kit.mjs";

const root = process.env.MS_ROOT;
const evidence = process.env.MS_EVIDENCE;
const DARK_BELOW = 110;
const LIGHT_ABOVE = 255 - DARK_BELOW;
const IDE_SHARE = 0.7;
const SCAN_LINES = 60;
const SCAN_INSET = 20;
const FIT_PX = 2;
const FIT_STEPS = [["open", "open-92s"], ["back", "back"], ["moved", "moved"], ["resized", "resized"], ["second-open", "second-open"]];
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

/** One saved window picture as pixels. Inputs: the picture's name and whether the IDE is dark.
 * Output: its size and a test for whether a pixel is as dark, or as light, as the IDE, which no
 * whole box of the app page in the other appearance is. */
function pixels(name, ideDark) {
  assert.equal(typeof ideDark, "boolean", "the IDE's appearance is known before a picture is read");
  const bmp = join(evidence, "scan.bmp");
  const made = spawnSync("/usr/bin/sips", ["-s", "format", "bmp", join(evidence, `${name}.png`), "--out", bmp], { encoding: "utf8" });
  assert.equal(made.status, 0, `the ${name} picture converts: ${made.stderr}`);
  const data = readFileSync(bmp);
  rmSync(bmp);
  const start = data.readUInt32LE(10);
  const width = data.readInt32LE(18);
  const rows = data.readInt32LE(22);
  const bytes = data.readUInt16LE(28) / 8;
  const height = Math.abs(rows);
  const stride = Math.ceil((width * bytes) / 4) * 4;
  const ide = (x, y) => {
    const at = start + (rows > 0 ? height - 1 - y : y) * stride + x * bytes;
    const mean = (data[at] + data[at + 1] + data[at + 2]) / 3;
    return ideDark ? mean < DARK_BELOW : mean > LIGHT_ABOVE;
  };
  return { width, height, ide };
}

/** Finds the IDE in a window picture from its pixels alone, not from what the page asked for.
 * Inputs: the picture's pixels, the left and right pixel columns to search and the first pixel
 * row under the window's title bar, which follows the system appearance as the IDE does.
 * Output: the IDE's box edges in pixels, or null when there is none. */
function ideBox(image, left, right, below) {
  assert.ok(below >= 0, `the title bar ends inside the picture: ${below}`);
  assert.ok(below < image.height, `the title bar ends inside the picture: ${below} of ${image.height}`);
  const span = (from, to) => Array.from({ length: SCAN_LINES }, (_, line) => Math.round(from + ((to - from) * line) / (SCAN_LINES - 1)));
  const columns = span(left + SCAN_INSET, right - SCAN_INSET);
  const rowIde = (y) => columns.filter((x) => image.ide(x, y)).length >= IDE_SHARE * SCAN_LINES;
  let top = below;
  while (top < image.height && !rowIde(top)) top += 1;
  if (top === image.height) return null;
  let bottom = image.height - 1;
  while (!rowIde(bottom)) bottom -= 1;
  const rows = span(top + SCAN_INSET, bottom - SCAN_INSET);
  const columnIde = (x) => rows.filter((y) => image.ide(x, y)).length >= IDE_SHARE * SCAN_LINES;
  let first = 0;
  while (first < image.width && !columnIde(first)) first += 1;
  if (first === image.width) return null;
  let last = image.width - 1;
  while (!columnIde(last)) last -= 1;
  return { left: first, top, right: last + 1, bottom: bottom + 1 };
}

/** Compares where the IDE really is in a window picture with its panel. Inputs: the page's row
 * for the step, the picture's name and whether the IDE is dark. Output: both boxes in window
 * points, the largest edge difference and whether the IDE stays clear of the tab row and the
 * side rail. */
function fit(row, picture, ideDark) {
  const image = pixels(picture, ideDark);
  const scale = image.width / row.inner[0];
  const chrome = image.height / scale - row.inner[1];
  const found = ideBox(image, Math.round(row.panel.x * scale), Math.round((row.panel.x + row.panel.width) * scale), Math.round(chrome * scale));
  assert.ok(found, `${row.step}: the IDE is in the ${picture} picture`);
  const seen = Object.fromEntries(Object.entries(found).map(([edge, value]) => [edge, value / scale]));
  const want = { left: row.panel.x, top: row.panel.y + chrome, right: row.panel.x + row.panel.width, bottom: row.panel.y + row.panel.height + chrome };
  const off = Math.max(...Object.keys(want).map((edge) => Math.abs(seen[edge] - want[edge])));
  const clear = seen.top >= row.tabs.bottom + chrome - FIT_PX && seen.left >= row.rail.right - FIT_PX;
  return { step: row.step, picture, scale, chrome, seen, want, off, clear };
}

/** What the real IDE page got back when it called a native command. Output: the page's address
 * and its answer, from the file the test build wrote; the webview may JSON-encode the script's
 * result a second time. */
function ideRefusal() {
  const written = JSON.parse(readFileSync(join(evidence, "ide-refusal.json"), "utf8"));
  return typeof written === "string" ? JSON.parse(written) : written;
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
    const rect = (selector) => {
      const box = document.querySelector(selector)?.getBoundingClientRect();
      return box ? { top: box.top, right: box.right, bottom: box.bottom, left: box.left } : null;
    };
    const note = async (step) => {
      const box = document.querySelector(".ide-panel")?.getBoundingClientRect();
      log.push({ step, status: await answer("desktop_status"), alerts: [...document.querySelectorAll("[role=alert]")].map((node) => node.textContent), panel: box ? { x: box.x, y: box.y, width: box.width, height: box.height } : null, inner: [innerWidth, innerHeight], outer: [outerWidth, outerHeight], screen: [screenX, screenY], tabs: rect(".tabs[role=tablist]"), rail: rect(".sidebar"), hash: location.hash });
    };
    for (const [at, kind, value] of rows) {
      setTimeout(() => {
        if (kind === "go") location.hash = value;
        if (kind === "note") void note(value);
        if (kind === "shift") document.querySelector(".pj-tab-panel").style.marginTop = value;
        if (kind === "press") [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === value)?.click();
      }, at * 1000);
    }
  }, plan);
}

/** Every row the test build wrote about the IDE webviews and the app's windows. Output: the
 * rows, oldest first, each webview's page answer parsed; the webview may JSON-encode it twice. */
function windowRows() {
  let text = "";
  try {
    text = readFileSync(join(evidence, "ide-own-window.jsonl"), "utf8");
  } catch {
    return [];
  }
  return text.slice(0, text.lastIndexOf("\n") + 1).split("\n").filter(Boolean).map((line) => {
    const row = JSON.parse(line);
    if (row.look !== undefined) {
      const once = JSON.parse(row.look);
      row.look = typeof once === "string" ? JSON.parse(once) : once;
    }
    return row;
  });
}

/** Waits for a row the test build wrote. Inputs: the test on a row, what is awaited and how
 * long. Output: the first row that passes, among those newer than `after`. */
async function rowWhere(test, label, timeout, after = 0) {
  for (const deadline = Date.now() + timeout; ; await pause(POLL_MS / 2)) {
    const found = windowRows().find((row) => row.at > after && test(row));
    if (found) return found;
    assert.ok(Date.now() < deadline, `${label}; last row: ${JSON.stringify(windowRows().at(-1))}`);
  }
}

/** The test app's process number. Output: it as text; throws when the app is not running. */
function appProcess() {
  const found = spawnSync("/usr/bin/pgrep", ["-f", `^${app}`], { encoding: "utf8" }).stdout.trim().split("\n")[0];
  assert.ok(found, "the test app has a process");
  return found;
}

/** Runs one System Events line against the test app's process. Input: the line. Output: what
 * it answered; throws when it failed. */
function onApp(line) {
  const script = `tell application "System Events" to tell (first process whose unix id is ${appProcess()})\n${line}\nend tell`;
  const done = spawnSync("/usr/bin/osascript", ["-e", script], { encoding: "utf8" });
  assert.equal(done.status, 0, `${line}: ${done.stderr}`);
  return done.stdout.trim();
}

/** The app menu's top items as the system shows them. Output: their names. */
function menuNames() {
  return onApp("get name of every menu bar item of menu bar 1").split(", ");
}

/** Brings the test app forward and fails the row, with nothing sent, unless it is the frontmost
 * app, because a real key goes to whichever app is in front and Cmd+W could close an owner's
 * window. Output: once the test app is in front; throws when it never gets there. */
async function testAppInFront() {
  onApp("set frontmost to true");
  const mine = appProcess();
  let front = "";
  for (const deadline = Date.now() + FRONT_MS; ; await pause(POLL_MS / 4)) {
    const asked = spawnSync("/usr/bin/osascript", ["-e", 'tell application "System Events" to get unix id of first process whose frontmost is true'], { encoding: "utf8" });
    assert.equal(asked.status, 0, `reading the frontmost app: ${asked.stderr}`);
    front = asked.stdout.trim();
    if (front === mine) return;
    assert.ok(Date.now() < deadline, `process ${front} is in front, not the test app ${mine}, so no key or click was sent`);
  }
}

/** Sends one Command key combination to the front window of the test app, only when the test app
 * is the frontmost app. Input: the key. Output: once it was sent. */
async function commandKey(key) {
  await testAppInFront();
  onApp(`keystroke "${key}" using command down`);
}

/** Raises the app's window with this title. Input: the title's words. */
function raise(title) {
  onApp("set frontmost to true");
  onApp(`perform action "AXRaise" of (first window whose title contains "${title}")`);
}

/** Presses the close button of the app's window with this title, only when the test app is the
 * frontmost app. Input: the title's words. Output: once it was pressed. */
async function closeTitled(title) {
  await testAppInFront();
  onApp(`click (first button of (first window whose title contains "${title}") whose subrole is "AXCloseButton")`);
}

const OWN_WINDOW = /^ide-window-[0-9a-f]+$/;
const OPEN_WINDOW = "Open in its own window";
const WORKBENCH_MS = 90;
const OWN_WINDOW_MS = 30000;
const FRONT_MS = 5000;
const SAME_PAGE_MS = 1000;
const UNSAVED = "UnsavedBeforeRestart";
const BACKUP_MS = 3000;
const SERVER_GONE_MS = 20000;
const WINDOW_GONE_MS = 90000;
const REOPEN_MS = 60000;
const RESTORE_MS = 120000;
const LEAVE_S = 320;

/** Fails the row unless two rows show one page. WebKit's time origin wobbles by a millisecond or
 * so, while a reload would move it by the many seconds since the page first loaded. Inputs: the
 * two rows and the message. Output: none. */
function samePage(row, other, message) {
  assert.ok(row.look && Math.abs(row.look.origin - other.look.origin) < SAME_PAGE_MS, `${message}: ${row.look?.origin} and ${other.look.origin}`);
}

/** Whether a project's IDE server process is alive under this run's test root. Input: the
 * project. Output: a boolean. */
function serverUp(project) {
  const pattern = `server-data-dir ${root}/[^ ]*/ide/${project}/`;
  return spawnSync("/usr/bin/pgrep", ["-f", pattern], { encoding: "utf8" }).stdout.trim() !== "";
}

/** Opens a project's IDE tab, waits for the workbench and presses the window button, all on the
 * page's timers because the webdriver session cannot see the main window meanwhile. Input: the
 * project. Output: the row that first shows the webview in its own window. */
async function moveOut(project) {
  const started = Date.now();
  await startPlan([[0, "go", `#/projects/${encodeURIComponent(project)}/ide`], [WORKBENCH_MS, "press", OPEN_WINDOW]]);
  return rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? ""), "the IDE never reached its own window", WORKBENCH_MS * 1000 + OWN_WINDOW_MS, started);
}

describe("In the real window the IDE webview stays in its panel and leaves with its tab", () => {
  let ideDark = null;

  before(async () => {
    await until(() => browser.execute(() => Boolean(window.__TAURI_INTERNALS__)), 120000, "the app window never loaded");
    await until(async () => (await invoke("desktop_connect").catch(() => ({}))).connected === true, 60000, "the app never connected to the test engine");
    assert.equal((await invoke("desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: ++generation })).outcome, "applied");
    mkdirSync(join(root, "projects"), { mode: 0o700 });
    const homed = await invoke("desktop_set_projects_home", { path: join(root, "projects"), generation: ++generation });
    assert.equal(homed.outcome, "applied", `the projects home is set: ${JSON.stringify(homed)}`);
    await browser.execute(() => location.reload());
    await until(() => browser.execute(() => document.readyState === "complete" && Boolean(window.__TAURI_INTERNALS__)), 60000, "the app window never loaded again");
    ideDark = await browser.execute(() => {
      const dark = matchMedia("(prefers-color-scheme: dark)").matches;
      document.documentElement.setAttribute("data-theme", dark ? "light" : "dark");
      return dark;
    });
    const painted = await browser.execute(() => getComputedStyle(document.body).backgroundColor);
    const mean = painted.match(/\d+/g).slice(0, 3).reduce((sum, part) => sum + Number(part), 0) / 3;
    assert.ok(ideDark ? mean > LIGHT_ABOVE : mean < DARK_BELOW, `the app page takes the appearance the IDE, which follows the system, does not: ${painted}`);
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
    const refusal = ideRefusal();
    assert.match(refusal.origin, /^http:\/\/127\.0\.0\.1:\d+$/, `the call came from the real IDE page: ${JSON.stringify(refusal)}`);
    assert.match(refusal.answer ?? "", /^refused: .*not allowed/, `the real IDE page gets no native command: ${JSON.stringify(refusal)}`);
    const backRow = log.find((row) => row.step === "back");
    const leftImage = pixels("left", ideDark);
    const leftScale = leftImage.width / backRow.inner[0];
    const leftBelow = leftImage.height - Math.round(backRow.inner[1] * leftScale);
    assert.equal(ideBox(leftImage, Math.round(backRow.panel.x * leftScale), Math.round((backRow.panel.x + backRow.panel.width) * leftScale), leftBelow), null, "the IDE is not on screen once its tab is left");
    const fits = FIT_STEPS.map(([step, picture]) => fit(log.find((row) => row.step === step), picture, ideDark));
    writeFileSync(join(evidence, "fit.json"), JSON.stringify(fits, null, 2));
    for (const row of fits) {
      assert.ok(row.off <= FIT_PX, `${row.step}: the IDE sits on its panel within ${FIT_PX} px: ${JSON.stringify(row)}`);
      assert.ok(row.clear, `${row.step}: the IDE covers neither the tab row nor the side rail: ${JSON.stringify(row)}`);
    }
  });

  it("a saved level ends the IDE's own window, the tab opens it again, and the Save's words match what happens to an unsaved edit", async () => {
    const project = await newProject("ide-own-level");
    await buildReady();
    await go(`#/projects/${encodeURIComponent(project)}/overview`);
    await until(() => browser.execute(() => Boolean(document.querySelector("#tab-ide"))), 60000, "the project page never listed the IDE tab");
    const moved = await moveOut(project);
    assert.ok(serverUp(project), "the server runs while the IDE is in its own window");
    raise("IDE");
    const focused = await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.view.focused && row.look !== null, "the IDE window never had the focus", OWN_WINDOW_MS, moved.at);
    const before = focused.look.editors;
    await commandKey("n");
    await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.look?.editors === before + 1, "Cmd+N opened no editor in the IDE window", OWN_WINDOW_MS, focused.at);
    await testAppInFront();
    onApp(`keystroke "${UNSAVED}"`);
    await pause(BACKUP_MS);
    shot("unsaved-before-restart");
    await startPlan([[LEAVE_S, "go", "#/"]]);
    const view = await invoke("desktop_project_agent_policy", { id: project });
    const access = view.access === "sandboxed" ? "own_branch" : "sandboxed";
    const saving = Date.now();
    const saved = await invoke("desktop_set_project_agent_policy", { id: project, version: view.version, policy: { access, network: view.network, toolchains: [], ide_git_hooks: false }, generation: ++generation });
    assert.equal(saved.outcome, "applied", JSON.stringify(saved));
    await until(() => !serverUp(project), SERVER_GONE_MS, "the server kept running after its level was saved");
    await rowWhere((row) => row.windows && !row.windows.some((label) => OWN_WINDOW.test(label)), "the own window stayed open after its server ended", WINDOW_GONE_MS, saving);
    await until(() => serverUp(project), REOPEN_MS, "the tab never opened the IDE again at the new level");
    const editors = () => windowRows().filter((row) => row.at > saving && row.view?.window === "main" && row.look && row.look.origin > focused.look.origin + SAME_PAGE_MS).at(-1)?.look.editors ?? -1;
    let count = editors();
    for (const deadline = Date.now() + RESTORE_MS; count !== before + 1 && Date.now() < deadline; await pause(POLL_MS)) count = editors();
    const back = count === before + 1;
    shot("unsaved-after-restart");
    writeFileSync(join(evidence, "unsaved-after-restart.txt"), `${back ? "returned" : "lost"} editors ${count} before ${before}\n`);
    const said = readFileSync(new URL("../../src/screens/project/access.tsx", import.meta.url), "utf8").includes("Unsaved edits are lost.");
    assert.equal(said, !back, `an unsaved edit was ${back ? "returned" : "lost"} after the restart, and the Save ${said ? "says" : "does not say"} it is lost`);
    await pause(Math.max(0, saving + LEAVE_S * 1000 + POLL_MS * 5 - Date.now()));
    await until(() => browser.execute(() => location.hash === "#/"), 60000, "the page never left the project");
  });

  it("moves to its own window with no reload, comes back, and closes with its project and the main window", async () => {
    const first = await newProject("ide-own-one");
    const second = await newProject("ide-own-two");
    const third = await newProject("ide-own-three");
    const fourth = await newProject("ide-own-four");
    await buildReady();
    await go(`#/projects/${encodeURIComponent(first)}/overview`);
    await until(() => browser.execute(() => Boolean(document.querySelector("#tab-ide"))), 60000, "the project page never listed the IDE tab");

    const moved = await moveOut(first);
    const inMain = windowRows().filter((row) => row.at < moved.at && row.view?.window === "main" && row.look?.origin > 0).at(-1);
    assert.ok(inMain, "the webview was seen in the main window before the move");
    samePage(moved, inMain, "the same page moved, with no reload");
    const shaped = await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.view.maximized && row.view.focused, "the window was never maximized with the focus", OWN_WINDOW_MS);
    assert.equal(shaped.view.fullscreen, false, "and it is not in native full screen");
    writeFileSync(join(evidence, "own-window-rows.json"), JSON.stringify({ inMain, moved }, null, 2));

    await until(() => browser.execute(() => document.querySelector(".ide-tab")?.textContent.includes("This IDE is in its own window")), OWN_WINDOW_MS, "the tab never said the IDE moved");
    const buttons = await browser.execute(() => [...document.querySelectorAll("button")].map((node) => node.textContent.trim()));
    assert.ok(buttons.includes("Bring it forward"), JSON.stringify(buttons));

    const settled = await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.view.ide.at[1] > 0 && row.look !== null, "the IDE never sat below the title bar", OWN_WINDOW_MS);
    const { ide, outer, scale } = settled.view;
    assert.equal(ide.at[0], 0, "the IDE starts at the window's left edge");
    assert.equal(ide.size[0], outer[0], "and fills its width");
    assert.equal(ide.at[1] + ide.size[1], outer[1], "and reaches its bottom");
    assert.equal(Math.round(settled.look.height * scale), ide.size[1], "the page has the webview's whole height");

    raise("IDE");
    await pause(POLL_MS);
    shot("own-window");
    await testAppInFront();
    onApp(`set size of (first window whose title contains "IDE") to {${SHOWN_WIDTH}, ${SHOWN_HEIGHT}}`);
    const resized = await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.view.outer[0] === SHOWN_WIDTH * scale && row.view.ide.size[0] === row.view.outer[0], "the IDE did not follow its window's new size", OWN_WINDOW_MS);
    assert.equal(resized.view.ide.at[1] + resized.view.ide.size[1], resized.view.outer[1], "after a resize it still reaches the bottom");
    assert.equal(resized.view.ide.at[1], ide.at[1], "and still sits below the title bar");
    shot("own-window-resized");
    await browser.saveScreenshot(join(evidence, "tab-while-out.png"));
    assert.ok(!menuNames().includes("File"), `the IDE window's menu owns none of the editor's keys: ${menuNames()}`);
    const before = (await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.view.focused && row.look !== null, "the IDE window never had the focus", OWN_WINDOW_MS)).look.editors;
    await commandKey("n");
    const opened = await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.look?.editors === before + 1, "Cmd+N opened no editor in the IDE window", OWN_WINDOW_MS);
    await commandKey("w");
    await rowWhere((row) => OWN_WINDOW.test(row.view?.window ?? "") && row.look?.editors === before, "Cmd+W closed no editor in the IDE window", OWN_WINDOW_MS, opened.at);
    assert.ok(windowRows().at(-1).windows.some((label) => OWN_WINDOW.test(label)), "Cmd+W left the window itself open");

    raise("CrazyAgent");
    await pause(POLL_MS);
    assert.ok(menuNames().includes("File"), `the main window gets its own menu back: ${menuNames()}`);
    await browser.execute(() => [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === "Bring it forward").click());
    await pause(POLL_MS);
    assert.ok(!menuNames().includes("File"), `Bring it forward gives the menu to the IDE window: ${menuNames()}`);

    await startPlan([[20, "go", `#/`]]);
    const closing = Date.now();
    await closeTitled("IDE");
    const home = await rowWhere((row) => row.view?.window === "main", "closing the window did not return the IDE to the main window", OWN_WINDOW_MS, closing);
    samePage(home, inMain, "the same page came back");
    assert.ok(!home.windows.some((label) => OWN_WINDOW.test(label)), "and the window has gone");
    await until(() => browser.execute(() => location.hash === "#/"), 60000, "the page never left the project");

    await go(`#/projects/${encodeURIComponent(fourth)}/overview`);
    await moveOut(fourth);
    assert.ok(serverUp(fourth), "the IDE's server runs while the IDE is in its own window");
    await go("#/");
    await pause(POLL_MS);
    assert.ok(serverUp(fourth), "leaving the page keeps the server for the window");
    const leaving = Date.now();
    await closeTitled("IDE");
    await rowWhere((row) => row.windows && !row.windows.some((label) => OWN_WINDOW.test(label)), "closing the window after the page left kept the window", OWN_WINDOW_MS, leaving);
    await until(() => !serverUp(fourth), OWN_WINDOW_MS, "closing the window after its page had left kept the IDE's server running");

    await go(`#/projects/${encodeURIComponent(second)}/overview`);
    await moveOut(second);
    assert.ok(serverUp(second), "the second IDE's server runs while the IDE is in its own window");
    await go("#/");
    await pause(POLL_MS);
    const removing = Date.now();
    const removal = await invoke("desktop_remove_project", { id: second, generation: ++generation, deleteName: null });
    assert.equal(removal.outcome, "applied", JSON.stringify(removal));
    await invoke("desktop_close_project_walls", { id: second });
    await rowWhere((row) => row.windows && !row.windows.some((label) => OWN_WINDOW.test(label)), "removing the project left its IDE window open", OWN_WINDOW_MS, removing);
    await until(() => !serverUp(second), OWN_WINDOW_MS, "removing a project whose page had closed left its IDE server running");

    await go(`#/projects/${encodeURIComponent(third)}/overview`);
    await moveOut(third);
    await browser.deleteSession();
    browser.sessionId = undefined;
    await closeTitled("CrazyAgent");
    for (const deadline = Date.now() + OWN_WINDOW_MS; spawnSync("/usr/bin/pgrep", ["-f", `^${app}`], { encoding: "utf8" }).stdout.trim() !== ""; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `closing the main window left a window open: ${JSON.stringify(windowRows().at(-1))}`);
    }
  });
});
