import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { SevereServiceError } from "webdriverio";
import { closedPort, start, stop, value } from "./e2e-harness.mjs";
import { PICKS_VARIABLE, attach, stopRelaunched } from "./trial-app.mjs";

process.umask(0o077);
const mode = process.env.TRIAL3_MODE;
assert(["live", "fixture"].includes(mode), "TRIAL3_MODE must be live or fixture");
const app = process.env.TRIAL3_APP;
const evidence = process.env.TRIAL_EVIDENCE;
const port = Number(process.env.WD_PORT);
for (const [name, path] of [["TRIAL3_APP", app], ["TRIAL_EVIDENCE", evidence]]) {
  assert(path && isAbsolute(path), `${name} must be an absolute path`);
}
assert(Number.isInteger(port) && port > 1024 && port < 65536, "WD_PORT must be a free local port");
const fixture = mode === "fixture" ? process.env.TRIAL3_FIXTURE_ROOT : null;
const GIT = "/usr/bin/git";
const SEED_MS = 900000;
const POLL_MS = 500;
const QUESTION = "Which greeting should greet.py print?";
const FIXTURE_SKILLS = ["trial3-alpha", "trial3-beta"];
let engine;
let seeding;

if (mode === "live") {
  const socket = process.env.CRAZYAGENT_DESKTOP_SOCKET;
  assert(socket && isAbsolute(socket), "live mode needs CRAZYAGENT_DESKTOP_SOCKET, the running engine's absolute socket path");
  assert(process.env.TRIAL_PROJECT && isAbsolute(process.env.TRIAL_PROJECT), "live mode needs TRIAL_PROJECT, the seeded project folder");
} else {
  assert(fixture && isAbsolute(fixture), "fixture mode needs TRIAL3_FIXTURE_ROOT, an absolute path for a new folder");
  for (const name of ["TRIAL3_ENGINE", "TRIAL3_BRIDGE"]) assert(process.env[name] && isAbsolute(process.env[name]), `fixture mode needs ${name} as an absolute path`);
  assert(Buffer.byteLength(join(fixture, "engine", "engine.sock")) < 100, "TRIAL3_FIXTURE_ROOT must be short enough for the engine socket path");
  process.env.CRAZYAGENT_DESKTOP_SOCKET = join(fixture, "engine", "engine.sock");
  process.env.TRIAL_PROJECT = join(fixture, "home", "Projects", "ca-trial-3");
  process.env.TRIAL_PROMPT ??= join(fixture, "prompt.md");
  process.env.TRIAL_TEAM_MS ??= "90000";
}
process.env.TRIAL_PROMPT ??= fileURLToPath(new URL("../../../../scripts/trial/prompt.md", import.meta.url));
process.env.CRAZYAGENT_DESKTOP_TEST_EVIDENCE = join(evidence, "screenshots");
process.env[PICKS_VARIABLE] = join(evidence, "picks");
const picks = { [PICKS_VARIABLE]: process.env[PICKS_VARIABLE] };
const appEnv = fixture
  ? { CRAZYAGENT_DESKTOP_SOCKET: process.env.CRAZYAGENT_DESKTOP_SOCKET, HOME: join(fixture, "home"), CFFIXED_USER_HOME: join(fixture, "home"), TMPDIR: join(fixture, "tmp"), ...picks }
  : { CRAZYAGENT_DESKTOP_SOCKET: process.env.CRAZYAGENT_DESKTOP_SOCKET, ...picks };

/** Runs git with no system or user config. Inputs: the folder and git's arguments.
 * Output: git's trimmed stdout; throws with git's stderr when it fails. */
