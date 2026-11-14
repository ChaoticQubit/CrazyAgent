import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
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
const TMUX_FOLDERS = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const SHOWN = `(() => {
  const quit = document.querySelector('input[type=radio][name="on-quit"]:checked');
  const left = document.querySelector('input[type=radio][name="left-running"]:checked');
  const hours = document.getElementById("left-running-hours");
  return [quit?.value ?? null, left?.value ?? null, hours?.value ?? null, hours?.disabled ?? null];
})()`;
const SAVE = 'form[aria-label="Quitting"] button[type=submit]';
const KEEP_CARD = "Keep team agents running They go on with their work, and CrazyAgent takes them over when it opens again. An agent under network None is frozen instead, until CrazyAgent opens again.";
const STOP_CARD = "Stop team agents Each agent stops as Stop would, and its task goes back to the queue.";
const HOUR_S = 60 * 60;
const TEN_YEARS_S = 10 * 365 * 24 * HOUR_S;
const SLACK_S = 5;

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

/** Leaves a tmux server at the engine's terminal socket in this test's own root, as an engine
 * that quit with team agents running leaves its server, so the next engine finds it there.
 * Input: the stopped engine's root. Output: none. */
function leftServer(runtime) {
  const folder = join(runtime, "terminal");
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  const tmux = TMUX_FOLDERS.map((place) => join(place, "tmux")).find((program) => existsSync(program));
  assert.ok(tmux, "tmux is installed");
  const started = spawnSync(tmux, ["-S", "tmux.sock", "new-session", "-d", "-s", "left-running", "sleep", "600"], { cwd: folder, env: { PATH: "/usr/bin:/bin" }, encoding: "utf8" });
  assert.equal(started.status, 0, `the left server starts: ${started.stderr}`);
  assert.ok(statSync(join(folder, "tmux.sock")).isSocket(), "the left server listens at the engine's socket");
}

/** The time the server at the engine's terminal socket ends at, as the engine set it: the
 * socket's modification time. Input: the engine root. Output: whole seconds since the epoch. */
