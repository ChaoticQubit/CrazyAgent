import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, endTerminals, owner, required, serveApp, start as launch, stop, terminalServers, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const GLOBAL = `# the owner's own notes
version = 1
timeout_ms = 2000 # keep this
allowed_destinations = []
`;
const SECTION = "When a plugin is turned off";
const STOP_CARD = "Stop runs using it Runs using the plugin are stopped.";
const ESSENTIAL_NOTE = "This plugin stays on. CrazyAgent needs it to save what the agents were doing when the app quits.";
const SHOWN = `(() => document.querySelector('input[type=radio][name="app-plugin-when-off"]:checked')?.value ?? null)()`;
const LOCKED = `(() => document.querySelectorAll('ul[aria-label="Built-in code plugins"] button[role=switch][id^="app-plugin-"][aria-disabled="true"][aria-checked="true"]').length)()`;

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
async function until(what, wanted, read) {
  for (const deadline = Date.now() + STEP_MS; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Starts the real engine with writable settings and the bridge, serves the built app to a
 * browser page, runs the steps and stops everything. Inputs: the test context, the temporary
 * folder prefix and the steps, given the bridge, the owner on the page, a route opener, the
 * global settings file and an engine restart. Output: none. */
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
  for (const folder of [runtime, evidence, home, projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const global = join(runtime, "global.toml");
  writeFileSync(global, GLOBAL, { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  const engineArgs = ["engine", runtime, global, join(runtime, "project.toml"), "engine.sock", "--writable-settings"];
  let engine;
  let bridgeProcess;
  let site;
  try {
    ({ child: engine } = await launch(engineBinary, engineArgs, { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
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
        const open = async () => {
          assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}#/settings/app-plugins` });
          await me.waitFor("the App plugins settings", (text) => text.includes(SECTION) && text.includes("Refuse new calls"));
        };
        await steps({ bridge, me, page, open, global, restart });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
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

/** The turn-off choice as the engine reads it. Input: the bridge. Output: the choice and
 * whether a project file overrides it. */
async function stored(bridge) {
  const page = await value(bridge, "desktop_app_plugins");
  return [page.when_turned_off.value, page.when_turned_off.project_override];
}

test("App plugins: Stop runs using it is saved to the global settings file with the owner's other lines kept, and stays after an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-app-plugins-when-off-", async ({ bridge, me, page, open, global, restart }) => {
  await open();
  await until("the default in the engine", ["refuse_new_calls", false], () => stored(bridge));
  await until("the default on the page", "refuse_new_calls", () => page.evaluate(SHOWN));
  await me.click(STOP_CARD, "label.radio-card");
  await until("Stop runs using it in the engine", ["stop_runs", false], () => stored(bridge));
  const saved = readFileSync(global, "utf8");
  assert.ok(saved.startsWith(GLOBAL), `the owner's lines stay byte for byte: ${saved}`);
  assert.ok(saved.includes('[plugins]\nwhen_turned_off = "stop_runs"\n'), `the choice is in the file: ${saved}`);
  await restart();
  await until("Stop runs using it after an engine restart", ["stop_runs", false], () => stored(bridge));
  await open();
  await until("Stop runs using it on the page after the restart", "stop_runs", () => page.evaluate(SHOWN));
  await me.capture("app-plugins-when-off");
}));

test("App plugins: the essential built-in plugin is listed with its switch locked on and the reason shown", { timeout: 600000 }, (t) => onEngine(t, "ca-app-plugins-essential-", async ({ me, page, open }) => {
  await open();
  await me.waitFor("the built-in code plugins", (text) => text.includes("Built-in code plugins") && text.includes(ESSENTIAL_NOTE));
  assert.ok(await page.evaluate(LOCKED) >= 1, "the essential plugin's switch is locked");
  await me.capture("app-plugins-essential");
}));
