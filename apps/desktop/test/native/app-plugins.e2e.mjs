import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SYNTHETIC_KEY = "sk-e2e-synthetic-plugin-key-0001";
const SEARCH_SENDS = ["Session text", "Tool inputs and outputs", "Memories", "Search queries"];

/** A plugin manifest that embeds through the test's own listener on this computer, plain
 * http with no secret. Inputs: name, title, port and version. Output: the YAML text. */
function loopback(name, title, port, version = "1.0.0") {
  return `schema: 1
kind: embedding
name: ${name}
title: ${title}
version: ${version}
description: Embeds through the test's own listener on this computer.
http:
  url: http://127.0.0.1:${port}/embed
  hosts: [127.0.0.1]
  body:
    input: "{{inputs}}"
  vectors: embeddings.*
  dimensions: 128
`;
}

/** A plugin manifest with one secret whose https host this test never reaches. Inputs: the
 * version. Output: the YAML text. */
function keyed(version = "1.0.0") {
  return `schema: 1
kind: embedding
name: harbor
title: Harbor embeddings
version: ${version}
description: Embeds through a host this test never reaches.
http:
  url: https://api.harbor.invalid/v1/embed
  hosts: [api.harbor.invalid]
  secrets: [api_key]
  headers:
    Authorization: "Bearer {{secret.api_key}}"
  body:
    input: "{{inputs}}"
  vectors: data.*.embedding
  dimensions: 256
`;
}

const REFUSED = `schema: 1
kind: embedding
name: plain
title: Plain http elsewhere
version: 1.0.0
http:
  url: http://api.harbor.invalid/v1/embed
  hosts: [api.harbor.invalid]
  body:
    input: "{{inputs}}"
  vectors: data.*.embedding
  dimensions: 256
`;

/** Writes one manifest into the engine's plugins folder the way an owner drops a file in:
 * the folder private to the owner, the file 0600. Inputs: the engine root, the file name and
 * the text. Output: the file's path. */
function dropIn(runtime, file, text) {
  const folder = join(runtime, "app-plugins");
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  const path = join(folder, file);
  writeFileSync(path, text, { mode: 0o600 });
  return path;
}

/** Starts a loopback listener that answers every embedding request with one fixed vector and
 * counts the requests. Output: the server, its port and the count reader. */
