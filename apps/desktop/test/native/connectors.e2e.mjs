import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const DAY = 24 * 60 * 60 * 1000;
const AGES = [3, 10, 45];
const SETTINGS = { write: "ask_every_time", wait: "minutes10", native: "off", size_cap: "kib256", keep_log: "days30" };

/** Polls a read until it equals what is wanted, failing with the last value after one step's
 * time. Inputs: what is awaited, the wanted value and the read. Output: none. */
async function until(what, wanted, read, ms = STEP_MS) {
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS)) {
    const seen = await read();
    if (JSON.stringify(seen) === JSON.stringify(wanted)) return;
    assert.ok(Date.now() < deadline, `${what} never came: ${JSON.stringify(seen)}, wanted ${JSON.stringify(wanted)}`);
  }
}

/** Replaces the logged calls with three reads 3, 10 and 45 days old, written straight into
 * the engine database the way the gateway's log writes them. Inputs: the engine root and
 * now. Output: none. */
function seedCalls(runtime, now) {
  const db = new DatabaseSync(join(runtime, "engine.db"));
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    db.exec("DELETE FROM connector_calls");
    const insert = db.prepare(`INSERT INTO connector_calls
      (launch, connector, tool, kind, sent, project, agent, started_ms, ms, bytes_in, bytes_out, outcome)
      VALUES ('e2e-launch', 'board', 'list_issues', 'read', 1, 'e2e-project', 'Agent 1', ?, 340, 120, 2048, 'ok')`);
    for (const days of AGES) insert.run(now - days * DAY);
  } finally {
    db.close();
  }
}

/** How many calls the engine database still holds. Input: the engine root. Output: the count. */
function heldCalls(runtime) {
  const db = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    return db.prepare("SELECT COUNT(*) AS count FROM connector_calls").get().count;
  } finally {
    db.close();
  }
}

/** The keep time and the ages in days of the calls the engine lists, read through the
 * bridge. Inputs: the bridge and now. Output: the keep time and the ages. */
async function listed(bridge, now) {
  const view = await value(bridge, "desktop_connector_activity", { connector: null, project: null });
  return { keep: view.settings.keep_log, ages: view.calls.map((call) => Math.round((now - call.started_ms) / DAY)), version: view.version };
}

/** Starts the real engine and the bridge on a fresh root with an owner profile and projects
 * folder, serves the built app to a browser page, runs the steps and stops everything.
 * Inputs: the test context, the folder prefix and the steps, given the bridge, the owner on
 * the page, a route opener and the engine root. Output: none. */
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
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(runtime, "global.toml"), "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n", { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let engine;
  let bridgeProcess;
  let site;
  try {
    const startedEngine = await launch(engineBinary, ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"], { ...env }, evidence, "engine", /^ready$/m);
    engine = startedEngine.child;
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    site = await serveApp(dist, bridge, [], log);
    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        let visits = 0;
        const open = async (hash, what, check) => {
          assert.deepEqual(await page.evaluate("window.__e2eErrors ?? []"), [], "the page raised no errors before it was left");
          visits += 1;
          await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/?visit=${visits}${hash}` });
          await me.waitFor(what, check);
        };
        await steps({ bridge, me, open, runtime });
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
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

test("Connectors: each Activity log keeps option is saved through the page, read back from the engine and drops older calls", { timeout: 900000 }, (t) => onEngine(t, "ca-connectors-", async ({ bridge, me, open, runtime }) => {
  const now = Date.now();
  const activity = () => open("#/settings/connector-activity", "Connector activity", (text) => text.includes("Connector activity") && text.includes("Activity log keeps"));
  await activity();
  await me.waitFor("the empty log", (text) => text.includes("No connector calls yet."));

  const keep = async (seed, choice, wanted) => {
    const { version } = await listed(bridge, now);
    await value(bridge, "desktop_connector_settings_set", { settings: { ...SETTINGS, keep_log: seed }, version });
    seedCalls(runtime, now);
    await activity();
    await me.waitFor("the logged calls", (text) => text.includes("list_issues") && text.includes("Project: e2e-project"));
    await me.click(choice);
    await me.click("Save changes");
    await me.waitFor("the save", (text) => text.includes("Connector settings saved."));
    await until(`the engine keeps ${choice}`, wanted, async () => {
      const back = await listed(bridge, now);
      return { keep: back.keep, ages: back.ages, held: heldCalls(runtime) };
    });
    await me.capture(`conn-keep-log-${choice.replace(/\s+/gu, "-")}`);
  };

  await t.test("conn keep_log 7 days", () => keep("days30", "7 days", { keep: "days7", ages: [3], held: 1 }));
  await t.test("conn keep_log 30 days", () => keep("days90", "30 days", { keep: "days30", ages: [3, 10], held: 2 }));
  await t.test("conn keep_log 90 days", () => keep("days30", "90 days", { keep: "days90", ages: [3, 10, 45], held: 3 }));
}));
