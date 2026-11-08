import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { AGENTS_VIEW, INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SETTING = "Terminals per window";
const AGENTS = 7;

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Booking\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the booking app"]]) {
    const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
    assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  }
}

/** Writes the stand-in Codex executable the engine is started with; it exits at once, so no
 * model ever runs. Input: the test root. Output: its absolute path. */
function standInHarness(root) {
  mkdirSync(join(root, "bin"), { recursive: true, mode: 0o700 });
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** Puts resting sub-agents in the engine's team table, each counted as running with a
 * terminal name and run by no worker. Inputs: the engine root, the project and how many.
 * Output: their ids. */
function restingAgents(runtime, project, count) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const insert = store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,turns,parent) VALUES (?,'sub','codex',1,'idle',?,?,1,NULL)");
    const ids = Array.from({ length: count }, () => Number(insert.run(project, now, now).lastInsertRowid));
    assert.ok(ids.every((id) => Number.isSafeInteger(id) && id > 0), "the engine numbers each team agent");
    return ids;
  } finally {
    store.close();
  }
}

/** The tiles a wall shows, read from the engine's graph: every node with a terminal, the
 * master first, then the rest in graph order. Input: the graph. Output: each tile's label and
 * whether it is the master's. */
function tiles(graph) {
  const open = graph.nodes.filter((node) => node.terminal !== null);
  return [...open.filter((node) => node.kind === "master"), ...open.filter((node) => node.kind !== "master")]
    .map((node) => ({ label: node.label, master: node.kind === "master" }));
}

/** Reads the wall window the page shows: its heading, its tiles in order and its free slots.
 * Input: the page. Output: what it shows. */
function wallShown(page) {
  return page.evaluate(`(() => ({
    heading: document.querySelector(".tw-head h1")?.textContent.trim() ?? null,
    tiles: [...document.querySelectorAll(".tw-tile section.tg-terminal")].map((panel) => panel.getAttribute("aria-label").replace(/ terminal$/, "")),
    free: document.querySelectorAll(".tw-free").length,
    stops: [...document.querySelectorAll(".tw-tile [aria-label^='Stop ']")].length,
    messages: [...document.querySelectorAll(".tw-tile [aria-label^='Message ']")].length,
  }))()`);
}

/** Starts the real engine with the stand-in Codex and the bridge on a new project with seven
 * resting agents, serves the built app to a browser page on the project's Agents tab, runs
 * the steps and stops everything. Inputs: the test context, the temporary folder prefix and
 * the steps, given the project, the owner on the page, the page, a page reload, the team
 * status reader, the graph's tiles, the site's address and the call log. Output: none. */