function git(folder, ...args) {
  const result = spawnSync(GIT, ["-c", "commit.gpgsign=false", "-c", "user.name=Trial Owner", "-c", "user.email=owner@example.invalid", ...args],
    { cwd: folder, encoding: "utf8", env: { PATH: "/usr/bin:/bin", HOME: folder, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Seeds the fixture project as the trial runner seeds ca-trial-3: one commit on main with
 * the project rule, the trial3-land branch and an untracked private folder. Input: the folder. */
function seedProject(folder) {
  mkdirSync(join(folder, "private"), { recursive: true });
  writeFileSync(join(folder, "CLAUDE.md"), "# ca-trial-3 rules\n\nTRIAL3-PROJECT-RULE: every Python function has a one-line docstring.\n");
  writeFileSync(join(folder, "greet.py"), "def greet():\n    \"\"\"Prints the greeting.\"\"\"\n    print(\"Hi\")\n");
  writeFileSync(join(folder, ".gitignore"), "private/\n");
  writeFileSync(join(folder, "private", "note.txt"), "TRIAL3-HIDDEN-MARK: agents must not read this file.\n");
  git(folder, "init", "-q", "-b", "main");
  git(folder, "add", ".");
  git(folder, "commit", "-q", "-m", "Initial trial repo");
  git(folder, "branch", "trial3-land");
}

/** Writes the fixture engine's settings: one local pool whose only connection points at a
 * closed port, so setup's mapping agent fails at once and no model ever runs. Input: the
 * engine folder. */
async function writeSettings(runtime) {
  const provider = await closedPort();
  writeFileSync(join(runtime, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:${provider}/v1" }, model = { id = "trial-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
}

/** Writes the stand-in Codex executable the fixture engine starts with, so its team runtime
 * runs its tick; it exits at once and answers nothing. Input: the fixture root. Output: its path. */
function standInHarness(root) {
  const path = join(root, "bin", "codex");
  writeFileSync(path, "#!/bin/sh\nexit 1\n", { mode: 0o700 });
  return path;
}

/** Waits until the fixture engine holds the trial project and reads its id. Input: the engine
 * folder. Output: the project id; throws when no project names the folder within SEED_MS. */
async function projectId(runtime) {
  const folder = JSON.stringify(realpathSync(process.env.TRIAL_PROJECT)).slice(1, -1);
  for (const deadline = Date.now() + SEED_MS; Date.now() < deadline; await pause(POLL_MS)) {
    if (!existsSync(join(runtime, "engine.db"))) continue;
    const store = new DatabaseSync(join(runtime, "engine.db"), { readOnly: true });
    try {
      store.exec("PRAGMA busy_timeout = 5000");
      const found = store.prepare("SELECT id, body FROM projects").all().find((row) => String(row.body).includes(folder));
      if (found) return String(found.id);
    } catch (failure) {
      if (!/no such table/.test(String(failure))) throw failure;
    } finally {
      store.close();
    }
  }
  throw new Error(`the fixture engine never held a project for ${process.env.TRIAL_PROJECT}`);
}

/** Runs one desktop-e2e-bridge command against the fixture engine. Input: its arguments.
 * Output: its trimmed stdout; throws with its stderr when it fails. */
function bridge(...args) {
  const result = spawnSync(process.env.TRIAL3_BRIDGE, args, { encoding: "utf8", timeout: 30000, killSignal: "SIGKILL", env: { PATH: "/usr/bin:/bin", HOME: join(fixture, "home"), LANG: "en_US.UTF-8" } });
  assert.equal(result.status, 0, `desktop-e2e-bridge ${args[0]}: ${result.stderr}`);
  return result.stdout.trim();
}

/** Puts the team a live trial grows on the fixture project once it exists, through the
 * engine's own tables and functions: a native Claude lead with two teammates, an app-spawned
 * lead with two sub-agents, one change set per lead from the edit its worktree holds at turn
 * end and one question for the owner from the first sub-agent. Output: what was seeded, for
 * the fixture log. */
async function seedTeam() {
  const runtime = join(fixture, "engine");
  const project = await projectId(runtime);
  const repo = realpathSync(process.env.TRIAL_PROJECT);
  const store = new DatabaseSync(join(runtime, "engine.db"));
  const now = Date.now();
  const leads = [];
  let asker;
  try {
    store.exec("PRAGMA busy_timeout = 5000");
    const insert = store.prepare("INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,turns,parent,pane) VALUES (?,?,?,1,'idle',?,?,1,?,?)");
    const agent = (role, harness, parent, pane) => Number(insert.run(project, role, harness, now, now, parent, pane).lastInsertRowid);
    const native = agent("sub", "claude", null, null);
    agent("teammate", "claude", native, "%11");
    agent("teammate", "claude", native, "%12");
    const spawned = agent("sub", "codex", null, null);
    asker = agent("sub", "codex", spawned, null);
    agent("sub", "codex", spawned, null);
    for (const [lead, name] of [[native, "lead-a"], [spawned, "lead-b"]]) {
      const worktree = join(fixture, "worktrees", name);
      git(repo, "worktree", "add", "-q", "-b", `crazyagent/${name}`, worktree, "main");
      writeFileSync(join(worktree, `${name}.py`), `def ${name.replace("-", "_")}():\n    """Says which lead wrote it."""\n    return "${name}"\n`);
      const gitDir = join(repo, ".git", "worktrees", name);
      store.prepare("INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?,?,?,?,?)").run(lead, project, repo, worktree, gitDir);
      leads.push({ lead, worktree, gitDir, name });
    }
  } finally {
    store.close();
  }
  const sets = leads.map(({ lead, worktree, gitDir, name }) => bridge("record-turn-end", runtime, project, String(lead), name, repo, worktree, gitDir, `Merge the ${name} work`));
  const question = bridge("ask-owner", runtime, project, String(asker), "high", QUESTION);
  return { project, leads: leads.map(({ lead }) => lead), sets, question };
}

/** Gives the fixture engine what the owner's profile already holds, a saved profile, the
 * projects folder and two approved skills, through the desktop bridge's own commands, so the
 * app opens on Home as it does on the owner's profile and workers can be given a real subset
 * of skills. Input: the engine folder and the environment of its children. */
async function onboardOwner(runtime, env) {
  const { child, match } = await start(process.env.TRIAL3_BRIDGE, [join(runtime, "engine.sock")], env, join(fixture, "logs"), "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
  try {
    const bridge = { port: Number(match[1]), token: match[2] };
    assert.equal((await value(bridge, "desktop_connect")).connected, true, "the bridge connects to the fixture engine");
    assert.equal((await value(bridge, "desktop_save_owner_profile", { profile: { name: "Trial Owner" }, generation: 1 })).outcome, "applied", "the fixture owner has a profile");
    assert.equal((await value(bridge, "desktop_set_projects_home", { path: join(fixture, "home", "Projects"), generation: 2 })).outcome, "applied", "the fixture owner has a projects folder");
    for (const name of FIXTURE_SKILLS) {
      const folder = join(fixture, "skills", name);
      mkdirSync(folder, { recursive: true, mode: 0o700 });
      writeFileSync(join(folder, "SKILL.md"), `---\nname: ${name}\ndescription: Teaches ${name}.\n---\n\nSteps for ${name}.\n`);
      const added = await value(bridge, "desktop_add_skill", { scope: { kind: "global" }, folder });
      assert.equal(added.name, name, `the fixture owner approves ${name}: ${JSON.stringify(added).slice(0, 300)}`);
    }
  } finally {
    await stop(child);
  }
}

/** Builds the fixture: its folders, the seeded project, settings, the stand-in harness and a
 * prompt with the project placeholder, then starts the engine, onboards the owner and starts
 * the team seeder. */
async function prepareFixture() {
  try {
    mkdirSync(fixture, { mode: 0o700 });
  } catch {
    throw new Error(`TRIAL3_FIXTURE_ROOT must name a new folder so the run starts from first run: ${fixture}`);
  }
  for (const folder of ["engine", "home/Projects", "tmp", "bin", "worktrees", "logs"]) mkdirSync(join(fixture, folder), { recursive: true, mode: 0o700 });
  mkdirSync(process.env.TRIAL_PROJECT, { mode: 0o700 });
  seedProject(process.env.TRIAL_PROJECT);
  await writeSettings(join(fixture, "engine"));
  if (!existsSync(process.env.TRIAL_PROMPT)) {
    writeFileSync(process.env.TRIAL_PROMPT, "Fixture trial prompt for {{PROJECT}}.\nTwo leads, two sub-agents each, one question for the owner.\n");
  }
  const runtime = join(fixture, "engine");
  const env = { PATH: process.env.PATH, HOME: join(fixture, "home"), TMPDIR: join(fixture, "tmp"), LANG: "en_US.UTF-8" };
  ({ child: engine } = await start(process.env.TRIAL3_ENGINE,
    ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings", "--codex-executable", standInHarness(fixture)],
    env, join(fixture, "logs"), "engine", /^ready$/m));
  await onboardOwner(runtime, env);
  seeding = seedTeam().then(
    (seeded) => writeFileSync(join(fixture, "logs", "seeded.json"), `${JSON.stringify(seeded, null, 2)}\n`),
    (failure) => writeFileSync(join(fixture, "logs", "seeded.json"), `${JSON.stringify({ failed: String(failure.stack ?? failure) }, null, 2)}\n`));
}

/** Checks the live run's preconditions without touching the engine's data: the engine
 * socket exists, the seeded project is a git folder and the trial prompt is there. */
function checkLive() {
  const socket = process.env.CRAZYAGENT_DESKTOP_SOCKET;
  assert(existsSync(socket) && statSync(socket).isSocket(), `no engine socket at ${socket}: start the installed engine service and quit the installed app`);
  assert(existsSync(join(process.env.TRIAL_PROJECT, ".git")), `TRIAL_PROJECT is not a seeded git folder: ${process.env.TRIAL_PROJECT}`);
  assert(existsSync(process.env.TRIAL_PROMPT), `no trial prompt at ${process.env.TRIAL_PROMPT}`);
}

export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("trial3.e2e.mjs", import.meta.url))],
  maxInstances: 1,
  capabilities: [{ browserName: "tauri", "tauri:options": { application: app } }],
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded", autoInstallTauriDriver: false, embeddedPort: port,
    appArgs: [], startTimeout: 60000, captureBackendLogs: Boolean(fixture), backendLogLevel: "info",
    env: appEnv,
  }]],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  outputDir: fixture ? `${fixture}-webdriver` : undefined,
  connectionRetryCount: 0,
  connectionRetryTimeout: 180000,
  waitforTimeout: 15000,
  specFileRetries: 0,
  mochaOpts: { timeout: 14400000, retries: 0, bail: false },
  onPrepare: async function () {
    try {
      mkdirSync(join(evidence, "screenshots"), { recursive: true, mode: 0o700 });
      assert.deepEqual(readdirSync(join(evidence, "screenshots")), [], "the run folder has no screenshots from an earlier run");
      if (mode === "live") checkLive();
      else await prepareFixture();
    } catch (failure) {
      await stop(engine);
      throw new SevereServiceError(`trial 3 cannot start: ${failure?.message ?? failure}`);
    }
  },
  before: async function () {
    await attach(globalThis.browser);
  },
  onComplete: async function () {
    try {
      const stopped = await stopRelaunched(evidence);
      if (stopped) console.log(`trial: ${stopped}`);
    } finally {
      if (seeding) await seeding;
      await stop(engine);
    }
  },
};
