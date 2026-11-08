import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { LIB, ROUTES, commands, plan, render } from "./bridge-routes.mjs";
import { AGENTS_VIEW, INTERNALS, POLL_MS, STEP_MS, closedPort, owner, required, serveApp, start as launch, stop, value } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const REPO = "land-shop";
const SETUP_MS = 120000;
const GIT = "/usr/bin/git";
const AGENT = "agent-lander";
const AGENT_LABEL = "Lin (@lander) · Codex";
const OWNER = ["-c", "user.name=E2E Owner", "-c", "user.email=owner@example.invalid"];
const README = "# Land shop\n\nSells land.\n";

/** Runs git in a folder with no system or user config. Inputs: the folder and git's
 * arguments. Output: git's trimmed stdout. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", ...args], { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Makes the owner's project: main with one commit, a stage branch one commit ahead of it and
 * checked out nowhere, and the owner's checkout on feat/x with a notes commit of its own, so
 * a change set measured from the owner's checkout would count the notes file too. Input: the
 * folder. Output: the tips of main, stage and feat/x. */
function ownerRepo(folder) {
  writeFileSync(join(folder, "README.md"), README);
  git(folder, "init", "-q", "-b", "main");
  git(folder, "add", ".");
  git(folder, ...OWNER, "commit", "-q", "-m", "Start the land shop");
  const main = git(folder, "rev-parse", "HEAD");
  git(folder, "checkout", "-q", "-b", "stage");
  writeFileSync(join(folder, "STAGE.md"), "Staged work.\n");
  git(folder, "add", ".");
  git(folder, ...OWNER, "commit", "-q", "-m", "Stage the shop");
  const stage = git(folder, "rev-parse", "HEAD");
  git(folder, "checkout", "-q", "-b", "feat/x", main);
  writeFileSync(join(folder, "NOTES.md"), "Owner notes.\n");
  git(folder, "add", ".");
  git(folder, ...OWNER, "commit", "-q", "-m", "Keep notes");
  git(folder, "config", "user.name", "E2E Owner");
  git(folder, "config", "user.email", "owner@example.invalid");
  return { main, stage, feat: git(folder, "rev-parse", "HEAD") };
}

/** Puts a sub-agent on the project's team, by default stopped with its worktree, the way the
 * engine keeps one between turns, or idle, as one resting between turns that no worker of
 * this engine runs, with or without the worktree. Inputs: the engine root, the project, the
 * owner repository, the worktree, the status and whether it has the worktree. Output: the
 * agent's engine id. */
function teamAgent(runtime, project, repo, worktree, status = "stopped", tree = true) {
  assert.ok(["stopped", "idle"].includes(status), status);
  const store = new DatabaseSync(join(runtime, "engine.db"));
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const now = Date.now();
    const id = Number(store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES (?,'sub','codex',1,?,?,?)").run(project, status, now, now).lastInsertRowid);
    assert.ok(Number.isSafeInteger(id) && id > 0, "the engine numbers the team agent");
    if (tree) store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(id, project, repo, worktree, join(repo, ".git", "worktrees", AGENT));
    return String(id);
  } finally {
    store.close();
  }
}

/** The engine's prompts to one agent, oldest first, read from its database. Inputs: the
 * engine root and the agent. Output: the prompt texts. */
function prompts(runtime, agent) {
  const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
  try {
    return store.prepare("SELECT body FROM coord_messages WHERE from_kind='engine' AND to_agent=? AND kind='prompt' ORDER BY id").all(Number(agent)).map((row) => row.body);
  } finally {
    store.close();
  }
}

