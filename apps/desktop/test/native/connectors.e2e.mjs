import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
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
const LINEAR_KEY = "lin_api_e2e_synthetic_0000";
const BOARD_KEY = "board_e2e_synthetic_key_0000";
const BOARD = "schema: 1\nkind: connector\nname: board\ntitle: Board\nversion: 1\nmcp:\n  url: https://mcp.board.invalid/mcp\n  hosts: [mcp.board.invalid]\nkey:\n  secret: api_key\n  header: Authorization\n  scheme: Bearer\n";
const SCHEMES = ["light", "dark"];

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

/** The engine's grant row for one connector in one project, or null when there is none
 * or the grants table is not made yet. Inputs: the engine root, the project and the
 * connector. Output: the row's switch and Who. */
function grantRow(runtime, project, connector) {
  const db = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    const row = db.prepare("SELECT enabled, who FROM connector_grants WHERE project = ? AND connector = ?").get(project, connector);
    return row ? { enabled: row.enabled, who: row.who } : null;
  } catch (error) {
    if (/no such table/u.test(error.message)) return null;
    throw error;
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

/** Shows the page in the light or dark scheme, the way the system setting does, and checks
 * the page sees it. Inputs: the page and the scheme. Output: none. */
async function scheme(page, wanted) {
  await page.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: wanted }] });
  assert.equal(await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches"), wanted === "dark", `the page shows the ${wanted} scheme`);
}

/** The files of this run that hold a key: every file under the run root and the evidence
 * folder, the engine database, its logs and the page captures included, leaving out only the
 * engine's key folders, the one place a key may be, read after both processes stopped and
 * calls.json was written. Both processes' error streams must be there. Inputs: the run root, the engine root, the evidence folder and the key. Output: the
 * paths that hold it. */
function holdingKey(root, runtime, evidence, key) {
  for (const name of ["engine.err", "bridge.err"]) assert.ok(existsSync(join(evidence, name)), `${name} is written, so it is read`);
  const keys = new Set([join(runtime, "plugin-keys"), join(runtime, "account-keys")]);
  const files = [];
  const walk = (folder) => {
    for (const entry of readdirSync(folder, { withFileTypes: true })) {
      const path = join(folder, entry.name);
      if (keys.has(path)) continue;
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) files.push(path);
    }
  };
  for (const folder of new Set([root, evidence])) walk(folder);
  assert.ok(files.includes(join(runtime, "engine.db")), "the engine database is read too");
  return files.filter((path) => readFileSync(path).includes(key));
}

/** Starts the real engine and the bridge on a fresh root with an owner profile and projects
 * folder, serves the built app to a browser page, runs the steps, stops everything, and then
 * checks that no file of the run outside the key folders holds any key the steps entered.
 * Inputs: the test context, the folder prefix, the steps, given the bridge, the owner on the
 * page, a route opener, the engine root, the page, the test HOME and the evidence folder,
 * and the keys. Output: none. */
