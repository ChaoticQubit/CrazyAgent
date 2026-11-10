import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const SHOWN = `(() => {
  const checked = document.querySelector('input[type=radio][name="finished-agents"]:checked');
  const days = document.getElementById("finished-agents-days");
  return [checked?.value ?? null, days?.value ?? null, days?.disabled ?? null];
})()`;
const SAVE = 'form[aria-label="Remove finished agents\' worktrees"] button[type=submit]';
const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;

/** Runs the owner's git in a folder with no global or system config. Inputs: the folder and
 * the arguments. Output: what git printed, trimmed. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Makes the owner's project folder: a git repository on main with one commit. Input: the
 * folder. Output: none. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Tool shop\n\nKeeps the tools.\n");
  for (const args of [["init", "-q", "-b", "main"], ["add", "."], [...OWNER, "commit", "-q", "-m", "Start the tool shop"]]) git(folder, ...args);
}

/** Adds one finished sub-agent the way the engine leaves it: its row stopped at `stoppedMs`, a
 * worktree of the owner's repository in the project's `.crazyagent/worktrees` folder with its
 * record, and a conversation in its agent folder. Inputs: the engine root, the project, the
 * repository, a label and when it stopped. Output: the agent's id, label and paths. */
function finishedAgent(runtime, project, repo, label, stoppedMs) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,stopped_ms) VALUES (?,'sub','codex',1,'stopped',?,?,?)").run(project, stoppedMs - HOUR_MS, stoppedMs, stoppedMs).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    const name = `agent-${id}`;
    const worktree = join(repo, ".crazyagent", "worktrees", name);
    const gitDir = join(repo, ".git", "worktrees", name);
    git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
    store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(id, project, repo, worktree, gitDir);
    const conversation = join(runtime, "team", "agents", name, "home", "session.jsonl");
    mkdirSync(dirname(conversation), { recursive: true, mode: 0o700 });
    writeFileSync(conversation, "{}\n");
    return { id, label, worktree, gitDir, conversation };
  } finally {
    store.close();
  }
}

/** The text of the first button whose words start with `start`, as the page shows it, or null.
 * Inputs: the page and the start. Output: the words. */
function buttonStarting(page, start) {
  return page.evaluate(`[...document.querySelectorAll("button")].map((button) => (button.innerText ?? "").replace(/\\s+/g, " ").trim()).find((words) => words.startsWith(${JSON.stringify(start)})) ?? null`);
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
 * test context, the temporary folder prefix and the steps, given the project, the bridge,
 * the owner on the page, the page, a route opener, a page reload, the team status reader, the
 * test's root folder, the engine root, the repository, the environment and an engine restart
 * that starts the engine and the bridge again on the same root, as the app's next start does.
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
    const project = receipt.project_id;
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: project });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }
    const status = () => value(bridge, "desktop_team_status", { id: project });
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
        await steps({ project, bridge, me, page, open, reload, status, root, runtime, repo, env, restart });
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
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}

/** Types the days the first option waits into the days field, as the owner's typing does:
 * the field takes the whole text and tells the page it changed. Inputs: the page and the
 * text. Output: none. */
async function days(page, text) {
  const typed = await page.evaluate(`(() => {
    const field = document.getElementById("finished-agents-days");
    if (!field || field.disabled) return false;
    field.focus();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(field, ${JSON.stringify(text)});
    field.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  })()`);
  assert.ok(typed, "the days field takes typing");
}

test("Finished agents: each of the three options is saved in the engine and stays after a reload, and the days stay while another option is picked", { timeout: 600000 }, (t) => onEngine(t, "ca-agent-cleanup-", async ({ me, page, open, reload, bridge }) => {
  const stored = async () => {
    const settings = await value(bridge, "desktop_cleanup_settings");
    return [settings.mode, settings.days];
  };
  const both = async (what, mode, wanted, field) => {
    await until(`${what} in the engine`, [mode, wanted], stored);
    await until(`${what} on the page`, [mode, String(wanted), field], () => page.evaluate(SHOWN));
  };
  await open("#/settings/finished", "the Finished agents setting", (text) => text.includes("Remove finished agents' worktrees"));
  await me.waitFor("the three options", (text) => ["1 day after the agent finishes", "Once its work lands", "Only when I click Clean up"].every((words) => text.includes(words)));
  await both("1 day after the agent finishes, the default", "after_days", 1, false);
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null`), true, "Save waits for a change");

  await days(page, "366");
  await me.waitFor("the days refused before anything is sent", (text) => text.includes("Enter a whole number of days from 1 to 365."));
  assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(SAVE)})?.disabled ?? null`), true, "Save stays off for 366 days");
  await days(page, "3");
  await me.click("Save changes", SAVE);
  await both("3 days after the agent finishes", "after_days", 3, false);
  await reload("Remove finished agents' worktrees");
  await both("3 days after a reload", "after_days", 3, false);
  await me.capture("finished-agents-after-days");

  await me.click("Once its work lands", "label.radio-card");
  await me.click("Save changes", SAVE);
  await both("once its work lands, with the 3 days kept", "once_landed", 3, true);
  await reload("Remove finished agents' worktrees");
  await both("once its work lands after a reload", "once_landed", 3, true);
  await me.capture("finished-agents-once-landed");

  await me.click("Only when I click Clean up", "label.radio-card");
  await me.click("Save changes", SAVE);
  await both("only when the owner clicks Clean up", "manual", 3, true);
  await reload("Remove finished agents' worktrees");
  await both("only when the owner clicks Clean up after a reload", "manual", 3, true);
  await me.capture("finished-agents-manual");

  await me.click("3 days after the agent finishes", "label.radio-card");
  await days(page, "1");
  await me.click("Save changes", SAVE);
  await both("back to the default", "after_days", 1, false);
}));

