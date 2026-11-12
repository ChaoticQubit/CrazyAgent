import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { POLL_MS, STEP_MS, bridgeCall, closedPort, endTerminals, owner, required, serveApp, stop, value } from "./e2e-harness.mjs";
import { FOLDER, OWNER_EMAIL, OWNER_NAME, SETTLE_MS, WORKBENCH_MS, git, ideAnswers, input, onboard, openTab, setLevel, stack, workbench, workbenchUrl } from "./ide-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const TERMINAL_KEYS = `document.activeElement?.classList.contains("xterm-helper-textarea") === true`;
const ATTEMPTS_MAX = 8;
const COUNT_FILE = "run-count";
const COUNT_ONCE = "run-once\n";

/** A git repository with one commit by the owner and the file the counted command edits.
 * Input: the folder. Output: none. */
function seedRepo(folder) {
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "README.md"), "# Run\n");
  writeFileSync(join(folder, COUNT_FILE), "run\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "config", "user.name", OWNER_NAME);
  git(folder, "config", "user.email", OWNER_EMAIL);
  git(folder, "add", ".");
  git(folder, "commit", "-q", "-m", "Start");
  git(folder, "config", "--unset", "user.name");
  git(folder, "config", "--unset", "user.email");
}

/** Asks the real engine, through the bridge, to type one command into the project's IDE
 * terminal, the call Confirm makes. A "busy" bridge is asked again, and so is an IDE whose
 * extension has not subscribed yet when `waitForIde` is set. Inputs: the bridge, the project,
 * the command and whether to wait for the IDE. Output: null once the IDE typed it, else the
 * refusal code. */
async function ask(bridge, project, command, waitForIde = false) {
  const deadline = Date.now() + STEP_MS;
  for (let attempt = 1; ; attempt += 1) {
    const answer = await bridgeCall(bridge, "desktop_ide_run", { projectId: project, command });
    if ("value" in answer) {
      assert.equal(answer.value, null, "a typed command answers no data");
      return null;
    }
    const code = answer.failure?.code;
    assert.equal(typeof code, "string", `desktop_ide_run answered ${JSON.stringify(answer)}`);
    const again = (code === "busy" && attempt < ATTEMPTS_MAX) || (waitForIde && code === "ide_not_running" && Date.now() < deadline);
    if (!again) return code;
    await pause(250 * Math.min(attempt, ATTEMPTS_MAX));
  }
}

/** Waits for a file the typed command makes. Inputs: what is waited for and the path.
 * Output: none; fails when the file never appears. */
async function appears(what, path) {
  for (const deadline = Date.now() + STEP_MS; !existsSync(path); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `${what}: ${path} never appeared`);
  }
}