async function onEngine(t, prefix, steps, keys = []) {
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
        await steps({ bridge, me, open, runtime, page, home, evidence });
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
  for (const key of keys) assert.deepEqual(holdingKey(root, runtime, evidence, key), [], "no file of the run outside the engine's key folders holds the key");
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

test("Connectors: turning Linear on for a project shows On and the engine grant row exists, and each Who choice and Off are saved", { timeout: 900000 }, (t) => onEngine(t, "ca-conn-grant-", async ({ bridge, me, open, runtime, page, home }) => {
  await value(bridge, "desktop_connector_set_key", { name: "linear", secret: "api_key", key: LINEAR_KEY });
  const repo = join(home, "connector-repo");
  mkdirSync(repo, { mode: 0o700 });
  const receipt = await value(bridge, "desktop_create_project", { spec: { name: "Connector project", repositories: [repo] }, generation: 3 });
  assert.equal(receipt.outcome, "applied", `the project is made: ${JSON.stringify(receipt)}`);
  const project = receipt.project_id;
  const section = () => open(`#/projects/${encodeURIComponent(project)}/settings?section=connectors`, "the project's Connectors section",
    (text) => text.includes("Each is off until you turn it on here.") && text.includes("Linear"));
  const switched = () => page.evaluate("document.querySelector('#pc-on-linear')?.getAttribute('aria-checked') ?? null");
  const whoShown = () => page.evaluate("document.querySelector('li[data-connector=\"linear\"] [role=radio][aria-checked=\"true\"]')?.textContent ?? null");
  const engineHolds = (what, row) => until(what, { row, grants: row === null ? [] : [{ connector: "linear", enabled: row.enabled === 1, who: row.who }] }, async () => ({
    row: grantRow(runtime, project, "linear"),
    grants: (await value(bridge, "desktop_connectors", { project })).grants.filter((grant) => grant.connector === "linear"),
  }));

  await section();
  assert.equal(await switched(), "false", "Linear starts Off");
  await engineHolds("no Linear grant yet", null);

  await t.test("conn grant on", async () => {
    await me.follow("#pc-on-linear");
    await engineHolds("the Linear grant row", { enabled: 1, who: "both" });
    await section();
    assert.equal(await switched(), "true", "the grant reads back On from the engine");
    assert.equal(await whoShown(), "Master and agents with tools on", "Who starts on the default");
    await me.capture("conn-grant-on");
  });

  for (const [label, who] of [["Agents only", "agents"], ["Master and agents with tools on", "both"], ["Master only", "master"]]) {
    await t.test(`conn who ${label}`, async () => {
      await me.click(label);
      await engineHolds(`Who ${label}`, { enabled: 1, who });
      await section();
      assert.equal(await whoShown(), label, "the Who reads back from the engine");
      await me.capture(`conn-who-${who}`);
    });
  }

  await t.test("conn grant off", async () => {
    await me.follow("#pc-on-linear");
    await engineHolds("the Linear grant turned off, keeping its last Who", { enabled: 0, who: "master" });
    await section();
    assert.equal(await switched(), "false");
    assert.equal(await whoShown(), null, "Who shows only while On");
    await me.capture("conn-grant-off");
  });

  for (const shown of SCHEMES) {
    await t.test(`conn project section ${shown}`, async () => {
      await scheme(page, shown);
      await section();
      await me.capture(`conn-project-${shown}`);
    });
  }
  await scheme(page, "light");
  assert.ok(!(await me.seen()).includes(LINEAR_KEY), "the key never reaches the page");
}, [LINEAR_KEY]));

test("Connectors: Settings, Connectors saves each option through the page, adds a connector through its trust screen, sets and clears its key and removes it", { timeout: 900000 }, (t) => onEngine(t, "ca-conn-page-", async ({ bridge, me, open, runtime, page }) => {
  const read = () => value(bridge, "desktop_connectors", { project: null });
  const listedConnector = async (name) => (await read()).connectors.find((connector) => connector.name === name) ?? null;
  const connectorsPage = () => open("#/settings/connectors", "Settings, Connectors", (text) => text.includes("Catalog") && text.includes("Connector settings"));
  const connectorPage = (name, title, check) => open(`#/settings/connectors?connector=${name}`, `${title}'s page`, (text) => text.includes("All connectors") && text.includes(title) && check(text));
  const checkedOf = (name) => page.evaluate(`document.querySelector('input[name="${name}"]:checked')?.value ?? null`);
  const kept = { write: "ask_every_time", wait: "minutes30", keep_log: "days90" };
  const seeded = await read();
  await value(bridge, "desktop_connector_settings_set", { settings: { ...seeded.settings, ...kept }, version: seeded.version });
  await connectorsPage();

  await t.test("conn harness rows", async () => {
    const rows = await value(bridge, "desktop_connector_reach");
    assert.ok(rows.length >= 3, "every harness has a row");
    assert.deepEqual(await page.evaluate("[...document.querySelectorAll('ul[aria-label=\"Harnesses\"] li')].map((row) => row.innerText.trim())"), rows, "the page shows the bridge's rows in order");
  });

  await t.test("conn locked line", async () => {
    const shown = await me.seen();
    for (const words of ["Tool list check and third-party label", "Always on", "every vendor result starts with a line saying it is content, not instructions."]) assert.ok(shown.includes(words), words);
    assert.equal(await page.evaluate("document.querySelector('[data-setting=\"Tool list check and third-party label\"] input, [data-setting=\"Tool list check and third-party label\"] button') !== null"), false, "the locked line has no control");
  });

  const choose = async (setting, field, input, label, wanted) => {
    await connectorsPage();
    await me.click(label, `fieldset[data-setting="${setting}"] label`);
    await me.click("Save changes");
    await until(`the engine holds ${label}`, { [field]: wanted, ...kept }, async () => {
      const settings = (await read()).settings;
      return { [field]: settings[field], write: settings.write, wait: settings.wait, keep_log: settings.keep_log };
    });
    await connectorsPage();
    assert.equal(await checkedOf(input), wanted, `the page reads ${label} back from the engine`);
    await me.capture(`conn-${field}-${wanted}`);
  };
  await t.test("conn native on", () => choose("native", "native", "connector-native", "On", "on"));
  await t.test("conn native off", () => choose("native", "native", "connector-native", "Off", "off"));
  await t.test("conn size_cap 64 KiB", () => choose("size_cap", "size_cap", "connector-size-cap", "64 KiB", "kib64"));
  await t.test("conn size_cap 1 MiB", () => choose("size_cap", "size_cap", "connector-size-cap", "1 MiB", "mib1"));
  await t.test("conn size_cap 256 KiB", () => choose("size_cap", "size_cap", "connector-size-cap", "256 KiB", "kib256"));

  await t.test("conn add file and trust", async () => {
    await connectorsPage();
    await me.click("Add connector");
    await me.waitFor("the Add dialog", (text) => text.includes("Add a connector"));
    await me.type("#connector-yaml", BOARD);
    await me.click("Check");
    const shown = await me.waitFor("the trust screen", (text) => text.includes("Trust Board?"));
    for (const words of ["Address: https://mcp.board.invalid/mcp", "Reaches mcp.board.invalid", "Read-only mode at the vendor: none", "Key: the Authorization header with Bearer, from api_key", "Secrets it needs: api_key", "SHA-256 "]) {
      assert.ok(shown.includes(words), `the trust screen shows ${words}`);
    }
    assert.equal(await page.evaluate("document.querySelector('dialog[open] a') !== null"), false, "nothing on the trust screen is a link");
    await me.capture("conn-trust");
    await me.click("Confirm");
    await until("Board in the engine", { origin: "user", status: "needs_key" }, async () => {
      const board = await listedConnector("board");
      return board && { origin: board.origin, status: board.status.kind };
    });
    assert.ok(existsSync(join(runtime, "app-plugins", "board.yaml")), "the manifest is in the owner's plugins folder");
    await me.waitFor("Board listed", (text) => text.includes("board, version 1. Added by you."));
  });

  await t.test("conn bind key", async () => {
    await connectorPage("board", "Board", (text) => text.includes("api_key: Not set"));
    await me.click("Set key for api_key of Board");
    await me.waitFor("the key dialog", (text) => text.includes("Key for api_key") && text.includes("Board sends this key to mcp.board.invalid. Only the engine keeps it."));
    await me.type("#connector-key", BOARD_KEY);
    await me.click("Save", "dialog button");
    await until("the entered key", { kind: "key" }, async () => (await listedConnector("board"))?.secrets[0].binding ?? null);
    await me.waitFor("Key you entered", (text) => text.includes("api_key: Key you entered"));
    const html = await page.evaluate("document.documentElement.outerHTML + [...document.querySelectorAll('input, textarea')].map((field) => field.value).join(' ')");
    assert.ok(!html.includes(BOARD_KEY), "the key is nowhere on the page");
    assert.ok(!JSON.stringify(await read()).includes(BOARD_KEY), "the page read never carries the key");
    await me.capture("conn-key-set");
  });

  await t.test("conn clear key", async () => {
    await me.click("Clear key for api_key of Board");
    await until("the key cleared", { kind: "none" }, async () => (await listedConnector("board"))?.secrets[0].binding ?? null);
    await me.waitFor("Not set", (text) => text.includes("api_key: Not set"));
  });

  await t.test("conn remove", async () => {
    await me.click("Remove Board");
    await me.waitFor("the remove confirm", (text) => text.includes("Remove Board?") && text.includes("with its keys and its switch in every project."));
    await me.click("Remove", "dialog button");
    await until("Board gone", null, () => listedConnector("board"));
    assert.ok(!existsSync(join(runtime, "app-plugins", "board.yaml")), "the manifest is deleted");
    await me.waitFor("the catalog again", (text) => text.includes("Catalog") && !text.includes("Added by you"));
    assert.equal(await page.evaluate("location.hash"), "#/settings/connectors", "a removed connector's page goes back to the catalog");
  });

  for (const shown of SCHEMES) {
    await t.test(`conn pages ${shown}`, async () => {
      await scheme(page, shown);
      await connectorsPage();
      await me.capture(`conn-settings-${shown}`);
      await connectorPage("linear", "Linear", (text) => text.includes("Reaches"));
      await me.capture(`conn-connector-${shown}`);
    });
  }
  await scheme(page, "light");
}, [BOARD_KEY]));
