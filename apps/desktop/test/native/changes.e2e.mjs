import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const REPO = "seed-shop";
const SETUP_MS = 120000;

const GIT = "/usr/bin/git";
const AGENT = "agent-builder";
const AGENT_LABEL = "Ada (@builder) \u00b7 Codex";
const XSS = '<img src=x onerror="window.__changesXss=1">';
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];

/** Runs git in a folder with no system or user config, so only the repository's own config
 * counts. Inputs: the folder and git's arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Makes the owner's project: a git repository on main with one commit by the owner and no
 * stored identity. Input: the folder. Output: the commit. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), "# Seed shop\n\nSells seeds.\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "add", ".");
  git(folder, ...OWNER, "commit", "-q", "-m", "Start the seed shop");
  return git(folder, "rev-parse", "HEAD");
}

/** Makes the edits a sub-agent leaves in its worktree: a text change with markup in it, a
 * runnable script, a link, a binary file, and the two files that change how Git treats the
 * project. Input: the worktree. Output: none. */
function agentEdits(worktree) {
  writeFileSync(join(worktree, "README.md"), `# Seed shop\n\nSells seeds.\nNow with tomatoes.\n${XSS}\n`);
  mkdirSync(join(worktree, "bin"));
  writeFileSync(join(worktree, "bin", "grow.sh"), "#!/bin/sh\necho grow\n");
  chmodSync(join(worktree, "bin", "grow.sh"), 0o755);
  symlinkSync("README.md", join(worktree, "latest"));
  writeFileSync(join(worktree, "logo.png"), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d]));
  writeFileSync(join(worktree, ".gitattributes"), "*.png binary\n");
  writeFileSync(join(worktree, ".gitmodules"), "[submodule \"vendor\"]\n\tpath = vendor\n\turl = ./vendor\n");
}