async function embedListener() {
  let requests = 0;
  const server = createServer((request, reply) => {
    requests += 1;
    request.resume();
    request.on("end", () => {
      reply.writeHead(200, { "content-type": "application/json" });
      reply.end(JSON.stringify({ embeddings: [Array.from({ length: 128 }, (_, index) => (index === 0 ? 1 : 0))] }));
    });
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return { server, port: server.address().port, requests: () => requests };
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

/** The model-source seam the engine reads in tests so a download row never reaches the
 * internet. Output: the seam's environment, or null while the engine has none (DEC-156
 * addendum 4: the download rows refuse to click Download without it). */
function modelSeam() {
  return null;
}

/** Starts the real engine and the bridge on a fresh root with an owner profile and projects
 * folder, serves the built app to a browser page, runs the steps and stops everything.
 * Inputs: the test context, the folder prefix, files to drop in before the engine starts and
 * the steps, given the bridge, the owner on the page, the page, a route opener, a page
 * reload, the engine root, the listener and the call log. Output: none. */
async function onEngine(t, prefix, drops, steps) {
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
  const listener = await embedListener();
  for (const [file, text] of drops(listener.port)) dropIn(runtime, file, text);
  writeFileSync(join(runtime, "global.toml"), "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n", { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8", ...(modelSeam() ?? {}) };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  const picks = [];
  let engine;
  let bridgeProcess;
  let site;
  try {
    ({ child: engine } = await launch(engineBinary, ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"], { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    site = await serveApp(dist, bridge, [], log, {
      desktop_pick_app_plugin_file: () => {
        assert.ok(picks.length > 0, "the page opened the manifest chooser more often than the owner picks");
        return { value: picks.shift() };
      },
    });
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
        await steps({ bridge, me, page, open, runtime, listener, picks, log });
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    const native = new Set(commands(readFileSync(LIB, "utf8")).filter((command) => plan(command).skip).map((command) => command.name));
    assert.deepEqual([...log.unrouted].filter((command) => !native.has(command)), [], "every plain command the page sent reached the bridge");
    assert.deepEqual(log.invalid, [], "the bridge accepted every argument the page sent");
    t.diagnostic(`model download rows: ${modelSeam() ? "seam present" : "no model-source seam, download rows skipped"}`);
  } finally {
    log.closing = true;
    writeFileSync(join(evidence, "calls.json"), JSON.stringify(log.calls, null, 1));
    site?.server.close();
    listener.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** The engine's own view of one plugin, read through the bridge. Inputs: the bridge and the
 * name. Output: the plugin, or null when it is not listed. */
async function stored(bridge, name) {
  const listing = await value(bridge, "desktop_app_plugins");
  return listing.plugins.find((plugin) => plugin.name === name) ?? null;
}

/** Pastes a manifest into the Add dialog as the owner's paste does and presses Check.
 * Inputs: the owner and the text. Output: none. */
async function pasteAndCheck(me, text) {
  await me.click("Add plugin");
  await me.waitFor("the Add dialog", (shown) => shown.includes("Add an app plugin"));
  await me.type("#app-plugin-yaml", text);
  await me.click("Check");
}

test("App plugins: every option is saved in the engine and the page shows it after a reload", { timeout: 900000 }, (t) => onEngine(t, "ca-app-plugins-", (port) => [
  ["dock.yaml", loopback("dock", "Dock embeddings", port)],
  ["plain.yaml", REFUSED],
], async ({ bridge, me, page, open, runtime, listener, picks, log }) => {
  const page_ = () => open("#/settings/app-plugins", "App plugins", (text) => text.includes("App plugins") && text.includes("Add plugin"));
  await page_();

  await t.test("A1 the list shows each status label", async () => {
    await me.waitFor("every status", (text) => ["On this Mac", "Enabled", "Dock embeddings", "Needs review", "Refused", "Off"].every((words) => text.includes(words)));
    assert.equal((await stored(bridge, "dock")).status.kind, "needs_review");
    assert.equal((await stored(bridge, "local")).enabled, true);
    assert.match(await me.seen(), /app_plugin_[a-z_]+, line \d+/u, "the refused file names its code and line");
    await me.capture("app-plugins-list");
  });

  await t.test("A3 Add then Cancel saves nothing", async () => {
    await pasteAndCheck(me, keyed());
    await me.waitFor("the trust screen", (text) => text.includes("Trust Harbor embeddings?"));
    await me.click("Cancel");
    assert.equal(await stored(bridge, "harbor"), null, "a cancelled add leaves no plugin");
    assert.ok(!existsSync(join(runtime, "app-plugins", "harbor.yaml")), "a cancelled add writes no file");
  });

  await t.test("A2 Add by paste shows the trust screen and Confirm adds it", async () => {
    await pasteAndCheck(me, keyed());
    const shown = await me.waitFor("the trust screen", (text) => text.includes("Trust Harbor embeddings?"));
    for (const words of ["Kind: Embedding", "Reaches api.harbor.invalid", "Secrets it needs: api_key", "SHA-256 "]) assert.ok(shown.includes(words), `the trust screen shows ${words}`);
    assert.ok(!shown.includes("local, not encrypted"));
    await me.capture("app-plugins-trust");
    await me.click("Confirm");
    await until("harbor in the engine", "user", async () => (await stored(bridge, "harbor"))?.origin ?? null);
    await me.waitFor("harbor listed", (text) => text.includes("Harbor embeddings") && text.includes("api_key: Not set"));
  });

  await t.test("A14 a manifest naming the owner's plugin replaces it", async () => {
    await pasteAndCheck(me, keyed("1.1.0"));
    await me.waitFor("the replace note", (text) => text.includes("Replaces your plugin named harbor."));
    await me.click("Confirm");
    await until("the new version in the engine", "1.1.0", async () => (await stored(bridge, "harbor"))?.version ?? null);
  });

  await t.test("A5 a refused manifest shows its code and line and offers no Confirm", async () => {
    await pasteAndCheck(me, REFUSED);
    const shown = await me.waitFor("the refusal", (text) => text.includes("This manifest can't be added"));
    assert.match(shown, /\(app_plugin_[a-z_]+, line \d+/u);
    assert.equal(await page.evaluate(`[...document.querySelectorAll("dialog button")].some((button) => button.innerText.trim() === "Confirm")`), false);
    await me.click("Edit manifest");
    await me.waitFor("the paste box again", (text) => text.includes("Add an app plugin"));
    assert.equal(await page.evaluate(`document.getElementById("app-plugin-yaml").value`), REFUSED, "Edit keeps the text");
    await me.click("Cancel");
    assert.deepEqual(readdirSync(join(runtime, "app-plugins")).sort(), ["dock.yaml", "harbor.yaml", "plain.yaml"], "a refused manifest writes no file");
  });

  await t.test("A4 and A13 Add by picking a .yaml file; plain http to this computer says local, not encrypted", async () => {
    picks.push(loopback("pier", "Pier embeddings", listener.port));
    await me.click("Add plugin");
    await me.click("Pick a .yaml file");
    const shown = await me.waitFor("the picked file's trust screen", (text) => text.includes("Trust Pier embeddings?"));
    assert.ok(shown.includes("Reaches 127.0.0.1 (local, not encrypted)"));
    await me.capture("app-plugins-loopback-trust");
    await me.click("Confirm");
    await until("pier in the engine", "user", async () => (await stored(bridge, "pier"))?.origin ?? null);
    await me.waitFor("pier listed unencrypted", (text) => text.includes("Pier embeddings") && text.includes("Reaches 127.0.0.1 (local, not encrypted)"));
    assert.equal(log.calls.filter((call) => call === "desktop_pick_app_plugin_file").length, 1);
  });

  await t.test("A9 Review then Confirm trusts a dropped-in file", async () => {
    await me.click("Review Dock embeddings");
    await me.waitFor("the review screen", (text) => text.includes("Review Dock embeddings"));
    await me.click("Confirm");
    await until("dock trusted", "ready", async () => (await stored(bridge, "dock"))?.status.kind ?? null);
  });

  await t.test("A7 and A6 each switch turns a plugin off and on in the engine", async () => {
    const on = await stored(bridge, "pier");
    await me.follow("#app-plugin-pier");
    await until("pier switched", !on.enabled, async () => (await stored(bridge, "pier"))?.enabled ?? null);
    await me.follow("#app-plugin-pier");
    await until("pier switched back", on.enabled, async () => (await stored(bridge, "pier"))?.enabled ?? null);
    await page_();
    await until("the switch after a reload", String(on.enabled), () => page.evaluate(`document.getElementById("app-plugin-pier")?.getAttribute("aria-checked") ?? null`));
  });

  await t.test("A10 a secret binds to an API-key account's key", async () => {
    const account = await value(bridge, "desktop_add_account", { draft: { provider: "openai_api", method: "api_key", name: "Work OpenAI", model: "text-embedding-3-small" }, key: SYNTHETIC_KEY });
    await page_();
    await me.click("Set key for api_key of Harbor embeddings");
    await me.waitFor("the key dialog", (text) => text.includes("Key for api_key") && text.includes("Key from Work OpenAI"));
    await me.follow(`label.radio-card:has(input[value="account:${account.id}"])`);
    await me.click("Save");
    await until("the account binding", { kind: "account", account: account.id, name: "Work OpenAI" }, async () => (await stored(bridge, "harbor"))?.secrets[0].binding ?? null);
    await me.waitFor("the binding on the page", (text) => text.includes("api_key: Key from Work OpenAI"));
  });

  await t.test("A12 Unbind clears the binding", async () => {
    await me.click("Unbind api_key of Harbor embeddings");
    await until("no binding", { kind: "none" }, async () => (await stored(bridge, "harbor"))?.secrets[0].binding ?? null);
    await me.waitFor("Not set again", (text) => text.includes("api_key: Not set"));
  });

  await t.test("A11 a key entered for the plugin is saved and never shown again", async () => {
    await me.click("Set key for api_key of Harbor embeddings");
    await me.follow(`label.radio-card:has(input[value="key"])`);
    await me.type("#app-plugin-key", SYNTHETIC_KEY);
    await me.click("Save");
    await until("the entered key binding", { kind: "key" }, async () => (await stored(bridge, "harbor"))?.secrets[0].binding ?? null);
    await me.waitFor("Key you entered", (text) => text.includes("api_key: Key you entered"));
    const html = await page.evaluate("document.documentElement.outerHTML + [...document.querySelectorAll('input, textarea')].map((field) => field.value).join(' ')");
    assert.ok(!html.includes(SYNTHETIC_KEY), "the key is nowhere on the page");
    assert.ok(!JSON.stringify(await value(bridge, "desktop_app_plugins")).includes(SYNTHETIC_KEY), "the listing never carries the key");
  });

  await t.test("A8 Remove deletes the owner's plugin and its file", async () => {
    await me.click("Remove Pier embeddings");
    await me.waitFor("the remove confirm", (text) => text.includes("Remove Pier embeddings?"));
    await me.click("Remove", "dialog button");
    await until("pier gone", null, () => stored(bridge, "pier"));
    assert.ok(!existsSync(join(runtime, "app-plugins", "pier.yaml")));
    assert.equal(await page.evaluate(`document.querySelector('[aria-label="Remove On this Mac"]')`), null, "a built-in plugin has no Remove");
  });

  await t.test("A15 a folder over its cap lists the first plugins and says so", async () => {
    for (let index = 0; index < 64; index += 1) dropIn(runtime, `more-${String(index).padStart(2, "0")}.yaml`, loopback(`more-${String(index).padStart(2, "0")}`, `More ${index}`, listener.port));
    assert.equal((await value(bridge, "desktop_app_plugins")).more, true, "the engine says there are more");
    await page_();
    await me.waitFor("the more note", (text) => /Showing the first \d+ plugins\./u.test(text));
  });
}));

test("Search: every option is saved in the engine, an HTTP plugin only after its confirm, and the model only after its own", { timeout: 900000 }, (t) => onEngine(t, "ca-search-", (port) => [
  ["pier.yaml", loopback("pier", "Pier embeddings", port)],
  ["harbor.yaml", keyed()],
], async ({ bridge, me, page, open }) => {
  for (const name of ["pier", "harbor"]) {
    const preview = await value(bridge, "desktop_app_plugin_review", { name });
    assert.equal(preview.kind, "ready", `${name} reads ready: ${JSON.stringify(preview)}`);
    await value(bridge, "desktop_app_plugin_trust", { name, sha256: preview.trust.sha256 });
    await value(bridge, "desktop_app_plugin_set_enabled", { name, enabled: true });
  }
  const choice = async () => (await value(bridge, "desktop_search_settings")).choice;
  const checked = () => page.evaluate(`document.querySelector('input[name="search-choice"]:checked')?.value ?? null`);
  const searchPage = () => open("#/settings/search", "Search", (text) => text.includes("Rank search with") && text.includes("Built-in model"));
  const pick = (value_) => me.follow(`label.radio-card:has(input[value="${value_}"])`);
  await searchPage();

  await t.test("S2 Local is the default", async () => {
    assert.deepEqual(await choice(), { kind: "plugin", name: "local" });
    await until("Local checked", "plugin:local", checked);
  });

  await t.test("S1 Full text only is saved and stays after a reload", async () => {
    await pick("full_text");
    await until("full text in the engine", { kind: "full_text" }, choice);
    await searchPage();
    await until("full text checked after a reload", "full_text", checked);
  });

  await t.test("S2 choosing Local again is saved", async () => {
    await pick("plugin:local");
    await until("local in the engine", { kind: "plugin", name: "local" }, choice);
  });

  await t.test("S4 an HTTP plugin then Cancel changes nothing", async () => {
    await pick("plugin:pier");
    await me.waitFor("the HTTP confirm", (text) => text.includes("Send search text to 127.0.0.1?"));
    await me.click("Cancel");
    assert.deepEqual(await choice(), { kind: "plugin", name: "local" });
  });

  await t.test("S3 an HTTP plugin is saved after a confirm naming its host and what leaves the computer", async () => {
    await pick("plugin:pier");
    const shown = await me.waitFor("the HTTP confirm", (text) => text.includes("Send search text to 127.0.0.1?"));
    for (const line of SEARCH_SENDS) assert.ok(shown.includes(line), `the confirm lists ${line}`);
    await me.capture("search-http-confirm");
    await me.click("Send to 127.0.0.1");
    await until("pier in the engine", { kind: "plugin", name: "pier" }, choice);
    await me.waitFor("the progress line", (text) => /Embedded \d+ of \d+ sessions and memories\./u.test(text));
  });

  await t.test("S10 a plugin that cannot be used is shown off with its reason", async () => {
    const harbor = (await value(bridge, "desktop_search_settings")).options.find((option) => option.name === "harbor");
    assert.ok(harbor && !harbor.usable && harbor.reason, `harbor is listed unusable with a reason: ${JSON.stringify(harbor)}`);
    assert.equal(await page.evaluate(`document.querySelector('input[value="plugin:harbor"]')?.disabled ?? null`), true);
    await me.waitFor("the reason", (text) => text.includes(harbor.reason));
  });

  await t.test("S11 a save another window beat reloads and says so", async () => {
    const page_version = (await value(bridge, "desktop_search_settings")).version;
    const saved = await value(bridge, "desktop_set_search", { choice: { kind: "full_text" }, confirmHost: null, version: page_version });
    assert.equal(saved.kind, "saved");
    await pick("plugin:local");
    await me.waitFor("the stale notice", (text) => text.includes("Search settings changed in another window."));
    assert.deepEqual(await choice(), { kind: "full_text" });
    await until("the saved choice on the page", "full_text", checked);
  });

  await t.test("S6 Download then Cancel at the confirm downloads nothing", async () => {
    await searchPage();
    assert.equal((await value(bridge, "desktop_search_settings")).model.state, "not_downloaded");
    await me.click("Download");
    await me.waitFor("the download confirm", (text) => text.includes("Download the built-in model?") && text.includes("126 MB"));
    await me.click("Cancel");
    assert.equal((await value(bridge, "desktop_search_settings")).model.state, "not_downloaded");
  });

  for (const row of ["S5 Download with the confirm runs to ready", "S7 Cancel download stops a running download", "S8 a failed download shows its problem and Download again", "S9 Delete model deletes the downloaded model after a confirm"]) {
    await t.test(row, { skip: modelSeam() === null && "no model-source seam in the engine yet (asked w-v3-plugins 14:22 and 14:29)" }, async () => {
      assert.fail("written once the engine names its model-source seam");
    });
  }
}));
