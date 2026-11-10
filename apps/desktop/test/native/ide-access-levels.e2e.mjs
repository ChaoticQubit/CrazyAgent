import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, endTerminals, owner, required, serveApp, stop, value } from "./e2e-harness.mjs";
import { BUILD_MS, FOLDER, IDE_FAILED, OWNER_EMAIL, OWNER_NAME, SETTLE_MS, WORKBENCH_MS, git, ideAnswers, input, onboard, stack, terminalRunner, workbench, workbenchUrl } from "./ide-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const PANEL_TERMINAL = `document.querySelectorAll(".part.panel .xterm").length > 0`;
const RAISE = "Raise project access to use this";
const MENU_TITLE = "New terminal at access level";
const DEEP = `const all = (root, selector) => [...root.querySelectorAll(selector), ...[...root.querySelectorAll("*")].filter((item) => item.shadowRoot).flatMap((item) => all(item.shadowRoot, selector))];`;
const CENTRE = `const centre = (item) => { const box = item.getBoundingClientRect(); return { x: box.left + box.width / 2, y: box.top + box.height / 2 }; };`;
const MENU_ACTION = `(() => {
  ${DEEP} ${CENTRE}
  const action = all(document, ".part.panel .action-item .action-label").find((item) => (item.getAttribute("aria-label") ?? item.title ?? "").includes(${JSON.stringify(MENU_TITLE)}));
  return action ? centre(action) : null;
})()`;
const MENU_ROWS = `(() => {
  ${DEEP} ${CENTRE}
  const rows = all(document, ".monaco-menu .action-item").filter((item) => item.querySelector(".action-menu-item")).map((item) => ({
    label: (item.querySelector(".action-label")?.textContent ?? "").trim(),
    disabled: item.classList.contains("disabled") || item.querySelector(".disabled") !== null,
    at: centre(item),
  }));
  return rows.length ? rows : null;
})()`;
const TRUST = `(() => ({
  asked: [...document.querySelectorAll(".monaco-dialog-box")].some((box) => (box.textContent ?? "").includes("Do you trust the authors of the files in this")),
  restricted: [...document.querySelectorAll(".part.banner")].some((banner) => (banner.textContent ?? "").includes("Restricted Mode is intended for safe code browsing.")),
}))()`;
const DISTRUST = `(() => {
  ${CENTRE}
  const button = [...document.querySelectorAll(".monaco-dialog-box .monaco-button")].find((item) => (item.textContent ?? "").includes("No, I don't trust the authors"));
  return button ? centre(button) : null;
})()`;
const CRAZYAGENT_VIEWS = ["Memories", "Agents", "Change sets", "Brief and documents"];
const CRAZYAGENT_PANES = `(() => {
  const titles = [...document.querySelectorAll(".part.sidebar .pane-header .title")].map((item) => (item.textContent ?? "").trim());
  return ${JSON.stringify(CRAZYAGENT_VIEWS)}.every((name) => titles.includes(name)) ? titles : null;
})()`;
const LEVELS = [
  { access: "sandboxed", label: "Sandboxed", confined: true, report: ["net-no", "outside-no", "config-no", "hooks-no"], menu: [["Sandboxed", false], [`Own branch - ${RAISE}`, true], [`Full access - ${RAISE}`, true]] },
  { access: "own_branch", label: "Own branch", confined: true, report: ["net-yes", "outside-no", "config-no", "hooks-no"], menu: [["Sandboxed", false], ["Own branch", false], [`Full access - ${RAISE}`, true]] },
  { access: "full", label: "Full access", confined: false, report: ["net-yes", "outside-yes", "config-yes", "hooks-yes"], menu: [["Sandboxed", false], ["Own branch", false], ["Full access", false]] },
];

const PICKS = [
  ["Sandboxed", ["net-no", "outside-no", "config-no", "hooks-no"]],
  ["Own branch", ["net-yes", "outside-no", "config-no", "hooks-no"]],
  ["Full access", ["net-yes", "outside-yes", "config-yes", "hooks-yes"]],
];

/** A git repository with one commit by the owner and a hooks folder. Input: the folder.
 * Output: none. */
