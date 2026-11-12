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
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const TITLE = "What reaches you, and how long finished items stay.";
const FORM = 'form[aria-label="Notifications"]';
const SAVE = `${FORM} button[type=submit]`;
const BELL = "button.notice-bell";
const PANEL = '[role=dialog][aria-label="Notifications"]';
const KIND = (kind) => `fieldset[data-kind="${kind}"] label`;
const BANNERS = 'fieldset[data-setting="banners"] label';
const SHOWN = `(() => {
  const picked = (name) => document.querySelector('input[type=radio][name="' + name + '"]:checked')?.value ?? null;
  const days = document.getElementById("notice-retention-days");
  return [picked("delivery-app_update_ready"), picked("delivery-question"), picked("banners"), days?.value ?? null];
})()`;

/** Runs the owner's git in a folder with no global or system config. Inputs: the folder and
 * the arguments. Output: what git printed, trimmed. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
async function until(what, wanted, read) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * serves the built app to a browser page, runs the steps and stops everything. Inputs: the
 * test context, the temporary folder prefix and the steps, given the bridge, the owner on the
 * page, a route opener, a page reload, the project id and an engine restart. Output: none. */
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
  const repo = join(root, "repos", "tool-shop");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(repo, "README.md"), "# Tool shop\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the tool shop"]]) git(repo, ...args);
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
  const engineArgs = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"];
  try {
    ({ child: engine } = await launch(engineBinary, engineArgs, { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2], generation: 10 };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool: "local-pool", generation: 3 });
    assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: receipt.project_id });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    let starts = 0;
    const restart = async () => {
      starts += 1;
      await stop(bridgeProcess);
      await stop(engine);
      ({ child: engine } = await launch(engineBinary, engineArgs, { ...env }, evidence, `engine-${starts}`, /^ready$/m));
      const again = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, `bridge-${starts}`, /^ready (\d+) ([0-9a-f]{64})$/m);
      bridgeProcess = again.child;
      bridge.port = Number(again.match[1]);
      bridge.token = again.match[2];
      assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the engine started again");
    };

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
        let visits = 0;
        const clean = async () => assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
        const open = async (hash, what, check) => {
          await clean();
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}${hash}` });
          await me.waitFor(what, check);
        };
        const reload = async (words) => {
          await clean();
          await page.evaluate("window.__e2eBeforeReload = true");
          await page.send("Page.reload", { ignoreCache: true });
          const fresh = `window.__e2eBeforeReload !== true && document.body.innerText.includes(${JSON.stringify(words)})`;
          for (const deadline = Date.now() + STEP_MS; !(await page.evaluate(fresh).catch(() => false)); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, `the reloaded page never showed ${words}`);
          }
        };
        await steps({ bridge, me, page, open, reload, restart, projectId: receipt.project_id });
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
    await endTerminals(runtime);
  }
  assert.deepEqual(terminalServers(runtime), [], "no tmux server outlives the test's engine");
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Sends one change to the store through the bridge as the page would, each with a fresh
 * generation. Inputs: the bridge and the change. Output: the receipt. */
async function act(bridge, action) {
  bridge.generation += 1;
  return value(bridge, "desktop_notification_action", { action, generation: bridge.generation });
}

/** Reports an app update ready to install, which records one App update ready item while
 * that kind is not Off. Inputs: the bridge and the version. Output: none. */
async function updateReady(bridge, version) {
  const receipt = await act(bridge, { report: { report: { kind: "app_update_ready", version } } });
  assert.equal(receipt.outcome, "applied", `the report is taken: ${JSON.stringify(receipt)}`);
}

/** Reads every item the store lists unfolded, newest first. Input: the bridge. Output: each
 * item's kind, text, unread flag and banner flag. */
async function items(bridge) {
  const page = await value(bridge, "desktop_notifications", { query: { filter: "all", project_id: null, kind: null, fold: false, before: null, after: null, limit: 50 } });
  return page.items.map((item) => [item.kind, item.text, item.unread, item.banner]);
}

/** The stored settings as the engine reads them. Input: the bridge. Output: App update
 * ready's delivery, Question's delivery, the banners choice and the retention days. */
async function stored(bridge) {
  const settings = await value(bridge, "desktop_notification_settings");
  const delivery = (kind) => settings.kinds.find((item) => item.kind === kind)?.delivery ?? null;
  return [delivery("app_update_ready"), delivery("question"), settings.banners, settings.retention_days];
}

/** Opens Settings, Notifications and waits for every kind and the macOS choices. Inputs: the
 * owner and the route opener. Output: none. */
async function settingsPage(me, open) {
  await open("#/settings/notifications", "the Notifications setting", (text) => text.includes(TITLE));
  await me.waitFor("every kind and the macOS choices", (text) => ["Question", "App update ready", "Team alert", "Action items only", "Keep finished items for"].every((words) => text.includes(words)));
}

/** Types the days Done items stay into the retention field, as the owner's typing does.
 * Inputs: the page and the text. Output: none. */
async function days(page, text) {
  const typed = await page.evaluate(`(() => {
    const field = document.getElementById("notice-retention-days");
    if (!field || field.disabled) return false;
    field.focus();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(field, ${JSON.stringify(text)});
    field.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  })()`);
  assert.ok(typed, "the days field takes typing");
}

/** The bell's name for a screen reader, which carries both counts. Input: the page. Output:
 * the aria-label, or null when no bell is shown. */
async function bell(page) {
  return page.evaluate(`document.querySelector(${JSON.stringify(BELL)})?.getAttribute("aria-label") ?? null`);
}

test("Notifications: an Info kind left On shows its item in the bell on every page with the unread badge and a macOS banner", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-on-", async ({ bridge, me, page, open, projectId }) => {
  await open("#/home", "Home", (text) => text.includes("Projects"));
  await until("the bell with nothing new", "Notifications: nothing new", () => bell(page));
  await until("App update ready On by default", ["on", "on", "all", 30], () => stored(bridge));
  await updateReady(bridge, "9.1.0");
  await until("the item with a banner", [["app_update_ready", "CrazyAgent 9.1.0 is ready to install.", true, true]], () => items(bridge));
  await until("the unread badge on Home", "Notifications: 1 unread, 0 pending", () => bell(page));
  await open(`#/projects/${encodeURIComponent(projectId)}/overview`, "the project page", (text) => text.includes("Overview"));
  await until("the unread badge on a project page", "Notifications: 1 unread, 0 pending", () => bell(page));
  await open("#/settings/accounts", "Settings", (text) => text.includes("Accounts"));
  await until("the unread badge in Settings", "Notifications: 1 unread, 0 pending", () => bell(page));
  await me.capture("notifications-on");
}));

