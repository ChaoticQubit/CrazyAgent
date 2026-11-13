import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, bridgeCall, closedPort, owner, required, serveApp, stop, value } from "./e2e-harness.mjs";
import { BUILD_MS, FOLDER, GIT, IDE_FAILED, OWNER_EMAIL, OWNER_NAME, SETTLE_MS, SETUP_MS, WORKBENCH_MS, git, ideAnswers, input, onboard, stack, terminalRunner, workbench, workbenchUrl } from "./ide-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const REPO = "seed-shop";
const AGENT = "agent-builder";
const AGENT_LABEL = "Ada (@builder) · Codex";
const AGENT_NAME = "Ada (@builder)";
const OWNER_LINE = "Owner line typed in the IDE.";
const AGENT_LINE = "Now with tomatoes.";
const TYPED = "Owner edit from the IDE";
const ARROWED = "Owner edit from the real IDE";
const UNSAVED = "UnsavedOwnerLineBeforeRestart";
const BACKUP_MS = 3000;
const STOP_MS = 20000;
const RESTORE_MS = 20000;
const PRIMARY = process.platform === "darwin" ? 4 : 2;
const CTRL = 2;
const CHAT_KEYS = process.platform === "darwin" ? CTRL | PRIMARY : CTRL | 1;
const SHIFT = 8;
const LEFTOVER_COMMANDS = ["Terminal: Select Default Profile", "Chat:", "Inline Chat", "Snooze Inline Suggestions", "Sync Account Policy", "Manage Accounts", "Accounts:", "Show Remote Menu", "Install Remote Development Extensions", "Settings Sync"];
const LOOPBACK = /^(127\.0\.0\.1|localhost|[0-9a-v]{52}\.localhost)$/;

/** Whether something accepts connections on a loopback port. Input: the port. Output: the answer. */
function listening(port) {
  return new Promise((done) => {
    const socket = createConnection({ host: "127.0.0.1", port });
    socket.once("connect", () => {
      socket.destroy();
      done(true);
    });
    socket.once("error", () => done(false));
  });
}

/** Starts a real git daemon on 127.0.0.1 serving the bare repositories in a folder, pushes
 * included. The IDE sandbox has no network, so the IDE reaches it only through the engine.
 * Input: the folder. Output: the daemon process and the URL of one served repository. */
async function gitDaemon(served) {
  const port = await closedPort();
  const daemon = spawn(GIT, ["daemon", "--reuseaddr", "--listen=127.0.0.1", `--port=${port}`, `--base-path=${served}`, "--export-all", "--enable=receive-pack", "--informative-errors", served], {
    env: { PATH: "/usr/bin:/bin", HOME: served, GIT_CONFIG_NOSYSTEM: "1" },
    stdio: ["pipe", "ignore", "ignore"],
  });
  for (const deadline = Date.now() + SETUP_MS; !(await listening(port)); await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline && daemon.exitCode === null, "the git daemon never listened on 127.0.0.1");
  }
  return { daemon, url: (name) => `git://127.0.0.1:${port}/${name}` };
}

/** Makes the owner's project: a repository on main with the owner's identity in its own
 * config, one commit, and a bare remote the git daemon serves that main tracks.
 * Inputs: the project folder, the remote's folder and its daemon URL. Output: the commit. */
function ownerRepo(folder, remote, url) {
  writeFileSync(join(folder, "README.md"), "# Seed shop\n\nSells seeds.\n");
  writeFileSync(join(folder, "notes.md"), "Planting notes.\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "config", "user.name", OWNER_NAME);
  git(folder, "config", "user.email", OWNER_EMAIL);
  git(folder, "add", ".");
  git(folder, "commit", "-q", "-m", "Start the seed shop");
  git(remote, "init", "-q", "--bare", "-b", "main");
  git(folder, "remote", "add", "origin", url);
  git(folder, "push", "-q", "-u", "origin", "main");
  git(folder, "config", "--unset", "user.name");
  git(folder, "config", "--unset", "user.email");
  return git(folder, "rev-parse", "HEAD");
}

/** Puts a stopped sub-agent on the project's team the way the engine keeps one: its row and
 * the worktree it works in. Inputs: the engine root, the project, the owner repository and
 * the worktree. Output: the agent's engine id. */
function teamAgent(runtime, project, repo, worktree) {
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?,'sub','codex',1,'stopped',?,?)").run(project, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(id, project, repo, worktree, join(repo, ".git", "worktrees", AGENT));
    return String(id);
  } finally {
    store.close();
  }
}