test("Finished agents: at a real engine start each option removes only the worktrees it names, and Only when I click Clean up waits for the click", { timeout: 900000 }, (t) => onEngine(t, "ca-agent-sweep-", async ({ project, bridge, me, page, open, restart, runtime, repo, env }) => {
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary");
  assert.ok(existsSync(join(repo, ".crazyagent")), "onboarding made the project's state folder");
  git(repo, "config", "user.name", "E2E Owner");
  git(repo, "config", "user.email", "owner@example.invalid");
  const agent = (label, stoppedMs) => finishedAgent(runtime, project, repo, label, stoppedMs);
  const there = (...agents) => agents.map((one) => existsSync(one.worktree));
  const turn = (one) => {
    writeFileSync(join(one.worktree, `${one.label}.md`), `# ${one.label}\n`);
    const result = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, String(one.id), one.label, repo, one.worktree, one.gitDir, `Write ${one.label}.md`], { encoding: "utf8", env: { ...env } });
    assert.equal(result.status, 0, `the engine records the agent's turn: ${result.stderr}`);
    const id = result.stdout.trim();
    assert.match(id, /^[0-9a-f]{32}$/, "the agent's edit makes a change set");
    return id;
  };
  const land = async (one) => {
    const id = turn(one);
    const set = (await value(bridge, "desktop_changes", { projectId: project })).find((change) => change.id === id);
    assert.ok(set, `the change set of ${one.label} is listed`);
    const outcome = await value(bridge, "desktop_accept_change", { id, expectedHead: set.head, message: null, target: null });
    assert.equal(outcome.kind, "landed", `the work of ${one.label} lands: ${JSON.stringify(outcome)}`);
  };
  const pick = async (option, mode) => {
    await open("#/settings/finished", "the Finished agents setting", (text) => text.includes("Remove finished agents' worktrees"));
    await me.click(option, "label.radio-card");
    await me.click("Save changes", SAVE);
    await until(`${option} in the engine`, mode, async () => (await value(bridge, "desktop_cleanup_settings")).mode);
  };
  const now = Date.now();

  assert.equal((await value(bridge, "desktop_cleanup_settings")).mode, "after_days", "1 day after the agent finishes is the default");
  const old = agent("old-work", now - 2 * DAY_MS);
  const recent = agent("recent-work", now - HOUR_MS);
  await land(old);
  await land(recent);
  await restart();
  await until("the worktree past its day gone, the one within it kept", [false, true], () => there(old, recent));
  assert.ok(existsSync(old.conversation), "the removed agent's conversation stays");

  await pick("Once its work lands", "once_landed");
  const unlanded = agent("unlanded-work", now - 2 * DAY_MS);
  turn(unlanded);
  const landed = agent("landed-work", Date.now());
  await land(landed);
  await restart();
  await until("the landed worktrees gone with no wait, the unlanded one kept", [false, false, true], () => there(recent, landed, unlanded));
  assert.ok(existsSync(landed.conversation), "the removed agent's conversation stays");

  await pick("Only when I click Clean up", "manual");
  const waiting = agent("waiting-work", now - 2 * DAY_MS);
  await land(waiting);
  await restart();
  await open(`#/projects/${encodeURIComponent(project)}/agents`, "the Clean up row", (text) => text.includes("Remove worktrees of 1 finished agent"));
  assert.deepEqual(there(waiting, unlanded), [true, true], "nothing went at the engine start before the click");
  await me.capture("finished-agents-before-the-click");
  const row = await buttonStarting(page, "Remove worktrees of 1 finished agent");
  assert.ok(row, "the Clean up button names the one agent that may go");
  await me.click(row, "button");
  await me.waitFor("the confirm dialog", (text) => text.includes("Remove the worktrees of 1 finished agent?"));
  const confirm = await buttonStarting(page, "Remove worktrees and free");
  assert.ok(confirm, "the dialog offers the removal");
  await me.click(confirm, "button");
  await until("the clicked worktree gone, the unlanded one kept", [false, true], () => there(waiting, unlanded));
  assert.ok(existsSync(waiting.conversation), "the removed agent's conversation stays");
  await me.capture("finished-agents-after-the-click");
}));