test("Notifications: Bell only for an Info kind keeps its items in the bell with no macOS banner, and stays after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-bell-only-", async ({ bridge, me, page, open, reload, restart }) => {
  await settingsPage(me, open);
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null`), true, "Save waits for a change");
  await me.click("Bell only", KIND("app_update_ready"));
  await me.click("Save changes", SAVE);
  await until("Bell only in the engine", ["bell_only", "on", "all", 30], () => stored(bridge));
  await reload(TITLE);
  await until("Bell only after a reload", ["bell_only", "on", "all", "30"], () => page.evaluate(SHOWN));
  await restart();
  await until("Bell only after an engine restart", ["bell_only", "on", "all", 30], () => stored(bridge));
  await updateReady(bridge, "9.2.0");
  await until("the item without a banner", [["app_update_ready", "CrazyAgent 9.2.0 is ready to install.", true, false]], () => items(bridge));
  await until("the unread badge", "Notifications: 1 unread, 0 pending", () => bell(page));
  await me.capture("notifications-bell-only");
}));

test("Notifications: Off for an Info kind records none of its items, and stays after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-off-", async ({ bridge, me, page, open, reload, restart }) => {
  await settingsPage(me, open);
  await me.click("Off", KIND("app_update_ready"));
  await me.click("Save changes", SAVE);
  await until("Off in the engine", ["off", "on", "all", 30], () => stored(bridge));
  await reload(TITLE);
  await until("Off after a reload", ["off", "on", "all", "30"], () => page.evaluate(SHOWN));
  await restart();
  await until("Off after an engine restart", ["off", "on", "all", 30], () => stored(bridge));
  await updateReady(bridge, "9.3.0");
  await pause(STEP_MS / 10);
  assert.deepEqual(await items(bridge), [], "an Off kind records nothing");
  await until("the bell with nothing new", "Notifications: nothing new", () => bell(page));
  await me.click("On", KIND("app_update_ready"));
  await me.click("Save changes", SAVE);
  await until("On again in the engine", ["on", "on", "all", 30], () => stored(bridge));
  await updateReady(bridge, "9.3.1");
  await until("the item once the kind is On again", [["app_update_ready", "CrazyAgent 9.3.1 is ready to install.", true, true]], () => items(bridge));
  await me.capture("notifications-off");
}));

test("Notifications: an Action kind offers On and Bell only, never Off, and the engine refuses Off for it", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-action-", async ({ bridge, me, page, open }) => {
  await settingsPage(me, open);
  const offered = await page.evaluate(`[...document.querySelectorAll('input[type=radio][name="delivery-question"]')].map((input) => input.value)`);
  assert.deepEqual(offered, ["on", "bell_only"], "Question offers no Off");
  await me.click("Bell only", KIND("question"));
  await me.click("Save changes", SAVE);
  await until("Bell only for Question in the engine", ["on", "bell_only", "all", 30], () => stored(bridge));
  const settings = await value(bridge, "desktop_notification_settings");
  const refused = await act(bridge, { save_settings: { settings: { ...settings, kinds: settings.kinds.map((item) => item.kind === "question" ? { ...item, delivery: "off" } : item) } } });
  assert.notEqual(refused.outcome, "applied", `Off for an Action kind is refused: ${JSON.stringify(refused)}`);
  await until("Bell only kept for Question", ["on", "bell_only", "all", 30], () => stored(bridge));
  await me.capture("notifications-action-no-off");
}));

test("Notifications: the Done retention takes whole days from 1 to 365, saves 7 and keeps it after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-retention-", async ({ bridge, me, page, open, reload, restart }) => {
  await settingsPage(me, open);
  await days(page, "366");
  await me.waitFor("the days refused before anything is sent", (text) => text.includes("Enter a whole number of days from 1 to 365."));
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null`), true, "Save stays off for 366 days");
  await days(page, "7");
  await me.click("Save changes", SAVE);
  await until("7 days in the engine", ["on", "on", "all", 7], () => stored(bridge));
  await reload(TITLE);
  await until("7 days after a reload", ["on", "on", "all", "7"], () => page.evaluate(SHOWN));
  await restart();
  await until("7 days after an engine restart", ["on", "on", "all", 7], () => stored(bridge));
  await me.capture("notifications-retention");
}));