/** Lists the processes an engine root started that run the IDE's agent host, which must
 * never start while the AI lock holds. Input: the engine root. Output: their command lines. */
function agentHosts(runtime) {
  const listed = spawnSync("/bin/ps", ["-axo", "command="], { encoding: "utf8" });
  assert.equal(listed.status, 0, "ps lists processes");
  return listed.stdout.split("\n").filter((line) => line.includes(runtime) && line.includes("--type=agentHost"));
}

test("the owner edits, reviews an agent change in Source Control, accepts, commits and pushes in the real IDE, with no AI UI", { timeout: 2400000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-ide-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  const served = join(root, "served");
  const remote = join(served, "remote.git");
  const worktree = join(repo, ".crazyagent", "worktrees", AGENT);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, remote]) mkdirSync(folder, { recursive: true, mode: 0o700 });
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
  let daemon;
  try {
    daemon = await gitDaemon(served);
    const start = ownerRepo(repo, remote, daemon.url("remote.git"));
    running = await stack(engineBinary, bridgeBinary, runtime, env, evidence);
    const { bridge } = running;
    const project = await onboard(bridge, projects, repo, "local-pool", next);
    git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
    const member = teamAgent(runtime, project, repo, worktree);
    writeFileSync(join(worktree, "README.md"), `# Seed shop\n\nSells seeds.\n${AGENT_LINE}\n`);
    const recorded = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, member, AGENT_LABEL, repo, worktree, join(repo, ".git", "worktrees", AGENT), "Add tomatoes to the readme"], { encoding: "utf8", env: { ...env } });
    assert.equal(recorded.status, 0, `the engine records the agent's turn: ${recorded.stderr}`);
    assert.match(recorded.stdout.trim(), /^[0-9a-f]{32}$/, "the agent's edit makes a change set");
    const head = () => git(repo, "rev-parse", "HEAD");

    const dist = join(root, "dist");
    await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
    const opened = [];
    site = await serveApp(dist, bridge, [], log, ideAnswers(bridge, opened));

    await t.test("the IDE tab downloads the pinned build and opens the project's IDE", async () => {
      await withPage(async (page) => {
        const me = owner(page, evidence);
        const tabs = () => page.evaluate(`[...document.querySelectorAll("[role=tab]")].map((item) => item.id)`);
        const swapped = async () => (await tabs()).filter((id) => ["tab-ide", "tab-changes", "tab-context"].includes(id));
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}` });
        for (const deadline = Date.now() + STEP_MS; !(await tabs()).includes("tab-overview"); await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, "the project page never showed its tabs");
        }
        await me.waitFor("the project page's line that the IDE isn't downloaded yet", (text) => /IDE unavailable on [^:\n]+: The IDE isn't downloaded yet\./.test(text));
        await me.capture("ide-missing");
        assert.deepEqual(await swapped(), ["tab-changes", "tab-context"], "a Missing build keeps Context and Changes and lists no IDE tab");
        await me.click("Download", ".pj-ide-unavailable button");
        await me.waitFor("the download under way", (text) => !text.includes("The IDE isn't downloaded yet."));
        for (const deadline = Date.now() + BUILD_MS; !(await tabs()).includes("tab-ide"); await pause(POLL_MS * 10)) {
          const text = await me.seen();
          assert.ok(!IDE_FAILED.test(text), `the IDE download reports a failure: ${text.slice(0, 600)}`);
          assert.ok(Date.now() < deadline, "the IDE build never became Ready");
        }
        assert.deepEqual(await swapped(), ["tab-ide", "tab-context"], "a Ready build lists the IDE in place of Changes, with Context still a tab beside it (DEC-152)");
        await me.click("IDE", "[role=tab]");
        await me.capture("ide-tab");
        for (const deadline = Date.now() + BUILD_MS; opened.length === 0; await pause(POLL_MS * 10)) {
          const text = await me.seen();
          assert.ok(!IDE_FAILED.test(text), `the IDE tab reports a failure: ${text.slice(0, 600)}`);
          assert.ok(Date.now() < deadline, "the IDE never opened");
        }
        await me.capture("ide-opened");
        assert.ok(log.calls.includes("desktop_ide_state"), "the tab read the engine's IDE state");
        assert.ok(log.calls.includes("desktop_ide_open"), "the tab asked the engine to open the project's IDE");
      });
      assert.equal(opened.length, 1, "one open for one project");
    });

    await withPage(async (page) => {
      const me = owner(page, evidence);
      const keys = input(page);
      const ide = workbench(page, keys, me);
      const requests = new Map();
      const failed = new Map();
      const answered = new Set();
      page.on("Network.requestWillBeSent", (event) => requests.set(event.requestId, event.request.url));
      page.on("Network.responseReceived", (event) => answered.add(event.requestId));
      page.on("Network.loadingFailed", (event) => failed.set(event.requestId, event.blockedReason ?? event.errorText));
      const server = opened[0];

      await t.test("the workbench opens on a cold start in a fresh browser with no AI UI", async () => {
        assert.ok(server, "blocked: the IDE tab step opened no IDE");
        await page.send("Network.enable");
        await page.send("Page.enable");
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 960, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: workbenchUrl(server) });
        await ide.until("the workbench with the project's folders", `(() => {
          const explorer = document.querySelector(".explorer-folders-view");
          return document.querySelector(".monaco-workbench .part.activitybar") !== null && explorer !== null && explorer.textContent.includes(${JSON.stringify(FOLDER)});
        })()`, WORKBENCH_MS);
        await pause(SETTLE_MS * 2);
        await me.capture("workbench-cold");
        const shown = await page.evaluate(`(() => ({
          welcome: [...document.querySelectorAll(".tabs-container .tab")].map((tab) => tab.getAttribute("aria-label") ?? tab.textContent).filter((name) => /welcome|walkthrough/i.test(name)),
          accounts: [...document.querySelectorAll(".activitybar .action-label, .activitybar .action-item")].filter((item) => /^Accounts/.test(item.getAttribute("aria-label") ?? "") || item.classList.contains("codicon-accounts-view-bar-icon")).length,
          chat: [...document.querySelectorAll(".activitybar [aria-label], .auxiliarybar [aria-label], .titlebar [aria-label], .statusbar [aria-label]")].map((item) => item.getAttribute("aria-label")).filter((name) => /\\bchat\\b|copilot|\\bagent mode\\b/i.test(name)),
          commandCenter: document.querySelector(".command-center") !== null,
          remote: document.getElementById("status.host") !== null,
          chatParts: document.querySelectorAll(".interactive-session, .chat-widget, .inline-chat").length,
          settingsEditors: [...document.querySelectorAll(".tabs-container .tab")].map((tab) => tab.getAttribute("aria-label") ?? tab.textContent).filter((name) => /settings\\.json/i.test(name)),
          saveFailures: [...document.querySelectorAll(".notifications-toasts .notification-toast, .notifications-center .notification-list-item")].map((item) => item.innerText).filter((text) => /failed to save|unable to write|EPERM/i.test(text)),
        }))()`);
        assert.deepEqual(shown.welcome, [], "no Welcome or walkthrough editor opens on a cold start");
        assert.equal(shown.accounts, 0, "no Accounts entry in the activity bar");
        assert.deepEqual(shown.chat, [], "no chat entry anywhere in the workbench chrome");
        assert.equal(shown.commandCenter, false, "no command center, so no Chat in it");
        assert.equal(shown.remote, false, "no Remote indicator in the status bar");
        assert.equal(shown.chatParts, 0, "no chat view or inline chat is open");
        for (const query of LEFTOVER_COMMANDS) {
          const listed = await ide.palette(query);
          await keys.key("Escape");
          assert.deepEqual(listed.filter((label) => label.includes(query)), [], `the F1 palette offers no "${query}" command`);
        }
        assert.deepEqual(shown.saveFailures, [], "no settings file fails to save on a cold start");
        assert.deepEqual(shown.settingsEditors, [], "no settings.json editor opens on a cold start");
      });

      await t.test("the chat shortcut, Open View and the secondary side bar bring back no chat", async () => {
        assert.ok(server, "blocked: the IDE tab step opened no IDE");
        const chatShown = `document.querySelectorAll(".interactive-session, .chat-widget, .inline-chat, .interactive-input-part").length`;
        await page.evaluate(`window.chatKeysSeen = 0; addEventListener("keydown", (event) => { if (event.code === "KeyI" && event.ctrlKey) window.chatKeysSeen += 1; }, true); 1`);
        await keys.key("I", CHAT_KEYS);
        await pause(SETTLE_MS);
        assert.equal(await page.evaluate("window.chatKeysSeen"), 1, "the chat shortcut reached the workbench once");
        assert.equal(await page.evaluate(chatShown), 0, "the chat shortcut opens no chat");
        await ide.run("View: Open View");
        await ide.until("the Open View list", ide.rows);
        await pause(SETTLE_MS);
        const views = await page.evaluate(ide.rows);
        await keys.key("Escape");
        assert.deepEqual(views.filter((label) => /chat/i.test(label)), [], "Open View lists no chat view");
        await ide.run("View: Toggle Secondary Side Bar Visibility");
        await ide.until("the secondary side bar open", `(() => {
          const bar = document.querySelector(".part.auxiliarybar");
          return Boolean(bar && bar.getClientRects().length && bar.getBoundingClientRect().width > 0);
        })()`);
        await pause(SETTLE_MS);
        await me.capture("secondary-side-bar");
        const bar = await page.evaluate(`[...document.querySelectorAll(".part.auxiliarybar [aria-label]")].map((item) => item.getAttribute("aria-label")).filter((name) => /chat/i.test(name) && name !== "Chat actions")`);
        assert.deepEqual(bar, [], "the opened secondary side bar names no chat but its empty toolbar");
        assert.equal(await page.evaluate(chatShown), 0, "the opened secondary side bar holds no chat");
        await ide.run("View: Toggle Secondary Side Bar Visibility");
        await ide.until("the secondary side bar closed", `(() => {
          const bar = document.querySelector(".part.auxiliarybar");
          return !bar || !bar.getClientRects().length || bar.getBoundingClientRect().width === 0;
        })()`);
      });

      await t.test("the owner opens a project file, types and saves, and the edit is in the project folder", async () => {
        await keys.key("P", PRIMARY);
        await ide.until("quick open", ide.rows);
        await keys.text("notes.md");
        await ide.pick("notes.md", FOLDER);
        await ide.until("notes.md open in an editor", `(() => [...document.querySelectorAll(".tabs-container .tab.active")].some((tab) => (tab.getAttribute("aria-label") ?? tab.textContent).includes("notes.md")))()`);
        await ide.until("the editor focused", `document.activeElement?.closest(".monaco-editor") !== null`);
        await keys.text(OWNER_LINE);
        await keys.key("Escape");
        await keys.key("Enter");
        await keys.key("S", PRIMARY);
        for (const deadline = Date.now() + STEP_MS; !readFileSync(join(repo, "notes.md"), "utf8").startsWith(OWNER_LINE); await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, `the saved file never reached the project folder: ${readFileSync(join(repo, "notes.md"), "utf8")}`);
        }
        assert.equal(readFileSync(join(repo, "notes.md"), "utf8"), `${OWNER_LINE}\nPlanting notes.\n`, "exactly the typed line was saved");
        await keys.key("I", PRIMARY);
        await pause(SETTLE_MS);
        assert.equal(await page.evaluate(`document.querySelectorAll(".inline-chat, .interactive-editor, .chat-widget").length`), 0, "the inline chat key opens no chat");
        await keys.key("Escape");
        await me.capture("owner-edit");
      });

      await t.test("F1 finds View: Show Changes and Ctrl+Shift+G opens it", async () => {
        const listed = await ide.palette("View: Show Changes");
        assert.ok(listed.some((label) => label.startsWith("View: Show Changes")), `the F1 palette lists View: Show Changes; it shows ${JSON.stringify(listed)}`);
        await ide.pick("View: Show Changes");
        await ide.until("the Source Control view from the palette", ide.sidebar(/source control/i));
        await keys.key("E", PRIMARY | SHIFT);
        await ide.until("the Explorer again", ide.sidebar(/explorer/i));
        await keys.key("G", CTRL | SHIFT);
        await ide.until("the Source Control view from Ctrl+Shift+G", ide.sidebar(/source control/i));
        await me.capture("source-control");
      });

      await t.test("Source Control stages and unstages through the engine into the project's real index", async () => {
        const staged = () => git(repo, "diff", "--cached", "--name-only");
        const through = async (command, reached, what) => {
          await ide.run(command);
          for (const deadline = Date.now() + STEP_MS; !reached(staged()); await pause(POLL_MS)) {
            const picker = await page.evaluate(ide.rows);
            if (picker && picker.some((label) => label.includes(REPO))) await ide.pick(REPO);
            assert.ok(Date.now() < deadline, `${what}: the real index shows ${JSON.stringify(staged())}`);
          }
        };
        const holdsNotes = (list) => list.split("\n").includes("notes.md");
        assert.equal(staged(), "", "nothing is staged before the IDE stages");
        await through("Git: Stage All Changes", holdsNotes, "the IDE's stage");
        await ide.until("the staged group in Source Control", `(document.querySelector(".scm-view")?.textContent ?? "").includes("Staged Changes")`);
        await me.capture("staged");
        await through("Git: Unstage All Changes", (list) => list === "", "the IDE's unstage");
        await through("Git: Stage All Changes", holdsNotes, "the IDE's second stage");
      });

      await t.test("the commit box takes typed input and arrow keys, and the IDE commits the owner's edit", async () => {
        const before = head();
        const box = await ide.until("the project's commit box", ide.commitBox, WORKBENCH_MS);
        await keys.click(box);
        await keys.text(TYPED);
        await ide.until("the typed message in the commit box", `(${ide.commitBox})?.text === ${JSON.stringify(TYPED)}`);
        for (let step = 0; step < 3; step += 1) await keys.key("ArrowLeft");
        await keys.text("real ");
        const edited = await ide.until("the message edited after three Left arrows", `(() => { const now = (${ide.commitBox})?.text; return now === ${JSON.stringify(ARROWED)} ? now : null; })()`);
        assert.doesNotMatch(edited, /[\u0000-\u001f\u007f]/, "arrow keys insert no control characters");
        await keys.key("ArrowRight");
        await keys.key("ArrowRight");
        await keys.key("ArrowRight");
        assert.equal((await page.evaluate(ide.commitBox))?.text, ARROWED, "Right arrows move the caret without changing the message");
        await me.capture("commit-box");
        await keys.key("Enter", PRIMARY);
        for (const deadline = Date.now() + STEP_MS; head() === before; await pause(POLL_MS)) {
          const yes = await page.evaluate(ide.dialogButton("Yes"));
          if (yes) await keys.click(yes);
          assert.ok(Date.now() < deadline, "the IDE never committed");
        }
        assert.equal(git(repo, "log", "-1", "--format=%s"), ARROWED, "the commit carries the message typed in the commit box");
        assert.equal(git(repo, "log", "-1", "--format=%an <%ae>"), `${OWNER_NAME} <${OWNER_EMAIL}>`, "the IDE commits under the owner's own Git name");
        assert.equal(git(repo, "show", "HEAD:notes.md"), `${OWNER_LINE}\nPlanting notes.`, "the commit holds the owner's edit");
        assert.equal(git(repo, "status", "--porcelain"), "", "nothing is left uncommitted, so CrazyAgent can land on the branch");
      });

      await t.test("the agent's change set shows in Source Control and Accept lands it under the owner's name", async () => {
        const before = head();
        await ide.until("the agent's change set in Source Control", `(() => {
          const view = document.querySelector(".scm-view");
          return Boolean(view && view.textContent.includes("CrazyAgent changes") && view.textContent.includes(${JSON.stringify(AGENT_NAME)}) && view.textContent.includes("README.md"));
        })()`, WORKBENCH_MS);
        assert.equal(await page.evaluate(`document.querySelectorAll('.scm-view .codicon-sparkle, .scm-view [aria-label*="Generate Commit Message"]').length`), 0, "no AI commit message action in Source Control");
        const found = await ide.until("the change set's group row", ide.group(AGENT_NAME));
        await keys.hover(found.row);
        const accept = await ide.until("the group's Accept action", `(() => { const found = ${ide.group(AGENT_NAME)}; return found && found.action; })()`);
        await keys.click(accept);
        await ide.until("the Accept commit message box", `(() => {
          const widget = document.querySelector(".quick-input-widget");
          return Boolean(widget && widget.getClientRects().length && widget.textContent.includes("Accept"));
        })()`);
        await me.capture("accept-message");
        await keys.key("Enter");
        for (const deadline = Date.now() + STEP_MS; head() === before; await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, "Accept never landed a commit on the owner's branch");
        }
        assert.equal(git(repo, "rev-parse", "HEAD^"), before, "Accept lands exactly one commit on top of the owner's branch");
        assert.equal(git(repo, "log", "-1", "--format=%an <%ae>"), `${OWNER_NAME} <${OWNER_EMAIL}>`, "the landed commit is under the owner's own Git name");
        assert.ok(readFileSync(join(repo, "README.md"), "utf8").includes(AGENT_LINE), "the agent's change is in the project folder");
        assert.equal(git(repo, "show", "HEAD:notes.md"), `${OWNER_LINE}\nPlanting notes.`, "landing keeps the owner's own commit");
        assert.equal(git(repo, "status", "--porcelain"), "", "landing leaves the owner's checkout clean");
        await ide.until("the change set gone from Source Control", `!(document.querySelector(".scm-view")?.textContent ?? "").includes(${JSON.stringify(AGENT_NAME)})`);
        await me.capture("accepted");
      });

      await t.test("the IDE pushes the branch through the engine to the git daemon on 127.0.0.1", async () => {
        await ide.run("Git: Push");
        const pushed = () => git(remote, "rev-parse", "main");
        for (const deadline = Date.now() + STEP_MS; pushed() !== head(); await pause(POLL_MS * 5)) {
          const picker = await page.evaluate(ide.rows);
          if (picker && picker.some((label) => label.includes(REPO))) await ide.pick(REPO);
          assert.ok(Date.now() < deadline, "the push never reached the git daemon");
        }
        assert.equal(git(repo, "rev-parse", "refs/remotes/origin/main"), head(), "the engine's push moved the project's origin/main, which the IDE sandbox cannot reach");
        assert.equal(git(remote, "log", "-1", "--format=%s", "main^"), ARROWED, "the remote holds the owner's IDE commit");
        assert.ok(git(remote, "show", "main:README.md").includes(AGENT_LINE), "the remote holds the accepted agent change");
        assert.notEqual(pushed(), start, "the remote moved past the first commit");
        await me.capture("pushed");
      });

      await t.test("Settings and the terminal show no AI search, Chat section or terminal AI hint", async () => {
        await ide.run("Preferences: Open Settings (UI)");
        await ide.until("the Settings editor", `document.querySelector(".settings-editor") !== null && document.querySelector(".settings-toc-container .monaco-list-row") !== null`);
        await pause(SETTLE_MS);
        const settings = await page.evaluate(`(() => ({
          toggle: document.querySelectorAll('.settings-editor .codicon-sparkle, .settings-editor [aria-label*="AI"]').length,
          toc: [...document.querySelectorAll(".settings-toc-container .monaco-list-row")].map((row) => row.textContent.trim()),
        }))()`);
        await me.capture("settings");
        assert.equal(settings.toggle, 0, "the Settings search has no AI toggle");
        assert.deepEqual(settings.toc.filter((entry) => /^chat\b/i.test(entry)), [], "the Settings table of contents has no Chat section");
        await keys.key("Escape");
        await ide.until("the modal Settings editor closed", `document.querySelector(".modal-editor-part, .monaco-modal-editor-block") === null`);
        await ide.run("Terminal: Create New Terminal");
        await pause(SETTLE_MS * 2);
        await me.capture("terminal");
        assert.equal(await page.evaluate(`document.querySelectorAll(".terminal-initial-hint").length`), 0, "the terminal shows no AI hint");
      });

      const ran = terminalRunner({ ide, keys, me, repo });
      const answers = async (file) => {
        const said = await ran(file, `command -v git; git --version; git config user.name; git config user.email; echo "HOME=$HOME"`);
        const lines = said.split("\n");
        assert.ok(lines.some((line) => line.startsWith("git version ")), `${file}: ${said}`);
        assert.ok(lines.some((line) => line.startsWith("HOME=") && line.includes("/ide-shell/")), `${file}: ${said}`);
        assert.ok(!lines.includes(OWNER_NAME), `${file}: ${said}`);
        assert.ok(!lines.includes(OWNER_EMAIL), `${file}: ${said}`);
        assert.doesNotMatch(said, /fatal|unable to access/, `${file}: ${said}`);
      };

      await t.test("the project's terminal reads git but cannot stage or commit", async () => {
        await keys.key("Escape");
        await ide.run("Terminal: Create New Terminal");
        await ide.pick(FOLDER);
        await ide.until("the project's terminal in the panel", `document.querySelectorAll(".part.panel .xterm").length > 0`);
        await me.capture("sandboxed-terminal");
        assert.equal(await page.evaluate(`document.querySelectorAll(".terminal-initial-hint").length`), 0, "the open terminal shows no AI hint");
        const started = await ran("sandboxed-terminal-started.txt", "echo started");
        assert.match(started, /^started$/m, `the login shell starts after the pane's line: ${started}`);
        await answers("sandboxed-terminal-git.txt");
        const refused = await ran("sandboxed-terminal-refused-git.txt", `echo staged > terminal-refused.txt; git add terminal-refused.txt; echo "add=$?"; git -c user.name=o -c user.email=o@example.invalid commit -q -m no; echo "commit=$?"; git status --porcelain`);
        assert.match(refused, /^add=[1-9]/m, refused);
        assert.match(refused, /^commit=[1-9]/m, refused);
        assert.match(refused, /^\?\? terminal-refused\.txt$/m, refused);
      });

      await t.test("nothing the workbench asked for left the machine, and the agent host never started", async () => {
        const outside = [...requests].filter(([, url]) => {
          const address = new URL(url);
          return ["http:", "https:", "ws:", "wss:"].includes(address.protocol) && !LOOPBACK.test(address.hostname);
        });
        for (const [id, url] of outside) {
          assert.ok(!answered.has(id), `a request left the machine and was answered: ${url}`);
          assert.equal(failed.get(id), "csp", `the policy blocked ${url} before it was sent`);
        }
        assert.deepEqual(agentHosts(runtime), [], "no IDE agent host process runs");
      });

      await t.test("a saved access level restarts the IDE, and the Save's words match what happens to an unsaved edit", async () => {
        await ide.run("File: New Untitled Text File");
        await ide.until("the editor focused", `document.activeElement?.closest(".monaco-editor") !== null`);
        await keys.text(UNSAVED);
        await ide.until("the edit marked unsaved", `document.querySelector(".tabs-container .tab.active.dirty") !== null`);
        await pause(BACKUP_MS);
        await me.capture("unsaved-before-restart");
        const view = await value(bridge, "desktop_project_agent_policy", { id: project });
        const access = view.access === "sandboxed" ? "own_branch" : "sandboxed";
        const saved = await value(bridge, "desktop_set_project_agent_policy", { id: project, version: view.version, policy: { access, network: view.network, toolchains: [], ide_git_hooks: false }, generation: next() });
        assert.equal(saved.outcome, "applied", `the level is saved: ${JSON.stringify(saved)}`);
        for (const deadline = Date.now() + STOP_MS; existsSync(server.token_path); await pause(POLL_MS)) {
          assert.ok(Date.now() < deadline, "the IDE server kept running after its level was saved");
        }
        let asked = false;
        page.on("Page.javascriptDialogOpening", () => {
          asked = true;
          void page.send("Page.handleJavaScriptDialog", { accept: true });
        });
        const reopened = await bridgeCall(bridge, "e2e_ide_open", { projectId: project });
        assert.ok("value" in reopened, `the IDE opens again: ${JSON.stringify(reopened)}`);
        const origin = await page.evaluate("performance.timeOrigin");
        await page.send("Page.navigate", { url: workbenchUrl(reopened.value) });
        await ide.until("the workbench again", `performance.timeOrigin > ${origin} && document.querySelector(".monaco-workbench .part.activitybar") !== null`, WORKBENCH_MS);
        let back = false;
        for (const deadline = Date.now() + RESTORE_MS; !back && Date.now() < deadline; await pause(POLL_MS)) {
          back = await page.evaluate(`[...document.querySelectorAll(".view-line")].some((line) => line.textContent.includes(${JSON.stringify(UNSAVED)}))`);
        }
        await me.capture("unsaved-after-restart");
        writeFileSync(join(evidence, "unsaved-after-restart.txt"), `${back ? "returned" : "lost"} prompt ${asked}\n`);
        const said = readFileSync(join(appRoot, "src/screens/project/access.tsx"), "utf8").includes("Unsaved edits are lost.");
        assert.equal(said, !back, `an unsaved edit was ${back ? "returned" : "lost"} after the restart, and the Save ${said ? "says" : "does not say"} it is lost`);
      });
    });
  } finally {
    log.closing = true;
    site?.server.close();
    await stop(running?.bridgeProcess);
    await stop(running?.engine);
    await stop(daemon?.daemon);
    if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
});