test("the owner reviews, discards, restores, accepts and updates an agent's changes on the real engine", { timeout: 600000 }, async (t) => {
  const engineBinary = required("E2E_ENGINE", "crazyagent engine binary");
  const bridgeBinary = required("E2E_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  assert.equal(readFileSync(ROUTES, "utf8"), render(readFileSync(LIB, "utf8")), "routes.rs is stale: run node test/native/bridge-routes.mjs");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(tmpdir(), "ca-ch-")));
  const runtime = join(root, "engine");
  const evidence = process.env.E2E_EVIDENCE && isAbsolute(process.env.E2E_EVIDENCE) ? process.env.E2E_EVIDENCE : join(root, "evidence");
  const home = join(root, "home");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  const worktree = join(root, "agents", AGENT);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, join(root, "agents")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const start = ownerRepo(repo);
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
    const receipt = await value(bridge, "desktop_start_onboarding", { path: repo, pool: "local-pool", generation: 3 });
    assert.equal(receipt.outcome, "applied", `the folder becomes a project: ${JSON.stringify(receipt)}`);
    const project = receipt.project_id;
    for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
      const view = await value(bridge, "desktop_onboarding", { id: project });
      if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
      assert.ok(Date.now() < deadline, `onboarding still running: ${JSON.stringify(view)}`);
    }

    assert.ok(existsSync(join(runtime, "engine.db")), "the engine keeps its database in its root");
    git(repo, "worktree", "add", "-q", "--detach", worktree, "HEAD");
    const recordTurn = (prompt) => {
      const result = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, AGENT, AGENT_LABEL, repo, worktree, join(repo, ".git", "worktrees", AGENT), prompt], { encoding: "utf8", env: { ...env } });
      assert.equal(result.status, 0, `the engine records the agent's turn: ${result.stderr}`);
      const id = result.stdout.trim();
      assert.match(id, /^[0-9a-f]{32}$/, "an edited worktree makes a change set");
      return id;
    };
    const head = () => git(repo, "rev-parse", "HEAD");

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
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/changes` });
        const tab = await me.waitFor("the Changes tab with no change sets yet", (text) => /No changes yet/.test(text) || /Not ready yet/.test(text), STEP_MS);
        await me.capture("changes-empty");
        assert.doesNotMatch(tab, /Not ready yet/, "the engine lands changes, so the Changes tab is ready");
        assert.match(tab, /When the master or a sub-agent edits files, its changes wait here for you to review and accept\./);
        assert.ok(log.calls.includes("desktop_changes"), "the tab read the engine's change sets");

        agentEdits(worktree);
        const first = recordTurn("Add tomatoes and a grow script");
        const rows = () => page.evaluate("document.querySelectorAll('.ch-row').length");
        await me.waitFor("the agent's change set in the list", (text) => text.includes(AGENT_LABEL) && /Ready to review/.test(text));
        await me.follow(".ch-row");
        const review = await me.waitFor("the change set's files and diff", (text) => /Now with tomatoes\./.test(text) && text.includes(".gitmodules: changes the project's submodules"));
        await me.capture("changes-review");
        assert.match(review, /6 files, \d+ lines? added, 0 removed/, "the row says how big the change is");
        assert.ok(review.includes("Not checked by CrazyAgent. Any test results in it are reported by the agent."));
        for (const mark of ["Can run as a program", "Symbolic link: points to another path", "Binary file, no text diff", ".gitattributes: changes how Git treats files", ".gitmodules: changes the project's submodules"]) {
          assert.ok(review.includes(mark), `the files list marks: ${mark}`);
        }
        for (const line of ["Modified README.md", "Added bin/grow.sh", "Added latest", "Added logo.png", "Added .gitattributes", "Added .gitmodules"]) {
          assert.ok(review.includes(line), `the files list names: ${line}`);
        }
        const diff = await page.evaluate("document.querySelector('pre.ch-diff')?.textContent ?? null");
        assert.ok(diff?.includes("+Now with tomatoes."), "the diff is the engine's text");
        assert.ok(diff.includes(`+${XSS}`), "markup in the diff shows as text");
        assert.equal(await page.evaluate("document.querySelector('.ch-detail img, .ch-diff *') === null && window.__changesXss === undefined"), true, "markup in the diff never renders or runs");

        await me.click("Discard");
        await me.waitFor("the discard question", (text) => /Discard these changes\?/.test(text));
        await me.click("Discard", "dialog button");
        const discarded = await me.waitFor("the discarded change set", (text) => /You can restore these changes until /.test(text));
        assert.ok(discarded.includes("Discarded. You can restore these changes for 7 days."), "the owner is told the discard can be undone");
        assert.equal(head(), start, "a discard leaves the owner's branch alone");
        await me.click("Restore");
        await me.waitFor("the restored change set", (text) => text.includes("Restored. The changes are ready to review again.") && /Ready to review/.test(text));

        await me.click("Accept");
        await me.waitFor("the accept question", (text) => /Accept these changes\?/.test(text) && text.includes("under your own Git name. CrazyAgent did not check them."));
        await me.click("Accept", "dialog button");
        await me.waitFor("the plain refusal for a missing Git identity", (text) => text.includes("Git has no name and email for you in your project folder. Set user.name and user.email, then accept again."));
        await me.capture("changes-no-identity");
        assert.equal(head(), start, "a refused Accept changes nothing");

        git(repo, "config", "user.name", "E2E Owner");
        git(repo, "config", "user.email", "owner@example.invalid");
        await me.click("Accept");
        await me.click("Accept", "dialog button");
        const accepted = await me.waitFor("the landed change set", (text) => /Accepted as [0-9a-f]{7} on main\./.test(text));
        await me.capture("changes-accepted");
        const landed = head();
        assert.ok(accepted.includes(`Accepted as ${landed.slice(0, 7)} on main.`), "the note names the commit on the owner's branch");
        assert.equal(git(repo, "rev-parse", `${landed}^`), start, "one squash commit on top of the owner's branch");
        assert.equal(git(repo, "log", "-1", "--format=%an <%ae>"), "E2E Owner <owner@example.invalid>", "the owner's own Git identity authored it");
        assert.equal(git(repo, "status", "--porcelain"), "", "the owner's checkout matches the commit");
        assert.ok(readFileSync(join(repo, "README.md"), "utf8").includes("Now with tomatoes."));
        assert.ok(statSync(join(repo, "bin", "grow.sh")).mode & 0o100, "the script stays runnable");
        assert.ok(lstatSync(join(repo, "latest")).isSymbolicLink() && readlinkSync(join(repo, "latest")) === "README.md", "the link stays a link");

        writeFileSync(join(repo, "README.md"), readFileSync(join(repo, "README.md"), "utf8").replace("# Seed shop", "# Seed shop, open daily"));
        git(repo, "commit", "-q", "-am", "Open daily");
        const daily = head();
        writeFileSync(join(worktree, "README.md"), readFileSync(join(worktree, "README.md"), "utf8").replace("# Seed shop", "# Seed shop for gardens"));
        const second = recordTurn("Name the shop for gardens");
        assert.notEqual(second, first, "a turn after an Accept opens a new change set");
        for (const deadline = Date.now() + STEP_MS; await rows() < 2; await pause(POLL_MS)) assert.ok(Date.now() < deadline, "the new change set reaches the list");
        await me.follow(".ch-row");
        await me.waitFor("the second change set", (text) => /Ready to review/.test(text) && /Seed shop for gardens/.test(text));
        await me.click("Accept");
        await me.click("Accept", "dialog button");
        await me.waitFor("the conflict sentence", (text) => text.includes("These files also changed on your branch: README.md. Ask the agent to update, or discard."));
        assert.equal(head(), daily, "a conflicting Accept changes nothing");
        const conflicted = await me.waitFor("the conflict state", (text) => /Conflicts with your branch/.test(text));
        await me.capture("changes-conflict");
        assert.equal(conflicted.split("These files also changed on your branch").length - 1, 1, "the owner reads the conflict sentence once");

        await me.click("Ask agent to update");
        await me.waitFor("the updating change set", (text) => /Agent is updating/.test(text) && text.includes("The agent is resolving the conflicts with your branch. Review again when it finishes."));
        await me.capture("changes-updating");
        assert.match(readFileSync(join(worktree, "README.md"), "utf8"), /^<<<<<<< /m, "the update merged the owner's branch into the agent's worktree with conflict markers");
        assert.equal(head(), daily, "an update never moves the owner's branch");
        for (const command of ["desktop_change_diff", "desktop_discard_change", "desktop_restore_change", "desktop_accept_change", "desktop_update_change"]) {
          assert.ok(log.calls.includes(command), `the page called ${command}`);
        }
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
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(log.trail, null, 1));
    site?.server.close();
    await stop(bridgeProcess);
    await stop(engine);
  }
  if (!process.env.E2E_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
});