function deadline(runtime) {
  return Math.floor(statSync(join(runtime, "terminal", "tmux.sock")).mtimeMs / 1000);
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * serves the built app to a browser page, runs the steps and stops everything, ending any
 * terminal server left in this test's root. Inputs: the test context, the temporary folder
 * prefix and the steps, given the bridge, the owner on the page, a route opener, a page
 * reload, the engine root and an engine restart that runs `between` while no engine runs.
 * Output: none. */
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
    const bridge = { port: Number(started.match[1]), token: started.match[2] };
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
    const restart = async (between = () => {}) => {
      starts += 1;
      await stop(bridgeProcess);
      await stop(engine);
      await between();
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
        await steps({ bridge, me, page, open, reload, runtime, restart });
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

/** Opens Settings, Quitting and waits for both choices. Inputs: the owner and the route
 * opener. Output: none. */
async function quitting(me, open) {
  await open("#/settings/quit", "the Quitting setting", (text) => text.includes("What happens to team agents when CrazyAgent quits."));
  await me.waitFor("both choices", (text) => ["Keep team agents running", "Stop team agents", "after CrazyAgent quits", "No limit, until CrazyAgent opens again"].every((words) => text.includes(words)));
}

/** Types the hours agents left running wait into the hours field, as the owner's typing does.
 * Inputs: the page and the text. Output: none. */
async function hours(page, text) {
  const typed = await page.evaluate(`(() => {
    const field = document.getElementById("left-running-hours");
    if (!field || field.disabled) return false;
    field.focus();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(field, ${JSON.stringify(text)});
    field.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  })()`);
  assert.ok(typed, "the hours field takes typing");
}

/** The stored setting as the engine reads it. Input: the bridge. Output: the quit choice, the
 * time limit and the hours. */
async function stored(bridge) {
  const settings = await value(bridge, "desktop_quit_settings");
  return [settings.on_quit, settings.left_running, settings.hours];
}

test("Quitting: Keep team agents running is the default, and picking it again after Stop is saved and stays after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-quit-keep-", async ({ bridge, me, page, open, reload, restart }) => {
  await quitting(me, open);
  await until("the default in the engine", ["keep_running", "after_hours", 24], () => stored(bridge));
  await until("the default on the page", ["keep_running", "after_hours", "24", false], () => page.evaluate(SHOWN));
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null`), true, "Save waits for a change");
  await me.click(STOP_CARD, "label.radio-card");
  await me.click("Save changes", SAVE);
  await until("Stop team agents in the engine", ["stop", "after_hours", 24], () => stored(bridge));
  await me.click(KEEP_CARD, "label.radio-card");
  await me.click("Save changes", SAVE);
  await until("Keep team agents running in the engine", ["keep_running", "after_hours", 24], () => stored(bridge));
  await reload("What happens to team agents when CrazyAgent quits.");
  await until("Keep team agents running after a reload", ["keep_running", "after_hours", "24", false], () => page.evaluate(SHOWN));
  await restart();
  await until("Keep team agents running after an engine restart", ["keep_running", "after_hours", 24], () => stored(bridge));
  await me.capture("quitting-keep-running");
}));

test("Quitting: Stop team agents is saved and stays after a reload and an engine restart", { timeout: 600000 }, (t) => onEngine(t, "ca-quit-stop-", async ({ bridge, me, page, open, reload, restart }) => {
  await quitting(me, open);
  await me.click(STOP_CARD, "label.radio-card");
  await me.click("Save changes", SAVE);
  await until("Stop team agents in the engine", ["stop", "after_hours", 24], () => stored(bridge));
  await reload("What happens to team agents when CrazyAgent quits.");
  await until("Stop team agents after a reload", ["stop", "after_hours", "24", false], () => page.evaluate(SHOWN));
  await restart();
  await until("Stop team agents after an engine restart", ["stop", "after_hours", 24], () => stored(bridge));
  await open("#/settings/quit", "the Quitting setting", (text) => text.includes("What happens to team agents when CrazyAgent quits."));
  await until("Stop team agents on the page after the restart", ["stop", "after_hours", "24", false], () => page.evaluate(SHOWN));
  await me.capture("quitting-stop");
}));

test("Quitting: 1 hour after CrazyAgent quits sets a server left running to end one hour after the engine came back, and hours outside 1 to 168 are refused", { timeout: 600000 }, (t) => onEngine(t, "ca-quit-hours-", async ({ bridge, me, page, open, runtime, restart }) => {
  await quitting(me, open);
  await hours(page, "169");
  await me.waitFor("the hours refused before anything is sent", (text) => text.includes("Enter a whole number of hours from 1 to 168."));
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null`), true, "Save stays off for 169 hours");
  await hours(page, "1");
  await me.click("Save changes", SAVE);
  await until("1 hour in the engine", ["keep_running", "after_hours", 1], () => stored(bridge));
  let back = 0;
  await restart(() => {
    leftServer(runtime);
    back = Math.floor(Date.now() / 1000);
  });
  await until("the engine that came back moves the server's end", true, () => deadline(runtime) > back + HOUR_S / 2);
  const ends = deadline(runtime) - back;
  const ready = Math.floor(Date.now() / 1000) - back;
  assert.ok(ends >= HOUR_S - SLACK_S && ends <= HOUR_S + ready + SLACK_S, `the server ends one hour after the engine came back: ${ends} s, engine ready after ${ready} s`);
  await me.capture("quitting-after-hours");
}));

test("Quitting: No limit sets a server left running to end no sooner than ten years after the engine came back, keeping the hours", { timeout: 600000 }, (t) => onEngine(t, "ca-quit-never-", async ({ bridge, me, page, open, reload, runtime, restart }) => {
  await quitting(me, open);
  await hours(page, "48");
  await me.click("Save changes", SAVE);
  await until("48 hours in the engine", ["keep_running", "after_hours", 48], () => stored(bridge));
  await me.click("No limit, until CrazyAgent opens again", "label.radio-card");
  await me.click("Save changes", SAVE);
  await until("No limit in the engine, with the 48 hours kept", ["keep_running", "never", 48], () => stored(bridge));
  await reload("What happens to team agents when CrazyAgent quits.");
  await until("No limit after a reload", ["keep_running", "never", "48", true], () => page.evaluate(SHOWN));
  let back = 0;
  await restart(() => {
    leftServer(runtime);
    back = Math.floor(Date.now() / 1000);
  });
  await until("the engine that came back moves the server's end ten years ahead", true, () => deadline(runtime) >= back + TEN_YEARS_S - SLACK_S);
  await me.capture("quitting-never");
}));