test("Run in project terminal types one confirmed command into the project's IDE terminal", { timeout: 2400000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-run-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", "run");
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, join(root, "repos")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
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
  writeFileSync(join(home, ".gitconfig"), `[user]\n\tname = ${OWNER_NAME}\n\temail = ${OWNER_EMAIL}\n`);
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: tmpdir(), LANG: "en_US.UTF-8" };
  const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
  let generation = 0;
  const next = () => (generation += 1);
  let running;
  let site;
  try {
    running = await stack(engineBinary, bridgeBinary, runtime, env, evidence);
    const { bridge } = running;
    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    const opened = [];
    site = await serveApp(dist, bridge, [], log, ideAnswers(bridge, opened));
    seedRepo(repo);
    const project = await onboard(bridge, projects, repo, "local-pool", next);
    await setLevel(bridge, project, "sandboxed", next);
    const mode = (wanted) => value(bridge, "desktop_ide_set_run_mode", { projectId: project, mode: wanted });

    await t.test("the setting starts on Type and run, and a run with no IDE open is refused", async () => {
      assert.equal(await value(bridge, "desktop_ide_run_mode", { projectId: project }), "type_and_run");
      assert.equal(await ask(bridge, project, "touch run-no-ide"), "ide_not_running");
      assert.equal(await ask(bridge, project, "touch run-a; touch run-b"), "ide_run_chained", "the rule is checked before the IDE is looked for");
    });

    await withPage((page) => openTab(page, { site, project, opened, evidence }));
    const server = opened.at(-1);
    await withPage(async (page) => {
      const me = owner(page, evidence);
      const keys = input(page);
      const ide = workbench(page, keys, me);
      await page.send("Page.enable");
      await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 960, deviceScaleFactor: 1, mobile: false });
      await page.send("Page.navigate", { url: workbenchUrl(server) });
      await ide.until("the workbench with the project's folder", `(() => {
        const explorer = document.querySelector(".explorer-folders-view");
        return document.querySelector(".monaco-workbench .part.activitybar") !== null && explorer !== null && explorer.textContent.includes(${JSON.stringify(FOLDER)});
      })()`, WORKBENCH_MS);
      await pause(SETTLE_MS * 2);

      await t.test("Type and run: the command runs once in a new IDE terminal", async () => {
        assert.equal(await ask(bridge, project, `sed -i '' -e 's/run/run-once/' ${COUNT_FILE}`, true), null);
        for (const deadline = Date.now() + STEP_MS; readFileSync(join(repo, COUNT_FILE), "utf8") !== COUNT_ONCE; await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, `the command never ran: ${JSON.stringify(readFileSync(join(repo, COUNT_FILE), "utf8"))}`);
        }
        await pause(SETTLE_MS * 2);
        assert.equal(readFileSync(join(repo, COUNT_FILE), "utf8"), COUNT_ONCE, "the command ran exactly once");
        assert.ok(!existsSync(join(repo, "run-no-ide")), "the run refused before the IDE opened was never typed later");
        await me.capture("run-type-and-run");
      });

      await t.test("Sandboxed: the typed command cannot write outside the project", async () => {
        const outside = join(root, "run-escape");
        assert.equal(await ask(bridge, project, `touch ${outside}`), null);
        assert.equal(await ask(bridge, project, "touch run-inside"), null);
        await appears("the command inside the project", join(repo, "run-inside"));
        await pause(SETTLE_MS * 2);
        assert.ok(!existsSync(outside), "a Sandboxed project's terminal wrote outside the project");
      });

      await t.test("a chained line and a line that hides a second command are refused by the engine and nothing is typed", async () => {
        assert.equal(await ask(bridge, project, "touch run-a; touch run-b"), "ide_run_chained");
        assert.equal(await ask(bridge, project, "touch run-a\ntouch run-b"), "ide_run_not_one_line");
        for (const hidden of ["cat =(touch run-hidden)", "ls *(e:'touch run-hidden':)", "eval $'touch run-a\\x3b touch run-hidden'", 'eval "touch run-a${IFS:2:1}touch run-hidden"']) {
          assert.equal(await ask(bridge, project, hidden), "ide_run_chained", hidden);
        }
        await pause(SETTLE_MS * 2);
        for (const name of ["run-a", "run-b", "run-hidden"]) assert.ok(!existsSync(join(repo, name)), `a refused line reached the terminal: ${name}`);
      });

      await t.test("Type only: the command is typed and runs only once the owner presses Enter", async () => {
        assert.equal(await mode("type_only"), "type_only");
        assert.equal(await value(bridge, "desktop_ide_run_mode", { projectId: project }), "type_only");
        assert.equal(await ask(bridge, project, "touch run-typed"), null);
        await pause(SETTLE_MS * 2);
        assert.ok(!existsSync(join(repo, "run-typed")), "Type only pressed Enter");
        await me.capture("run-type-only-typed");
        if (!(await page.evaluate(TERMINAL_KEYS))) await ide.run("Terminal: Focus Terminal");
        await ide.until("the terminal has the keyboard", TERMINAL_KEYS);
        await keys.key("Enter");
        await appears("the typed command after Enter", join(repo, "run-typed"));
      });

      await t.test("Off: the engine refuses the run and nothing is typed", async () => {
        assert.equal(await mode("off"), "off");
        assert.equal(await ask(bridge, project, "touch run-off"), "ide_run_off");
        await pause(SETTLE_MS * 2);
        assert.ok(!existsSync(join(repo, "run-off")), "Off typed the command");
        assert.equal(await mode("type_and_run"), "type_and_run");
      });
    });
  } finally {
    log.closing = true;
    site?.server.close();
    await stop(running?.bridgeProcess);
    await stop(running?.engine);
    await endTerminals(runtime);
    if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
});