test("Notifications: macOS notifications Off sends no banner for any item, and stays after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-banners-off-", async ({ bridge, me, page, open, reload, restart }) => {
  await settingsPage(me, open);
  await me.click("Off", BANNERS);
  await me.click("Save changes", SAVE);
  await until("banners Off in the engine", ["on", "on", "off", 30], () => stored(bridge));
  await reload(TITLE);
  await until("banners Off after a reload", ["on", "on", "off", "30"], () => page.evaluate(SHOWN));
  await restart();
  await until("banners Off after an engine restart", ["on", "on", "off", 30], () => stored(bridge));
  await updateReady(bridge, "9.4.0");
  await until("the item without a banner", [["app_update_ready", "CrazyAgent 9.4.0 is ready to install.", true, false]], () => items(bridge));
  await me.capture("notifications-banners-off");
}));

test("Notifications: macOS notifications for Action items only sends no banner for an Info item", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-banners-action-", async ({ bridge, me, open }) => {
  await settingsPage(me, open);
  await me.click("Action items only", BANNERS);
  await me.click("Save changes", SAVE);
  await until("banners Action only in the engine", ["on", "on", "action_only", 30], () => stored(bridge));
  await updateReady(bridge, "9.5.0");
  await until("the Info item without a banner", [["app_update_ready", "CrazyAgent 9.5.0 is ready to install.", true, false]], () => items(bridge));
  await me.capture("notifications-banners-action-only");
}));