async function onEngine(t, prefix, steps) {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), prefix)));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? join(process.env.E2E_EVIDENCE, prefix) : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "booking");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  ownerRepo(repo);
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
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings", "--codex-executable", standInHarness(root)],
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
    assert.ok(existsSync(join(runtime, "engine.db")), "the engine keeps its database in its root");
    restingAgents(runtime, project, AGENTS);
    const shownTiles = tiles(await value(bridge, "desktop_team_graph", { id: project }));
    assert.ok(shownTiles.length >= AGENTS, `every resting agent has a terminal: ${JSON.stringify(shownTiles)}`);
    const status = () => value(bridge, "desktop_team_status", { id: project });

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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/agents?section=coord` });
        await me.waitFor("the Terminals per window setting in Agent settings", (text) => text.includes(SETTING));
        const reload = async () => {
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(SETTING)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, "the reloaded page never showed the Terminals per window setting");
          }
        };
        const graph = () => value(bridge, "desktop_team_graph", { id: project });
        await steps({ project, me, page, reload, status, graph, runtime, tiles: shownTiles, site, log });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.ok(native.has("desktop_open_terminal_windows"), "opening windows is native-only, so this run proves the request and each window's page, not the native windows");
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    t.diagnostic(`native-only commands the page called and this test cannot run: ${[...log.unrouted].join(", ") || "none"}`);
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the page sent");
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    writeFileSync(join(evidence, "trail.json"), JSON.stringify(log.trail.map(({ at, ms, cmd, args }) => ({ at, ms, cmd, id: args?.id ?? null })), null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Clicks one choice of the Terminals per window setting, as the owner does. Inputs: the
 * owner on the page and the choice's label. Output: none. */
function pick(me, label) {
  return me.click(label, `[role=radiogroup][aria-label="${SETTING}"] [role=radio]`);
}

/** Reads which choice of the setting Agent settings shows as picked. Input: the page.
 * Output: the picked choice's label, or null. */
function shown(page) {
  return page.evaluate(`document.querySelector(${JSON.stringify(`[role=radiogroup][aria-label="${SETTING}"] [role=radio][aria-checked=true]`)})?.innerText.trim() ?? null`);
}

/** Waits until the engine's team status holds the setting as given. Inputs: the status
 * reader and the choice. Output: none. */
async function kept(status, perWindow) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const held = (await status()).terminals_per_window;
    if (held === perWindow) return;
    assert.ok(Date.now() < deadline, `the engine never kept ${perWindow} terminals per window; it holds ${JSON.stringify(held)}`);
  }
}

/** Opens the Team view, clicks Open all terminals and checks the one request it sends, then
 * opens each window's page as the native window would and checks its heading, its tiles in
 * order, their Message and Stop buttons and the free slots of the last window. Inputs: the
 * steps' context, the choice and the hint the button shows. Output: none. */
async function openAll({ project, me, page, tiles: all, site, log }, perWindow, hint) {
  const windows = Math.ceil(all.length / perWindow);
  await me.click("Team", AGENTS_VIEW);
  await me.waitFor("Open all terminals on the Team view", (text) => text.includes("Open all terminals"));
  const title = `[...document.querySelectorAll("button")].find((button) => button.innerText.includes("Open all terminals"))?.title ?? null`;
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const shown = await page.evaluate(title);
    if (shown === hint) break;
    assert.ok(Date.now() < deadline, `Open all terminals never read ${JSON.stringify(hint)}: ${JSON.stringify(shown)}`);
  }
  const before = log.trail.length;
  await me.click("Open all terminals", "button");
  for (const deadline = Date.now() + STEP_MS; !log.trail.slice(before).some((call) => call.cmd === "desktop_open_terminal_windows"); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, "Open all terminals never asked for the windows");
  }
  const opened = log.trail.slice(before).filter((call) => call.cmd === "desktop_open_terminal_windows");
  assert.equal(opened.length, 1, "one click sends one request for every window");
  assert.equal(opened[0].args.id, project);
  assert.equal(opened[0].args.count, windows, `${all.length} terminals at ${perWindow} a window take ${windows} windows`);
  assert.equal(opened[0].args.focus, true);
  assert.ok(typeof opened[0].args.name === "string" && opened[0].args.name.length > 0, "the request names the project for the window titles");
  for (let number = 1; number <= windows; number += 1) {
    const slice = all.slice((number - 1) * perWindow, number * perWindow);
    const mark = log.trail.length;
    await openWall(page, site, project, number);
    for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
      const wall = await wallShown(page).catch(() => null);
      if (wall?.tiles.length === slice.length) break;
      assert.ok(Date.now() < deadline, `window ${number} never drew its ${slice.length} tiles: ${JSON.stringify(wall)}`);
    }
    const wall = await wallShown(page);
    assert.equal(wall.heading, `Terminals ${number} of ${windows}`);
    assert.deepEqual(wall.tiles, slice.map((tile) => tile.label), `window ${number} shows its terminals in order, the master first`);
    assert.equal(wall.free, perWindow - slice.length, "only the last window has free slots, up to the setting");
    assert.equal(wall.messages, slice.length, "each tile has Message");
    assert.equal(wall.stops, slice.filter((tile) => !tile.master).length, "each agent's tile has Stop; the master's has none");
    const extend = await asked(log, mark, windows);
    assert.ok(extend.every((call) => call.args.id === project), `window ${number} asks only for its own project's windows: ${JSON.stringify(extend)}`);
    const attaches = () => log.trail.slice(mark).filter((call) => call.cmd === "desktop_terminal_attach");
    for (const deadline = Date.now() + STEP_MS; attaches().length < slice.length; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, `window ${number} attached ${attaches().length} of its ${slice.length} terminals`);
    }
    assert.ok(attaches().every((call) => call.args.id === project), `window ${number} attaches naming its own project: ${JSON.stringify(attaches().map((call) => call.args))}`);
    await me.capture(`wall-${perWindow}-${number}-of-${windows}`);
  }
  const attached = log.trail.slice(before).filter((call) => call.cmd === "desktop_terminal_attach").length;
  assert.equal(attached, all.length, "each window asks to attach each of its terminals once");
  assert.ok(log.trail.slice(before).some((call) => call.cmd === "desktop_terminals_per_window"), "each window reads the setting");
}