function seedRepo(folder) {
  mkdirSync(folder, { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "README.md"), "# Levels\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "config", "user.name", OWNER_NAME);
  git(folder, "config", "user.email", OWNER_EMAIL);
  git(folder, "add", ".");
  git(folder, "commit", "-q", "-m", "Start");
  mkdirSync(join(folder, ".git", "hooks"), { recursive: true });
  git(folder, "config", "--unset", "user.name");
  git(folder, "config", "--unset", "user.email");
}

/** Saves a project's level the way the owner's Save does, keeping its other choices. Inputs:
 * the bridge, the project, the level and the generation counter. Output: none. */
async function setLevel(bridge, project, access, next) {
  const view = await value(bridge, "desktop_project_agent_policy", { id: project });
  const policy = { access, network: view.network, toolchains: [], ide_git_hooks: view.ide_git_hooks };
  const receipt = await value(bridge, "desktop_set_project_agent_policy", { id: project, version: view.version, policy, generation: next() });
  assert.equal(receipt.outcome, "applied", `the level is saved: ${JSON.stringify(receipt)}`);
  assert.equal((await value(bridge, "desktop_project_agent_policy", { id: project })).access, access);
}

/** The first process running an opened server, found by its private token file. Input: the
 * opened server. Output: its pid. */
function serverPid(server) {
  const listed = spawnSync("/bin/ps", ["-axww", "-o", "pid=,command="], { encoding: "utf8" });
  assert.equal(listed.status, 0, "ps lists processes");
  const line = listed.stdout.split("\n").find((entry) => entry.includes(server.token_path));
  assert.ok(line, `no process runs the server with ${server.token_path}`);
  return Number(line.trim().split(/\s+/)[0]);
}

/** Whether a process runs under a Seatbelt profile, as the kernel reports it. `sandbox-exec`
 * replaces itself with the program it starts, so a process listing cannot tell. Input: the
 * pid. Output: true when it is confined. */
function confined(pid) {
  const code = "import ctypes,sys;print(ctypes.CDLL(None).sandbox_check(int(sys.argv[1]),None,0))";
  const asked = spawnSync("/usr/bin/python3", ["-I", "-c", code, String(pid)], { encoding: "utf8", env: { PATH: "/usr/bin:/bin" } });
  assert.equal(asked.status, 0, asked.stderr);
  return asked.stdout.trim() === "1";
}

/** The commands that report what a terminal may do, one word per check. Inputs: the listening
 * port and a file outside the project. Output: the commands. */
function probe(port, outside) {
  return [
    `/usr/bin/nc -z -G 5 127.0.0.1 ${port} && echo net-yes || echo net-no`,
    `echo x >> ${outside} && echo outside-yes || echo outside-no`,
    `echo x >> .git/config && echo config-yes || echo config-no`,
    `echo x >> .git/hooks/pre-commit && echo hooks-yes || echo hooks-no`,
  ].join("; ");
}

/** Opens a project's IDE tab, downloading the build if this engine has none yet, and waits for
 * the engine to open its server. Inputs: the page and the stage. Output: none. */
async function openTab(page, { site, project, opened, evidence }) {
  const me = owner(page, evidence);
  const known = opened.length;
  const tabs = () => page.evaluate(`[...document.querySelectorAll("[role=tab]")].map((item) => item.id)`);
  await page.send("Page.enable");
  await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
  await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}` });
  for (const deadline = Date.now() + STEP_MS; !(await tabs()).includes("tab-overview"); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, "the project page never showed its tabs");
  }
  await pause(SETTLE_MS);
  if ((await me.seen()).includes("The IDE isn't downloaded yet.")) {
    await me.click("Download", ".pj-ide-unavailable button");
    await me.waitFor("the download under way", (text) => !text.includes("The IDE isn't downloaded yet."));
  }
  for (const deadline = Date.now() + BUILD_MS; !(await tabs()).includes("tab-ide"); await pause(POLL_MS * 10)) {
    const text = await me.seen();
    assert.ok(!IDE_FAILED.test(text), `the IDE download reports a failure: ${text.slice(0, 600)}`);
    assert.ok(Date.now() < deadline, "the IDE build never became Ready");
  }
  await me.click("IDE", "[role=tab]");
  for (const deadline = Date.now() + BUILD_MS; opened.length === known; await pause(POLL_MS * 10)) {
    const text = await me.seen();
    assert.ok(!IDE_FAILED.test(text), `the IDE tab reports a failure: ${text.slice(0, 600)}`);
    assert.ok(Date.now() < deadline, `the IDE never opened: ${text.slice(0, 600)}`);
  }
}

/** One level: a project saved at it, its IDE opened, then the server, workspace trust, the
 * default terminal and for Full access the owner's own shell, each checked in the real
 * workbench. Inputs: the test context, the level and what the run shares. Output: none. */
async function atLevel(t, level, shared) {
  const { bridge, projects, root, next, evidence, port } = shared;
  const repo = join(root, "repos", `level-${level.access.replace("_", "-")}`);
  seedRepo(repo);
  const project = await onboard(bridge, projects, repo, "local-pool", next);
  await setLevel(bridge, project, level.access, next);
  await withPage((page) => openTab(page, { ...shared, project }));
  const server = shared.opened.at(-1);
  assert.equal(confined(serverPid(server)), level.confined, `${level.label}: the server's confinement`);
  await withPage(async (page) => {
    const me = owner(page, evidence);
    const keys = input(page);
    const ide = workbench(page, keys, me);
    const ran = terminalRunner({ ide, keys, me, repo });
    await page.send("Page.enable");
    await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 960, deviceScaleFactor: 1, mobile: false });
    await page.send("Page.navigate", { url: workbenchUrl(server) });
    await ide.until("the workbench with the project's folder", `(() => {
      const explorer = document.querySelector(".explorer-folders-view");
      return document.querySelector(".monaco-workbench .part.activitybar") !== null && explorer !== null && explorer.textContent.includes(${JSON.stringify(FOLDER)});
    })()`, WORKBENCH_MS);
    await pause(SETTLE_MS * 2);

    if (level.confined) {
      await t.test(`${level.label}: workspace trust is off`, async () => {
        assert.deepEqual(await page.evaluate(TRUST), { asked: false, restricted: false });
      });
    }

    if (level.access === "full") {
      await t.test("Full access: workspace trust asks first and the extension's views load in Restricted Mode", async () => {
        await ide.until("the workspace trust question", `${TRUST}.asked`);
        await me.capture("level-full-trust-asked");
        await keys.click(await ide.until("the answer that keeps the folder untrusted", DISTRUST));
        await ide.until("the Restricted Mode banner", `${TRUST}.restricted`);
        await ide.run("View: Show CrazyAgent");
        assert.deepEqual(await ide.until("the CrazyAgent views in Restricted Mode", CRAZYAGENT_PANES), CRAZYAGENT_VIEWS);
        await me.capture("level-full-restricted");
      });
    }

    await t.test(`${level.label}: the first terminal shows its panel`, async () => {
      await ide.run("Terminal: Create New Terminal");
      await pause(SETTLE_MS);
      if ((await page.evaluate(ide.rows))?.length) await ide.pick(FOLDER);
      await ide.until(`the first ${level.label} terminal in the panel`, PANEL_TERMINAL);
      await me.capture(`level-${level.access}-panel`);
    });

    if (level.access === "full") {
      await t.test("Full access: the terminal is the owner's login shell and git knows the owner", async () => {
        await ide.until("the Full access shell's prompt", `document.querySelector(".part.panel .terminal-command-decoration") !== null`);
        await ide.run("Terminal: Focus Terminal");
        const said = await ran("level-shell.txt", `ps -o command= -p $$; git config user.name; git config user.email`);
        assert.match(said.split("\n")[0], /\s-[a-z]*l\b/, said);
        assert.ok(said.split("\n").includes(OWNER_NAME), said);
        assert.ok(said.split("\n").includes(OWNER_EMAIL), said);
        assert.doesNotMatch(said, /fatal|unable to access/, said);
        const stderr = await ran("level-shell-stderr.txt", "git status --porcelain 2>&1 >/dev/null");
        assert.equal(stderr, "finished\n", stderr);
      });
    }

    await t.test(`${level.label}: the terminal reports what the level allows`, async () => {
      await ide.run("Terminal: Focus Terminal");
      await ide.until("a terminal in the panel", PANEL_TERMINAL);
      const said = await ran(`level-${level.access}.txt`, probe(port, join(root, `outside-${level.access}`)));
      writeFileSync(join(evidence, `level-${level.access}.txt`), said);
      for (const word of level.report) assert.match(said, new RegExp(`^${word}$`, "m"), said);
    });

    if (level.access === "own_branch") {
      await t.test("Own branch: Source Control stages and unstages through the engine into the project's real index", async () => {
        const name = "staged-at-own-branch.md";
        writeFileSync(join(repo, name), "Own branch\n");
        const staged = () => git(repo, "diff", "--cached", "--name-only");
        const through = async (command, reached, what) => {
          await ide.run(command);
          for (const deadline = Date.now() + STEP_MS; !reached(staged()); await pause(POLL_MS)) {
            const picker = await page.evaluate(ide.rows);
            if (picker && picker.some((label) => label.includes(basename(repo)))) await ide.pick(basename(repo));
            assert.ok(Date.now() < deadline, `${what}: the real index shows ${JSON.stringify(staged())}`);
          }
        };
        await ide.run("View: Show Changes");
        await ide.until("the Source Control view", ide.sidebar(/source control/i));
        await ide.until("the new file in Source Control", `(document.querySelector(".scm-view")?.textContent ?? "").includes(${JSON.stringify(name)})`);
        assert.equal(staged(), "", "nothing is staged before the IDE stages");
        await through("Git: Stage All Changes", (list) => list.split("\n").includes(name), "the IDE's stage");
        await me.capture("level-own_branch-staged");
        await through("Git: Unstage All Changes", (list) => list === "", "the IDE's unstage");
      });
    }

    await t.test(`${level.label}: the terminal menu offers each level and greys the higher ones`, async () => {
      await keys.click(await ide.until("the terminal menu in the panel's title", MENU_ACTION));
      const rows = await ide.until("the terminal menu's levels", MENU_ROWS);
      await me.capture(`level-${level.access}-menu`);
      assert.deepEqual(rows.map((row) => [row.label, row.disabled]), level.menu);
      if (level.access !== "full") {
        await keys.key("Escape");
        return;
      }
      await keys.key("Escape");
      const prompts = `document.querySelectorAll(".part.panel .terminal-command-decoration").length`;
      for (const [picked, report] of PICKS) {
        const before = await page.evaluate(prompts);
        await keys.click(await ide.until("the terminal menu in the panel's title", MENU_ACTION));
        const offered = await ide.until("the terminal menu's levels", MENU_ROWS);
        await keys.hover(offered.find((row) => row.label === picked).at);
        await pause(SETTLE_MS);
        await keys.key("Enter");
        await pause(SETTLE_MS);
        if ((await page.evaluate(ide.rows))?.length) await ide.pick(FOLDER);
        if (picked === "Full access") await ide.until("the new Full access shell's prompt", `${prompts} > ${before}`);
        const name = picked.split(" ")[0].toLowerCase();
        const said = await ran(`level-menu-${name}.txt`, probe(port, join(root, `outside-menu-${name}`)));
        writeFileSync(join(evidence, `level-menu-${name}.txt`), said);
        for (const word of report) assert.match(said, new RegExp(`^${word}$`, "m"), `${picked}: ${said}`);
      }
    });

    if (level.access === "full") {
      await t.test("Full access: a process its terminal left running is named when the IDE opens again", async () => {
        const said = await ran("level-left.txt", `nohup node -e "setInterval(() => {}, 1000)" > /dev/null 2>&1 & echo $!`);
        const pid = said.split("\n")[0];
        assert.match(pid, /^\d+$/, said);
        try {
          await setLevel(bridge, project, "own_branch", next);
          for (const deadline = Date.now() + STEP_MS; existsSync(server.token_path); await pause(POLL_MS)) {
            assert.ok(Date.now() < deadline, "the Full access server kept running after its level was lowered");
          }
          await setLevel(bridge, project, "full", next);
          const reopened = await value(bridge, "e2e_ide_open", { projectId: project });
          writeFileSync(join(evidence, "level-full-left.txt"), `${reopened.left ?? "none"}\n`);
          const named = (reopened.left ?? "").split(", ").some((item) => item.startsWith(`${pid} `));
          assert.ok(named, `the open named ${JSON.stringify(reopened.left)}, not ${pid}`);
        } finally {
          try {
            process.kill(Number(pid), "SIGKILL");
          } catch {
          }
        }
      });
    }
  });
}

test("a project's IDE server and default terminal run at its access level", { timeout: 2400000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-lv-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
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
  const listener = createServer().listen(0, "127.0.0.1");
  await once(listener, "listening");
  let running;
  let site;
  try {
    running = await stack(engineBinary, bridgeBinary, runtime, env, evidence);
    const { bridge } = running;
    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    const opened = [];
    site = await serveApp(dist, bridge, [], log, ideAnswers(bridge, opened));
    const shared = { bridge, projects, root, next, evidence, site, opened, port: listener.address().port };
    for (const level of LEVELS) await atLevel(t, level, shared);
  } finally {
    log.closing = true;
    site?.server.close();
    listener.close();
    await stop(running?.bridgeProcess);
    await stop(running?.engine);
    await endTerminals(runtime);
    if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
});