test("Notifications: macOS notifications for All is the default, and picking it again after Off brings banners back", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-banners-all-", async ({ bridge, me, page, open, reload }) => {
  await settingsPage(me, open);
  await until("All on the page by default", ["on", "on", "all", "30"], () => page.evaluate(SHOWN));
  await me.click("Off", BANNERS);
  await me.click("Save changes", SAVE);
  await until("banners Off in the engine", ["on", "on", "off", 30], () => stored(bridge));
  await me.click("All", BANNERS);
  await me.click("Save changes", SAVE);
  await until("banners All in the engine", ["on", "on", "all", 30], () => stored(bridge));
  await reload(TITLE);
  await until("All after a reload", ["on", "on", "all", "30"], () => page.evaluate(SHOWN));
  await updateReady(bridge, "9.6.0");
  await until("the item with a banner", [["app_update_ready", "CrazyAgent 9.6.0 is ready to install.", true, true]], () => items(bridge));
  await me.capture("notifications-banners-all");
}));

test("Notifications: a click on an item in the panel opens its exact page and marks it read; Mark all read and Delete work on what is picked", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-panel-", async ({ bridge, me, page, open }) => {
  await open("#/home", "Home", (text) => text.includes("Projects"));
  await updateReady(bridge, "9.7.0");
  await act(bridge, { report: { report: { kind: "engine_lost" } } });
  await until("two unread", "Notifications: 2 unread, 0 pending", () => bell(page));
  await me.follow(BELL);
  await me.waitFor("the panel with both items", (text) => ["App update ready", "CrazyAgent 9.7.0 is ready to install.", "Engine lost", "Mark all read"].every((words) => text.includes(words)));
  await me.click("Unread", `${PANEL} label`);
  await me.waitFor("the Unread filter", (text) => text.includes("CrazyAgent 9.7.0 is ready to install."));
  await me.click("All", `${PANEL} label`);
  const update = (await value(bridge, "desktop_notifications", { query: { filter: "all", project_id: null, kind: "app_update_ready", fold: false, before: null, after: null, limit: 50 } })).items[0];
  await me.follow(`${PANEL} [data-notice="${update.id}"] .notice-open`);
  await until("the Updates page", true, () => page.evaluate(`location.hash.startsWith("#/settings/updates")`));
  await until("the update read", [false], async () => (await items(bridge)).filter(([kind]) => kind === "app_update_ready").map(([, , unread]) => unread));
  await until("one unread", "Notifications: 1 unread, 0 pending", () => bell(page));
  await me.follow(BELL);
  await me.click("Mark all read", `${PANEL} button`);
  await until("nothing unread", "Notifications: nothing new", () => bell(page));
  await me.follow(`${PANEL} [data-notice="${update.id}"] input[type=checkbox]`);
  await me.waitFor("the picked count", (text) => text.includes("1 selected"));
  await me.click("Delete", `${PANEL} button`);
  await until("the update deleted", ["engine_lost"], async () => (await items(bridge)).map(([kind]) => kind));
  await me.capture("notifications-panel");
}));

test("Notifications: Home has no Needs you strip and the bell is on every page", { timeout: 600000 }, (t) => onEngine(t, "ca-notice-home-", async ({ me, page, open }) => {
  await open("#/home", "Home", (text) => text.includes("Projects"));
  assert.equal(await page.evaluate(`document.querySelector('section[aria-label="Needs you"]') === null`), true, "Home shows no Needs you strip");
  for (const hash of ["#/home", "#/metrics", "#/settings/notifications", "#/settings/profile"]) {
    await open(hash, hash, () => true);
    await until(`the bell on ${hash}`, true, () => page.evaluate(`document.querySelector(${JSON.stringify(BELL)}) !== null`));
  }
  await me.capture("notifications-home");
}));