/** Starts the real engine and the bridge on a new project made from the owner's repository,
 * serves the built app to a browser page, runs the steps and stops everything. Inputs: the
 * test context, the temporary folder prefix and the steps, given the project, its folders,
 * the branch tips, the owner on the page, a turn recorder, the team graph reader and a
 * stand-in agent's start or finish of work through the engine. Output: none. */
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
  const repo = join(root, "repos", REPO);
  const worktree = join(root, "agents", AGENT);
  for (const folder of [runtime, evidence, home, join(home, ".codex"), projects, repo, join(root, "agents")]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  const tips = ownerRepo(repo);
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
    const recordTurn = (member, prompt) => {
      const result = spawnSync(bridgeBinary, ["record-turn-end", runtime, project, member, AGENT_LABEL, repo, worktree, join(repo, ".git", "worktrees", AGENT), prompt], { encoding: "utf8", env: { ...env } });
      assert.equal(result.status, 0, `the engine records the agent's turn: ${result.stderr}`);
      const id = result.stdout.trim();
      assert.match(id, /^[0-9a-f]{32}$/, "an edited worktree makes a change set");
      return id;
    };
    const status = () => value(bridge, "desktop_team_status", { id: project });
    const graph = () => value(bridge, "desktop_team_graph", { id: project });
    const stand = (mode, ...args) => {
      const result = spawnSync(bridgeBinary, [mode, runtime, ...args], { encoding: "utf8", env: { ...env } });
      assert.equal(result.status, 0, `the stand-in agent's ${mode}: ${result.stderr}`);
      return result.stdout.trim();
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
        const open = (tab) => page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(project)}/${tab}` });
        await steps({ runtime, project, repo, worktree, tips, me, page, open, status, graph, stand, recordTurn, addAgent: (state, tree) => teamAgent(runtime, project, repo, worktree, state, tree) });
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

/** Saves one branch setting through its form on the Agents tab and waits until the engine
 * reports it. Inputs: the owner on the page, the form's label, the input's id, the branch,
 * the setting's field in the team status and the status reader. Output: none. */
async function saveBranch(me, form, input, branch, field, status) {
  await me.type(`[id="${input}"]`, branch);
  await me.click("Save", `form[aria-label="${form}"] button`);
  for (const deadline = Date.now() + STEP_MS; (await status())[field] !== branch; await pause(POLL_MS)) {
    assert.ok(Date.now() < deadline, `the engine never kept ${field} ${branch}`);
  }
}

/** Accepts the open change set with the Accept dialog's preselected branch and waits for the
 * landed note. Inputs: the owner on the page, the sentence the dialog says and the branch.
 * Output: the page text with the note. */
async function acceptPreselected(me, lands, branch) {
  await me.click("Accept");
  await me.waitFor("the accept question naming the Land on branch", (text) => /Accept these changes\?/.test(text) && text.includes(lands));
  await me.click("Accept", "dialog button");
  return me.waitFor("the landed change set", (text) => new RegExp(`Accepted as [0-9a-f]{7} on ${branch}\\.`).test(text));
}

test("Land on: the master's branch receives a finished agent's merge by default and again after the owner switches back from a named branch", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-lo-m-", async ({ project, repo, worktree, tips, me, page, open, status, recordTurn, addAgent }) => {
    await open("agents?section=access");
    await me.waitFor("the Land on setting in Agent settings", (text) => text.includes("Land on") && text.includes("Empty uses the master's branch."));
    assert.equal((await status()).land_on ?? null, null, "Land on starts as the master's branch");
    await saveBranch(me, "Master's branch", `tg-master-branch-${project}`, "main", "master_branch", status);
    await saveBranch(me, "Land on", `tg-land-on-${project}`, "stage", "land_on", status);
    await me.click("Use the master's branch");
    for (const deadline = Date.now() + STEP_MS; ((await status()).land_on ?? null) !== null; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "the engine never went back to the master's branch");
    }
    for (const deadline = Date.now() + STEP_MS; await page.evaluate(`document.getElementById(${JSON.stringify(`tg-land-on-${project}`)}).value`) !== ""; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "the Land on field still names a branch");
    }
    await me.capture("land-on-master-branch");

    git(repo, "worktree", "add", "-q", "--detach", worktree, tips.main);
    const member = addAgent();
    writeFileSync(join(worktree, "README.md"), `${README}Now with plots.\n`);
    recordTurn(member, "Add plots");
    await open("changes");
    await me.waitFor("the agent's change set measured from main", (text) => text.includes(AGENT_LABEL) && /Ready to review/.test(text));
    await me.follow(".ch-row");
    const review = await me.waitFor("the change set's diff", (text) => /Now with plots\./.test(text));
    assert.match(review, /1 file, 1 line added, 0 removed/, "the change set counts only what main lacks, not the owner's notes on feat/x");
    assert.ok(!review.includes("NOTES.md"), "the owner's own commit on feat/x is no part of the change set");

    const accepted = await acceptPreselected(me, "They land as one commit on main, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are.", "main");
    await me.capture("land-on-master-accepted");
    const landed = git(repo, "rev-parse", "main");
    assert.ok(accepted.includes(`Accepted as ${landed.slice(0, 7)} on main.`), "the note names the commit on the master's branch");
    assert.equal(git(repo, "rev-parse", `${landed}^`), tips.main, "one squash commit on top of main");
    assert.equal(git(repo, "show", `${landed}:README.md`), `${README}Now with plots.`.trimEnd(), "main holds the agent's work");
    assert.equal(git(repo, "rev-parse", "HEAD"), tips.feat, "the owner's checked-out feat/x is not moved");
    assert.equal(git(repo, "symbolic-ref", "HEAD"), "refs/heads/feat/x", "the owner's checkout stays on feat/x");
    assert.equal(readFileSync(join(repo, "README.md"), "utf8"), README, "the owner's files stay as they are");
    assert.equal(git(repo, "status", "--porcelain"), "", "the owner's checkout has no stray changes");
  });
});

test("Land on: a named branch checked out nowhere receives the merge and the owner's checkout is untouched", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-lo-n-", async ({ project, repo, worktree, tips, me, open, status, recordTurn, addAgent }) => {
    await open("agents?section=access");
    await me.waitFor("the Land on setting in Agent settings", (text) => text.includes("Land on") && text.includes("Empty uses the master's branch."));
    await me.type(`[id="tg-land-on-${project}"]`, "not a branch");
    await me.click("Save", 'form[aria-label="Land on"] button');
    await me.waitFor("the plain refusal for a bad branch name", (text) => text.includes("Type a branch name such as stage or release/2.0, or leave it empty for the master's branch."));
    assert.equal((await status()).land_on ?? null, null, "a refused name changes nothing");
    await saveBranch(me, "Land on", `tg-land-on-${project}`, "stage", "land_on", status);
    await me.capture("land-on-stage");

    git(repo, "worktree", "add", "-q", "--detach", worktree, tips.stage);
    const member = addAgent();
    writeFileSync(join(worktree, "README.md"), `${README}Now with plots.\n`);
    recordTurn(member, "Add plots");
    await open("changes");
    await me.waitFor("the agent's change set measured from stage", (text) => text.includes(AGENT_LABEL) && /Ready to review/.test(text));
    await me.follow(".ch-row");
    const review = await me.waitFor("the change set's diff", (text) => /Now with plots\./.test(text));
    assert.match(review, /1 file, 1 line added, 0 removed/, "the change set counts only what stage lacks");
    assert.ok(!review.includes("NOTES.md") && !review.includes("STAGE.md"), "neither the owner's notes nor stage's own file count");

    const accepted = await acceptPreselected(me, "They land as one commit on stage, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are.", "stage");
    await me.capture("land-on-stage-accepted");
    const landed = git(repo, "rev-parse", "stage");
    assert.ok(accepted.includes(`Accepted as ${landed.slice(0, 7)} on stage.`), "the note names the commit on stage");
    assert.equal(git(repo, "rev-parse", `${landed}^`), tips.stage, "one squash commit on top of stage");
    assert.equal(git(repo, "show", `${landed}:STAGE.md`), "Staged work.", "stage keeps its own work");
    assert.equal(git(repo, "rev-parse", "main"), tips.main, "main is not moved");
    assert.equal(git(repo, "rev-parse", "HEAD"), tips.feat, "the owner's checked-out feat/x is not moved");
    assert.equal(readFileSync(join(repo, "README.md"), "utf8"), README, "the owner's files stay as they are");
    assert.ok(!existsSync(join(repo, "STAGE.md")), "stage's files never reach the owner's checkout");
    assert.equal(git(repo, "status", "--porcelain"), "", "the owner's checkout has no stray changes");
  });
});

test("Land on: a deleted Land on branch is named in the Accept dialog, which lands nowhere until the owner picks a branch", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-lo-d-", async ({ project, repo, worktree, tips, me, page, open, status, recordTurn, addAgent }) => {
    await open("agents?section=access");
    await me.waitFor("the Land on setting in Agent settings", (text) => text.includes("Land on") && text.includes("Empty uses the master's branch."));
    await saveBranch(me, "Land on", `tg-land-on-${project}`, "stage", "land_on", status);

    git(repo, "worktree", "add", "-q", "--detach", worktree, tips.stage);
    const member = addAgent();
    writeFileSync(join(worktree, "README.md"), `${README}Now with plots.\n`);
    recordTurn(member, "Add plots");
    git(repo, "branch", "-D", "stage");
    await open("changes");
    await me.waitFor("the agent's change set still waiting", (text) => text.includes(AGENT_LABEL) && /Ready to review/.test(text));
    await me.follow(".ch-row");
    await me.waitFor("the change set's diff", (text) => /Now with plots\./.test(text));
    await me.click("Accept");
    const missing = "Your Land on branch stage is not a local branch of this project. Pick a branch, or change Land on in Team settings.";
    const asked = await me.waitFor("the Accept dialog naming the missing Land on branch", (text) => /Accept these changes\?/.test(text) && text.includes(missing));
    await me.capture("land-on-missing-accept");
    assert.ok(!asked.includes("They land as one commit on"), "the dialog names no branch the changes would land on");
    const dialog = await page.evaluate(`(() => {
      const select = document.querySelector("dialog select");
      const accept = [...document.querySelectorAll("dialog button")].find((button) => button.innerText.trim() === "Accept");
      return { value: select?.value ?? null, first: select?.options[0]?.text ?? null, disabled: accept?.disabled ?? null };
    })()`);
    assert.deepEqual(dialog, { value: "", first: "Pick a branch", disabled: true }, "no branch is picked and Accept waits for one");
    await me.click("Keep reviewing", "dialog button");
    await me.waitFor("the dialog closed", (text) => !/Accept these changes\?/.test(text));
    assert.equal(git(repo, "rev-parse", "main"), tips.main, "main is not moved");
    assert.equal(git(repo, "rev-parse", "HEAD"), tips.feat, "the owner's checked-out feat/x is not moved");
    assert.equal(git(repo, "symbolic-ref", "HEAD"), "refs/heads/feat/x", "the owner's checkout stays on feat/x");
    assert.equal(git(repo, "status", "--porcelain"), "", "the owner's checkout has no stray changes");
  });
});

test("Land on: under Wait a start on a file another agent's work holds waits, the waiting agent's card and row say for which file, and accepting that work lets the start go through", { timeout: 600000 }, async (t) => {
  await onEngine(t, "ca-lo-w-", async ({ runtime, project, repo, worktree, tips, me, open, status, graph, stand, recordTurn, addAgent }) => {
    await open("agents?section=settings");
    await me.waitFor("the Overlapping edits and Land on settings in Agent settings", (text) => text.includes("Overlapping edits") && text.includes("Land on"));
    await me.click("Wait", '[role=radiogroup][aria-label="Overlapping edits"] [role=radio]');
    for (const deadline = Date.now() + STEP_MS; (await status()).coordination?.settings.overlaps !== "wait"; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "the engine never kept Overlapping edits at Wait");
    }
    await saveBranch(me, "Land on", `tg-land-on-${project}`, "stage", "land_on", status);

    git(repo, "worktree", "add", "-q", "--detach", worktree, tips.stage);
    const lander = addAgent("idle");
    const waiter = addAgent("idle", false);
    const started = stand("start-work", project, lander, "Add plots", "README.md");
    const work = /^started (\d+)$/.exec(started)?.[1] ?? assert.fail(`the lander's start: ${started}`);
    const label = async (agent) => (await graph()).nodes.find((node) => node.id === `team-${agent}`)?.label ?? assert.fail(`no card for agent ${agent}`);
    const [holder, waiting] = [await label(lander), await label(waiter)];
    assert.equal(stand("start-work", project, waiter, "Fix plots", "README.md"),
      `Wait: ${holder}'s work #${work} holds README.md until it lands. You will get a message when it is free.`, "the waiting start reads who holds the file");
    assert.equal((await graph()).nodes.find((node) => node.id === `team-${waiter}`)?.waits_for, "README.md", "the engine's graph names the file the agent waits for");
    await me.click("Team", AGENTS_VIEW);
    await me.waitFor("the waiting agent's card naming the file", (text) => text.includes(waiting) && text.includes("Waiting for README.md."));
    await me.capture("wait-card");
    await open("conversation");
    await me.waitFor("the waiting agent's row in the team strip naming the file", (text) => text.includes("Team") && text.includes("Waiting for README.md."));
    await me.capture("wait-strip");

    writeFileSync(join(worktree, "README.md"), `${README}Now with plots.\n`);
    assert.equal(stand("finish-work", lander, work), "2", "the lander's work is review ready");
    recordTurn(lander, "Add plots");
    assert.deepEqual(prompts(runtime, waiter), [], "review-ready work holds the file until it lands");
    await open("changes");
    await me.waitFor("the lander's change set", (text) => text.includes(AGENT_LABEL) && /Ready to review/.test(text));
    await me.follow(".ch-row");
    await me.waitFor("the change set's diff", (text) => /Now with plots\./.test(text));
    await acceptPreselected(me, "They land as one commit on stage, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are.", "stage");
    for (const deadline = Date.now() + STEP_MS; prompts(runtime, waiter).length === 0; await pause(POLL_MS)) {
      assert.ok(Date.now() < deadline, "the waiting agent was never told the file is free");
    }
    assert.deepEqual(prompts(runtime, waiter), ["README.md is free now: start your work again."]);
    assert.match(stand("start-work", project, waiter, "Fix plots", "README.md"), /^started \d+$/, "the start goes through once the file is free");
    assert.equal((await graph()).nodes.find((node) => node.id === `team-${waiter}`)?.waits_for ?? null, null, "the agent waits for nothing now");
    await open("agents");
    await me.waitFor("the agent's card no longer waiting", (text) => text.includes(waiting) && text.includes("Team graph") && !text.includes("Waiting for README.md."));
    await me.capture("wait-over");
  });
});
