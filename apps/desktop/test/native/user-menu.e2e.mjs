import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const NAME = "E2E Owner";
const PAGE = "#/metrics";
const ITEMS = [["Profile", "profile"], ["Updates", "updates"], ["GitHub", "github"], ["Projects folder", "home"]];
const MENU = ".sidebar-foot [role=menuitem]";
const run = {};

/** Presses Esc with the real keyboard. Input: the page's send. Output: none. */
async function escape(send) {
  for (const type of ["keyDown", "keyUp"]) await send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
}

/** Reads what the owner sees of the user menu and its dialogs. Input: the page's evaluate.
 * Output: the hash, the menu items, the open dialogs' titles and the page heading. */
function state(evaluate) {
  return evaluate(`({
    hash: location.hash,
    items: [...document.querySelectorAll(${JSON.stringify(MENU)})].map((item) => item.textContent.trim()),
    dialogs: [...document.querySelectorAll("dialog[open] .dialog-head h2")].map((head) => head.textContent),
    heading: document.querySelector("main h1")?.textContent ?? null,
  })`);
}

/** Waits until the owner's view matches a check. Inputs: the page's evaluate, what is awaited
 * and the check. Output: the view. */
async function until(evaluate, what, check) {
  const deadline = Date.now() + 30000;
  for (;;) {
    const now = await state(evaluate);
    if (check(now)) return now;
    if (Date.now() > deadline) assert.fail(`${what}: ${JSON.stringify(now)}`);
    await new Promise((done) => setTimeout(done, 100));
  }
}

before(async () => {
  run.engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  run.bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  run.root = realpathSync(mkdtempSync(join(tmpdir(), "ca-um-")));
  run.evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(run.root, "evidence");
  const runtime = join(run.root, "engine");
  const home = join(run.root, "home");
  for (const folder of [runtime, run.evidence, home, join(home, ".codex")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
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
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  run.log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  ({ child: run.engine } = await launch(run.engineBinary,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
    { ...env }, run.evidence, "engine", /^ready$/m));
  const started = await launch(run.bridgeBinary, [join(runtime, "engine.sock")], { ...env }, run.evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  run.bridgeProcess = started.child;
  run.bridge = { port: Number(started.match[1]), token: started.match[2] };
  assert.equal((await value(run.bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
  assert.equal((await value(run.bridge, "desktop_save_owner_profile", { profile: { name: NAME }, generation: 1 })).outcome, "applied");
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

test("the name opens the user menu; each item opens its dialog over the page and Esc closes back to it; a deep link does the same; Settings no longer lists the four", { timeout: 300000 }, async () => {
  await withPage(async ({ evaluate, send }) => {
    const me = owner({ evaluate, send }, run.evidence);
    try {
      await send("Page.enable");
      await send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
      await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
      await send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/${PAGE}` });
      const start = await until(evaluate, "the Metrics page", (now) => now.heading !== null && now.hash === PAGE);
      for (const [label, tab] of ITEMS) {
        await me.follow(".sidebar-foot .me");
        await until(evaluate, "the user menu", (now) => now.items.join("|") === [...ITEMS.map(([item]) => item), "Restart engine"].join("|"));
        assert.equal((await state(evaluate)).hash, PAGE, "opening the menu navigates nowhere");
        await me.click(label, MENU);
        const open = await until(evaluate, `the ${label} dialog`, (now) => now.dialogs.length === 1 && now.dialogs[0] === label);
        assert.equal(open.hash, PAGE, `${label} opens over the page`);
        assert.deepEqual(open.items, [], "the menu closes under the dialog");
        await me.capture(`user-menu-${tab}`);
        await escape(send);
        const back = await until(evaluate, `${label} closed`, (now) => now.dialogs.length === 0);
        assert.deepEqual({ hash: back.hash, heading: back.heading }, { hash: PAGE, heading: start.heading }, `${label} closes back to the same page`);
      }
      await me.follow(".sidebar-foot .me");
      await me.click("Restart engine", MENU);
      await until(evaluate, "the restart question", (now) => now.dialogs.join() === "Restart the engine?");
      await me.click("Cancel", "dialog[open] button");
      await until(evaluate, "the restart question closed", (now) => now.dialogs.length === 0);
      await send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/settings/github` });
      const linked = await until(evaluate, "the deep-linked GitHub dialog", (now) => now.dialogs.join() === "GitHub" && now.hash === PAGE);
      assert.equal(linked.heading, start.heading, "the deep link keeps the page under its dialog");
      await me.capture("user-menu-deep-link");
      await escape(send);
      await until(evaluate, "the deep-linked dialog closed", (now) => now.dialogs.length === 0 && now.hash === PAGE);
      await send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/settings/accounts` });
      await me.waitFor("the Settings list", (text) => text.includes("Accounts") && text.includes("Sandbox"));
      const listed = await evaluate(`[...document.querySelectorAll('[aria-label="Settings sections"] a')].map((link) => link.textContent.trim())`);
      for (const [label] of ITEMS) assert.ok(!listed.includes(label), `${label} left the Settings list: ${listed.join(", ")}`);
      assert.deepEqual(await evaluate("window.__e2eErrors"), [], "the page raised no errors");
    } catch (failure) {
      await me.capture("user-menu-failure").catch(() => {});
      throw failure;
    }
  });
});

test("a deep link from a project section opens its dialog and puts that exact address back, section included", { timeout: 300000 }, async () => {
  const folder = join(run.root, "projects");
  mkdirSync(folder, { mode: 0o700 });
  assert.equal((await value(run.bridge, "desktop_set_projects_home", { path: folder, generation: 2 })).outcome, "applied");
  const receipt = await value(run.bridge, "desktop_setup_project", {
    spec: { name: "Menu Page", repositories: [] },
    setup: { about: "A page under the user menu", tech_stack: [], platforms: ["website"], research: [], pool: "local-pool", team: null, documents: [] },
    generation: 3,
  });
  assert.equal(receipt.outcome, "applied", `the project is made: ${JSON.stringify(receipt)}`);
  const section = `#/projects/${encodeURIComponent(receipt.project_id)}/settings?section=remove`;
  await withPage(async ({ evaluate, send }) => {
    const me = owner({ evaluate, send }, run.evidence);
    try {
      await send("Page.enable");
      await send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
      await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
      await send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/${section}` });
      await me.waitFor("the project's Remove section", (text) => text.includes("Remove project"));
      await send("Page.navigate", { url: `http://127.0.0.1:${run.site.port}/#/settings/profile` });
      await until(evaluate, "the Profile dialog over the section", (now) => now.dialogs.join() === "Profile" && now.hash === section);
      await me.capture("user-menu-over-section");
      await escape(send);
      await until(evaluate, "the dialog closed on the same section", (now) => now.dialogs.length === 0 && now.hash === section);
      await me.waitFor("the Remove section still shown", (text) => text.includes("Remove project"));
      assert.deepEqual(await evaluate("window.__e2eErrors"), [], "the page raised no errors");
    } catch (failure) {
      await me.capture("user-menu-section-failure").catch(() => {});
      throw failure;
    }
  });
});
