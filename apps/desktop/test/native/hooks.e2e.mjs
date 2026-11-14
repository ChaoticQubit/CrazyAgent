import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const COMMAND = 'sh "${CLAUDE_PLUGIN_ROOT}/hooks/guard.sh"';

/** Writes a Claude Code plugin folder with command hooks on two events in hooks/hooks.json and, when
 * named, the .claude-plugin/plugin.json a pack plugin ships. Inputs: the folder and the
 * plugin name, or null for a folder without plugin.json. Output: none. */
function plugin(folder, name) {
  mkdirSync(join(folder, "hooks"), { recursive: true, mode: 0o700 });
  if (name !== null) {
    mkdirSync(join(folder, ".claude-plugin"), { recursive: true, mode: 0o700 });
    writeFileSync(join(folder, ".claude-plugin", "plugin.json"), JSON.stringify({ name }));
  }
  writeFileSync(join(folder, "hooks", "guard.sh"), "#!/bin/sh\nexit 0\n", { mode: 0o700 });
  writeFileSync(join(folder, "hooks", "hooks.json"), JSON.stringify({
    hooks: {
      PreToolUse: [{ matcher: "Bash", hooks: [{ type: "command", command: COMMAND }] }],
      SessionStart: [{ hooks: [{ type: "command", command: COMMAND }] }],
    },
  }));
}

test("Settings, Plugins, Hooks lists the engine's hooks, refuses a folder without plugin.json, adds a plugin folder's hooks as one plugin row, turns the plugin off, one event on, then the plugin on", { timeout: 300000 }, async () => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-hk-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const guard = join(root, "plugins", "guard");
  const bare = join(root, "plugins", "bare");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  plugin(guard, "guard");
  plugin(bare, null);
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
      ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings"],
      { ...env }, evidence, "engine", /^ready$/m));
    const started = await launch(bridgeBinary, [join(runtime, "engine.sock")], { ...env }, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the test engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "E2E Owner" }, generation: 1 })).outcome, "applied");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: projects, generation: 2 })).outcome, "applied");
    const hooks = () => value(bridge, "desktop_plugin_hooks", { projectId: null });
    assert.deepEqual(await hooks(), [], "a new engine has no hooks");

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    assert.ok(existsSync(join(dist, "index.html")), "the frontend built");
    site = await serveApp(dist, bridge, [bare, guard], log);

    await withPage(async (page) => {
      const me = owner(page, evidence);
      try {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/settings/plugins` });
        await me.waitFor("the Plugins segments", (text) => text.includes("Skills") && text.includes("MCP servers") && text.includes("Hooks"), STEP_MS);
        await me.click("Hooks", "[role=radio]");
        const empty = await me.waitFor("the Hooks segment settled", (text) => ["No hooks yet", "Couldn't load hooks.", "Hooks isn't ready yet"].some((words) => text.includes(words)), STEP_MS);
        assert.ok(empty.includes("No hooks yet") && !empty.includes("Couldn't load hooks.") && !empty.includes("Hooks isn't ready yet"),
          `the Hooks segment loads its empty list from the engine, not an error or Not ready:\n${empty.slice(0, 2000)}`);
        await me.capture("hooks-empty");

        await me.click("Add hooks from a plugin");
        const refused = await me.waitFor("the refusal of a folder without plugin.json", (text) => text.includes("This folder has hooks but no .claude-plugin/plugin.json naming the plugin.") || text.includes("Added 2 hooks.") || text.includes("Something went wrong"), STEP_MS);
        assert.ok(refused.includes("This folder has hooks but no .claude-plugin/plugin.json naming the plugin.") && !refused.includes("Added 2 hooks."),
          `a folder without plugin.json is refused with its plain reason:\n${refused.slice(0, 2000)}`);
        assert.deepEqual(await hooks(), [], "the refused folder added no hooks");
        await me.capture("hooks-refused");

        await me.click("Add hooks from a plugin");
        const listed = await me.waitFor("the added plugin row in the list", (text) => text.includes("Added 2 hooks.") && text.includes("guard - 2 events"), STEP_MS);
        assert.ok(listed.includes("Show events") && !listed.includes("PreToolUse, matching Bash") && !listed.includes(COMMAND),
          `the plugin shows as one row with its events folded:\n${listed.slice(0, 2000)}`);
        assert.equal(log.picked, 2, "each Add hooks asked the owner for one plugin folder");
        const added = await hooks();
        assert.equal(added.length, 2, JSON.stringify(added));
        assert.deepEqual(added.map((hook) => [hook.plugin, hook.event, hook.command, hook.enabled, hook.scope]).sort((a, b) => a[1].localeCompare(b[1])),
          [["guard", "PreToolUse", COMMAND, true, { kind: "global" }], ["guard", "SessionStart", COMMAND, true, { kind: "global" }]]);
        await me.capture("hooks-added");

        const checked = (selector) => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.getAttribute('aria-checked')`);
        assert.equal(await checked("#hook-on-0"), "true", "the plugin shows On");
        await me.follow("#hook-on-0");
        await me.waitFor("the plugin's hooks turned off", (text) => text.includes("guard hooks turned off."), STEP_MS);
        assert.deepEqual((await hooks()).map((hook) => hook.enabled), [false, false], "the engine holds every hook of the plugin off");
        assert.equal(await checked("#hook-on-0"), "false", "the plugin shows Off");

        await me.click("Show guard events", "button");
        const opened = await me.waitFor("the plugin's events", (text) => text.includes("PreToolUse, matching Bash") && text.includes("SessionStart") && text.includes("Hide events"), STEP_MS);
        assert.ok(opened.includes(COMMAND), `each event shows its command:\n${opened.slice(0, 2000)}`);
        await me.capture("hooks-events");
        const bash = (await hooks()).findIndex((hook) => hook.event === "PreToolUse");
        assert.ok(bash >= 0);
        await me.follow(`#hook-on-0-${bash}`);
        await me.waitFor("the one hook turned on", (text) => text.includes("Hook turned on.") && text.includes("Partly on"), STEP_MS);
        assert.deepEqual((await hooks()).map((hook) => [hook.event, hook.enabled]).sort((a, b) => a[0].localeCompare(b[0])), [["PreToolUse", true], ["SessionStart", false]],
          "the engine holds only that event's hook on");
        assert.equal(await checked(`#hook-on-0-${bash}`), "true", "the event shows On");
        assert.equal(await checked("#hook-on-0"), "false", "the plugin row shows Partly on, not On");
        await me.capture("hooks-partly-on");

        await me.follow("#hook-on-0");
        await me.waitFor("the plugin's hooks turned on", (text) => text.includes("guard hooks turned on."), STEP_MS);
        assert.deepEqual((await hooks()).map((hook) => hook.enabled), [true, true], "the engine holds every hook of the plugin on again");
        assert.equal(await checked("#hook-on-0"), "true", "the plugin shows On again");
        await me.capture("hooks-on-again");
        assert.deepEqual(await page.evaluate("window.__e2eErrors"), [], "the page raised no errors");
      } catch (failure) {
        await me.capture("failure").catch(() => {});
        throw failure;
      }
    });
    for (const command of ["desktop_plugin_hooks", "desktop_add_hooks", "desktop_set_hook_enabled"]) {
      assert.ok(log.calls.includes(command), `the page called ${command}: ${JSON.stringify(log.calls)}`);
    }
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
});