let wallLoads = 0;

/** Opens one wall window's page as a new document, the way its native window starts, and
 * waits until the page before it is gone, so the page boots on the wall and never reloads the
 * route it showed before. Inputs: the page, the site, the project and the window number.
 * Output: none. */
async function openWall(page, site, project, number) {
  wallLoads += 1;
  await page.evaluate("window.__e2eLeaving = true");
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?load=${wallLoads}#/terminals/${encodeURIComponent(project)}/${number}` });
  for (const deadline = Date.now() + STEP_MS; await page.evaluate("window.__e2eLeaving === true").catch(() => true); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `window ${number}'s page never replaced the page before it`);
  }
}

/** Waits until a wall window has asked the bridge for its project's windows with the given
 * count, since a point in the call log. Inputs: the call log, the point and the count.
 * Output: every such request since the point. */
async function asked(log, mark, count) {
  const requests = () => log.trail.slice(mark).filter((call) => call.cmd === "desktop_extend_terminal_windows");
  for (const deadline = Date.now() + STEP_MS; !requests().some((call) => call.args.count === count); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `no wall window asked for ${count} windows: ${JSON.stringify(requests().map((call) => call.args))}`);
  }
  return requests();
}

/** With only the first wall window on the page and the main window on no page at all, puts
 * one agent more in the team than the open windows hold and checks the wall itself asks the
 * bridge for one more window of its own project, while nothing asks to open windows from the
 * main window. Inputs: the steps' context and the choice. Output: none. */
async function grows({ project, page, graph, runtime, site, log }, perWindow) {
  const before = tiles(await graph()).length;
  const windows = Math.ceil(before / perWindow);
  const mark = log.trail.length;
  await openWall(page, site, project, 1);
  await asked(log, mark, windows);
  restingAgents(runtime, project, windows * perWindow - before + 1);
  const after = tiles(await graph()).length;
  assert.equal(Math.ceil(after / perWindow), windows + 1, `${after} terminals at ${perWindow} a window take one window more`);
  const grown = await asked(log, mark, windows + 1);
  assert.ok(grown.every((call) => call.args.id === project), `the wall asks only for its own project's windows: ${JSON.stringify(grown)}`);
  assert.equal(log.trail.slice(mark).filter((call) => call.cmd === "desktop_open_terminal_windows").length, 0, "nothing asks the main window's way; the wall grows on its own");
}

test("Terminals per window 4: the owner's pick is kept, and Open all terminals asks for two windows of four, each window drawing its terminals and free slots and asking for one window more as the team grows", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-tw4-", async (context) => {
    await pick(context.me, "4");
    await kept(context.status, 4);
    await context.reload();
    assert.equal(await shown(context.page), "4", "4 is shown again after a reload");
    await openAll(context, 4, "Opens 2 windows, 4 terminals each.");
    await grows(context, 4);
  });
});

test("Terminals per window 6 (default): six is the pick until the owner changes it, and Open all terminals asks for two windows of six, and the wall grows on its own", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-tw6-", async (context) => {
    assert.equal((await context.status()).terminals_per_window, 6, "6 is the default");
    assert.equal(await shown(context.page), "6 (default)", "the tab shows the default");
    await pick(context.me, "9");
    await kept(context.status, 9);
    await pick(context.me, "6 (default)");
    await kept(context.status, 6);
    await context.reload();
    assert.equal(await shown(context.page), "6 (default)", "6 is shown again after a reload");
    await openAll(context, 6, "Opens 2 windows, 6 terminals each.");
    await grows(context, 6);
  });
});

test("Terminals per window 9: the owner's pick is kept, and Open all terminals asks for one window holding every terminal, and the wall grows on its own", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-tw9-", async (context) => {
    await pick(context.me, "9");
    await kept(context.status, 9);
    await context.reload();
    assert.equal(await shown(context.page), "9", "9 is shown again after a reload");
    await openAll(context, 9, "Opens 1 window of up to 9 terminals.");
    await grows(context, 9);
  });
});
