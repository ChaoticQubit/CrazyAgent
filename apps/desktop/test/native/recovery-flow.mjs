import { spawn, spawnSync } from "node:child_process";
import { chmodSync, createWriteStream, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { connect } from "node:net";
import { constants } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "../load-source.mjs";

const { RecoveryBannerView, bannerGroups } = await import("../../src/screens/project/recovery-banner.tsx");

const READY_MS = 60000;
const AUTH_MS = 60000;
const SETUP_MS = 180000;
const TEAM_MS = 90000;
const SCREEN_MS = 30000;
const SETTLE_MS = 2000;
const REAL_TURN_MS = 900000;
const POLL_MS = 250;
const SCREEN_STEPS = 12;
const REPO = "recovery-flow";
const ROOT_PREFIX = "ca-rf-";
const LAUNCHES = "launches.log";
const STAND_IN_SIGN = "recovery-flow stand-in";
const SILENT_TURN_S = 600;
const STOP_SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];
const ACTIVE_AUTH = new Set(["starting", "awaiting_owner", "checking_account", "cancelling"]);
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const LIVE_AGENT = new Set(["starting", "idle", "working"]);
const OPEN_TASK = new Set(["queued", "needs_pool", "running"]);
const RECORD_MISSING = "its session record is missing";
const RAN_PROMPT = "Run this exact shell command once with your shell tool and wait for it to finish: printf 'ran\\n' >> ran.txt && sleep 300\nThen reply with the word DONE.";
const TMUX_SEARCH = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const TUI_SESSION = "recovery-flow";
const TUI_COLUMNS = "200";
const TUI_ROWS = "60";
const BRIDGE_READY = /^ready (\d+) ([0-9a-f]{64})$/m;
const STAND_IN = `#!/bin/sh
marker="$CLAUDE_CONFIG_DIR/.recovery-flow-signed-in"
keychain="$CLAUDE_CONFIG_DIR/Library/Keychains/login.keychain-db"
service="Claude Code-credentials-$(printf %s "$CLAUDE_CONFIG_DIR" | /usr/bin/shasum -a 256 | /usr/bin/cut -c1-8)"
if [ "$1" != "auth" ]; then
  kind=other
  id=-
  for arg in "$@"; do
    case "$kind" in --session-id|--resume) [ "$id" = - ] && id="$arg" ;; esac
    case "$arg" in --session-id|--resume) kind="$arg" ;; --print) [ "$kind" = other ] && kind=--print ;; esac
  done
  printf '%s %s\\n' "$kind" "$id" >> "\${0%/*}/${LAUNCHES}"
  [ "$kind" = other ] || exec /bin/sleep ${SILENT_TURN_S}
  echo "${STAND_IN_SIGN}: this claude answers auth commands and holds a turn open, it never replies" >&2
  exit 64
fi
case "$2" in
  login)
    expires=$(( ( $(/bin/date +%s) + 28800 ) * 1000 ))
    record=$(printf '{"claudeAiOauth":{"accessToken":"recovery-flow-stand-in","refreshToken":"recovery-flow-stand-in","expiresAt":%s,"scopes":["user:inference"]}}' "$expires")
    if [ -x /usr/bin/security ]; then
      hex=$(printf %s "$record" | /usr/bin/od -An -tx1 | /usr/bin/tr -d ' \\n')
      printf 'add-generic-password -U -a recovery-flow -s "%s" -X %s "%s"\\n' "$service" "$hex" "$keychain" | /usr/bin/security -i >/dev/null || exit 1
    else
      (umask 077; printf %s "$record" > "$CLAUDE_CONFIG_DIR/.credentials.json") || exit 1
    fi
    : > "$marker"; exit 0 ;;
  logout)
    if [ -x /usr/bin/security ]; then /usr/bin/security delete-generic-password -s "$service" "$keychain" >/dev/null 2>&1; fi
    rm -f "$marker" "$CLAUDE_CONFIG_DIR/.credentials.json"; exit 0 ;;
  status)
    if [ -f "$marker" ]; then
      printf '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"%s","email":null,"orgId":null,"orgName":null,"subscriptionType":null}\\n' "$CLAUDE_CONFIG_DIR"
      exit 0
    fi
    printf '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty","configDirectory":"%s","email":null,"orgId":null,"orgName":null,"subscriptionType":null}\\n' "$CLAUDE_CONFIG_DIR"
    exit 1 ;;
esac
exit 2
`;
const children = new Set();

/** Reads an absolute path to a built binary from the environment, or stops the run.
 * Input: the variable name and what it names. Output: the existing path. */
function required(name, what) {
  const value = process.env[name];
  if (!value || !isAbsolute(value) || !existsSync(value)) {
    console.error(`BLOCKED: set ${name} to the built ${what} (absolute path)`);
    process.exit(2);
  }
  return value;
}

/** Finds the tmux the engine runs team agents in and the TUI step drives, or stops the run.
 * Input: none. Output: the tmux path. */
function tmuxPath() {
  const found = TMUX_SEARCH.map((folder) => join(folder, "tmux")).find((path) => existsSync(path));
  if (!found) {
    console.error("BLOCKED: tmux is not installed; team agents and the TUI step run in tmux");
    process.exit(2);
  }
  return found;
}

/** Reads the approved real-profile run: a CrazyAgent home whose engine is stopped, with a
 * signed-in Claude account in the pool of a project whose team has a tools-on Claude
 * sub-agent. Paths and ids only, never a secret. Input: none. Output: null without either
 * variable, the problem when the run cannot start, else the home and the project. */
function approvedHome() {
  const home = process.env.CRAZYAGENT_RECOVERY_HOME;
  const project = process.env.CRAZYAGENT_RECOVERY_PROJECT;
  if (!home && !project) return null;
  if (!home || !project) return { problem: "approved_home_incomplete: set CRAZYAGENT_RECOVERY_HOME and CRAZYAGENT_RECOVERY_PROJECT" };
  if (!isAbsolute(home) || !existsSync(join(home, "engine.db"))) return { problem: "approved_home_without_engine_db" };
  return { home, project };
}

/** Runs git in a folder and stops the run when it fails.
 * Input: the folder and the git arguments. Output: git's standard output. */
function git(cwd, ...args) {
  const run = spawnSync("git", args, { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.SMOKE_GIT_HOME, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  if (run.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${run.stderr.trim()}`);
  return run.stdout;
}

/** Builds the small repository the project is set up from: one commit with a README and
 * one source file. Input: the repository folder. Output: none. */
function smallRepo(repo) {
  git(repo, "init", "-q", "-b", "main");
  git(repo, "config", "user.name", "Recovery Flow");
  git(repo, "config", "user.email", "recovery-flow@example.invalid");
  writeFileSync(join(repo, "README.md"), "# Recovery flow\n\nA repository the recovery flow sets up.\n");
  writeFileSync(join(repo, "app.js"), "export const answer = 42;\nexport const question = \"unknown\";\n");
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "first");
}

/** Starts a child with its output kept as evidence and waits for its ready line.
 * Input: binary, arguments, environment, evidence folder, name and ready pattern.
 * Output: the child and the ready line's match. */
async function start(binary, args, env, evidence, name, ready) {
  const child = spawn(binary, args, { env, stdio: ["pipe", "pipe", "pipe"] });
  children.add(child);
  child.once("exit", () => children.delete(child));
  child.stderr.pipe(createWriteStream(join(evidence, `${name}.err`), { flags: "a", mode: 0o600 }));
  const out = createWriteStream(join(evidence, `${name}.out`), { flags: "a", mode: 0o600 });
  const match = await new Promise((done, fail) => {
    let seen = "";
    const timer = setTimeout(() => fail(new Error(`${name} printed no ready line in ${READY_MS} ms; see ${name}.err`)), READY_MS);
    child.stdout.on("data", (chunk) => {
      out.write(String(chunk).replace(/[0-9a-f]{64}/g, "<token>"));
      seen = (seen + chunk).slice(-4096);
      const found = seen.match(ready);
      if (found) {
        clearTimeout(timer);
        done(found);
      }
    });
    child.once("exit", (code, signal) => {
      clearTimeout(timer);
      fail(new Error(`${name} exited before ready: ${code ?? signal}; see ${name}.err`));
    });
  });
  return { child, match };
}

/** Stops a child the way the app quits and waits for it to exit. Input: the child.
 * Output: none. */
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((done) => child.once("exit", done));
  child.stdin.end();
  child.kill("SIGTERM");
  const timer = setTimeout(() => child.kill("SIGKILL"), 20000);
  await exited;
  clearTimeout(timer);
}

/** Kills a child at once, as a crash or a force quit does, and waits for it to exit.
 * Input: the child. Output: none. */
async function crash(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((done) => child.once("exit", done));
  child.kill("SIGKILL");
  await exited;
}

/** Sends one `desktop_*` command to the end-to-end bridge, as the app's invoke does, and keeps
 * it in the trail. Input: the bridge, the trail, the command and arguments. Output: the answer. */
function bridgeCall(bridge, trail, cmd, args = {}) {
  const body = JSON.stringify({ cmd, args });
  const asked = Date.now();
  return new Promise((done) => {
    const call = httpRequest({ host: "127.0.0.1", port: bridge.port, path: "/invoke", method: "POST", agent: false,
      headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body), authorization: `Bearer ${bridge.token}` } }, (reply) => {
      const chunks = [];
      reply.on("data", (chunk) => chunks.push(chunk));
      reply.on("end", () => {
        let answer;
        try {
          answer = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        } catch {
          answer = { failure: { code: "bridge_reply_unreadable" } };
        }
        trail.push({ at: asked, ms: Date.now() - asked, cmd, args, answer });
        done(answer);
      });
    });
    call.on("error", (error) => {
      const answer = { failure: { code: "bridge_unreachable", message: error.message } };
      trail.push({ at: asked, ms: Date.now() - asked, cmd, args, answer });
      done(answer);
    });
    call.end(body);
  });
}

/** Names why a bridge answer has no value. Input: the command and answer. Output: a code. */
function failureCode(cmd, answer) {
  if (answer.failure) return answer.failure.code ?? "failure_without_code";
  if ("unknown" in answer) return `unrouted:${cmd}`;
  if ("invalid" in answer) return "invalid_args";
  return "no_value";
}

/** Thrown when a bridge call, receipt or check stops a step; carries the engine code. */
class Refused extends Error {
  constructor(code, detail) {
    super(`${code}${detail ? `: ${detail}` : ""}`);
    this.code = code;
    this.detail = detail;
  }
}

/** Thrown when a step cannot run here without something the flow may not fake; carries why. */
class Blocked extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

/** Reads rows from an engine database without writing. Input: the database path, the query
 * and its parameters. Output: the rows, none without a database. */
function engineRows(database, sql, ...params) {
  if (!existsSync(database)) return [];
  const db = new DatabaseSync(database, { readOnly: true });
  try {
    return db.prepare(sql).all(...params);
  } finally {
    db.close();
  }
}

/** Tells whether any process of a process group is still alive. Input: the group id.
 * Output: true while the group has a member, false once it is gone. */
function groupAlive(group) {
  try {
    process.kill(-group, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}

/** Runs tmux against one server. Input: the tmux binary, the server socket, the environment
 * and the arguments. Output: tmux's standard output, or "" when it failed. */
function tmux(binary, socket, env, ...args) {
  const run = spawnSync(binary, ["-S", socket, ...args], { encoding: "utf8", env });
  return run.status === 0 ? run.stdout : "";
}

/** Reads every harness turn the stand-in claude held open, in order: its kind (--session-id,
 * --resume or --print) and the session it names. Calls that start no turn are left out.
 * Input: the stand-in's folder. Output: the starts. */
function launches(bin) {
  const path = join(bin, LAUNCHES);
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8").split("\n").filter(Boolean).map((line) => {
    const [kind, id] = line.split(" ");
    return { kind, id };
  }).filter((item) => item.kind !== "other");
}

/** Counts the lines a file holds, none when it is missing. Input: the path. Output: the count. */
function lines(path) {
  return existsSync(path) ? readFileSync(path, "utf8").split("\n").filter(Boolean).length : 0;
}

/** Escapes text the way React writes it into markup. Input: the text. Output: the markup text. */
function escaped(text) {
  return text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("\"", "&quot;").replaceAll("'", "&#x27;");
}

/** Joins a terminal screen into single-spaced words, so an item the TUI wrapped over two rows
 * still matches. Input: the captured screen or a line. Output: the words. */
function words(text) {
  return text.split("\n").map((line) => line.trim()).filter(Boolean).join(" ").replace(/\s+/g, " ");
}

/** Tells whether a Unix socket has a live listener. Input: the socket path. Output: true when
 * a connection opens. */
function socketLive(path) {
  if (!existsSync(path)) return Promise.resolve(false);
  return new Promise((done) => {
    const socket = connect(path);
    socket.once("connect", () => {
      socket.destroy();
      done(true);
    });
    socket.once("error", () => done(false));
  });
}

/** Removes the temp root and ends the tmux servers when the process ends: a normal exit, a
 * thrown failure, or SIGINT, SIGTERM or SIGHUP once the engines and bridges have stopped.
 * Input: the root, whether SMOKE_EVIDENCE keeps it, and the tmux servers as binary and
 * socket pairs. Output: none. */
function claimRoot(root, keep, servers) {
  process.once("exit", () => {
    for (const child of children) child.kill("SIGTERM");
    for (const [binary, socket] of servers) if (existsSync(socket)) spawnSync(binary, ["-S", socket, "kill-server"], { stdio: "ignore" });
    if (!keep) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  });
  for (const signal of STOP_SIGNALS) {
    process.once(signal, async () => {
      console.log(`\n${signal}: stopping the engines and bridges`);
      await Promise.all([...children].map(stop));
      process.exit(128 + constants.signals[signal]);
    });
  }
}

/** Kills the app mid-turn and checks what comes back: two working agents and a running master
 * turn, then SIGKILL of the engine and the desktop bridge with one agent's pane gone and one
 * left alive. After the restart the engine must record only the run without a live pane, show
 * it right in the desktop and TUI banners, start nothing twice and, with an approved profile,
 * resume the agent in its own session. Input: environment paths. Output: exit 1 on any FAIL,
 * 3 when only BLOCKED steps remain. */
async function main() {
  const engineBinary = required("CRAZYAGENT_TEST_NATIVE_AUTH_HOST", "crazyagent binary (the engine, its native-auth host and the TUI)");
  const bridgeBinary = required("SMOKE_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  const tmuxBinary = tmuxPath();
  const approved = approvedHome();
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(realpathSync("/tmp"), ROOT_PREFIX)));
  const evidence = process.env.SMOKE_EVIDENCE && isAbsolute(process.env.SMOKE_EVIDENCE) ? process.env.SMOKE_EVIDENCE : join(root, "evidence");
  const keep = Boolean(process.env.SMOKE_EVIDENCE) && evidence === join(root, "evidence");
  const crazyHome = join(root, "h");
  const engineTmux = join(crazyHome, "terminal", "tmux.sock");
  const tuiTmux = join(root, "tui.sock");
  claimRoot(root, keep, [[tmuxBinary, engineTmux], [tmuxBinary, tuiTmux]]);
  console.log(`temp root ${root} (${keep ? "kept for SMOKE_EVIDENCE" : "removed on exit; SMOKE_EVIDENCE=1 keeps it"})`);
  const userHome = join(root, "u");
  const bin = join(userHome, ".local", "bin");
  const repo = join(root, "repos", REPO);
  const projects = join(root, "projects");
  for (const folder of [crazyHome, userHome, bin, repo, projects, evidence]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  process.env.SMOKE_GIT_HOME = userHome;
  writeFileSync(join(bin, "claude"), STAND_IN, { mode: 0o700 });
  chmodSync(join(bin, "claude"), 0o700);
  smallRepo(repo);

  const env = { PATH: `${bin}:/usr/bin:/bin`, HOME: userHome, CRAZYAGENT_HOME: crazyHome, TMPDIR: root, LANG: "en_US.UTF-8" };
  const tuiEnv = { ...env, TERM: "xterm-256color" };
  const database = join(crazyHome, "engine.db");
  const trail = [];
  const results = [];
  const state = { generation: 0, project: null, name: null, a: null, b: null, taskA: null, groupB: null, before: null, crashedMs: null, failed: null };
  let engine;
  let bridgeProcess;
  let bridge;
  const call = (cmd, args, via = bridge) => bridgeCall(via, trail, cmd, args);
  const value = async (cmd, args, via = bridge) => {
    let answer = await call(cmd, args, via);
    for (let attempt = 1; answer.failure?.code === "busy" && attempt < 8; attempt += 1) {
      await pause(250 * attempt);
      answer = await call(cmd, args, via);
    }
    if (!("value" in answer)) throw new Refused(failureCode(cmd, answer), answer.failure?.message);
    return answer.value;
  };
  const generation = () => (state.generation += 1);
  const applied = (cmd, receipt) => {
    if (receipt?.outcome === "applied" && !receipt.pending) return receipt;
    throw new Refused(receipt?.rejection ?? (receipt?.pending ? "pending" : receipt?.outcome ?? "no_receipt"), cmd);
  };
  const connected = async (via) => {
    for (const deadline = Date.now() + READY_MS; ; await pause(POLL_MS)) {
      if ((await call("desktop_connect", {}, via)).value?.connected === true) return;
      if (Date.now() > deadline) throw new Refused("bridge_connect_failed");
    }
  };
  const startApp = async () => {
    ({ child: engine } = await start(engineBinary, ["engine"], env, evidence, "engine", /^ready$/m));
    const started = await start(bridgeBinary, [join(crazyHome, "engine.sock")], env, evidence, "bridge", BRIDGE_READY);
    bridgeProcess = started.child;
    bridge = { port: Number(started.match[1]), token: started.match[2] };
    await connected(bridge);
  };
  const engineErr = join(evidence, "engine.err");
  const errText = () => (existsSync(engineErr) ? readFileSync(engineErr, "utf8") : "");
  const step = async (id, title, run) => {
    const mark = errText().length;
    let row;
    try {
      row = { id, title, result: "PASS", code: (await run()) ?? "ok" };
    } catch (failure) {
      const result = failure instanceof Blocked ? "BLOCKED" : "FAIL";
      row = { id, title, result, code: failure instanceof Refused || failure instanceof Blocked ? failure.code : `script:${failure.message}` };
      if (failure instanceof Refused && failure.detail) row.code += ` (${failure.detail})`;
    }
    await pause(POLL_MS);
    row.causes = [...new Set(errText().slice(mark).match(/cause=[^\n]*/g) ?? [])].join("; ") || "-";
    results.push(row);
    console.log(`${row.result} ${id} ${title} [${row.code}] engine stderr: ${row.causes}`);
  };
  const needs = (what, from) => {
    if (what === null || what === undefined) throw new Refused(`blocked_by_${from}`);
    return what;
  };

  const connection = async (id) => value("desktop_inspect_model_access", { collection: "connections", id });
  const signIn = async (name) => {
    const account = await value("desktop_add_account", { draft: { provider: "claude", method: "subscription", name }, key: null });
    await value("desktop_start_native_auth", { ticket: (await connection(account.id)).ticket, method: "claude_official" });
    for (const deadline = Date.now() + AUTH_MS; ; await pause(POLL_MS)) {
      const view = await value("desktop_read_native_auth", { ticket: (await connection(account.id)).ticket });
      if (view.phase === "authenticated_observed") return account;
      if (!ACTIVE_AUTH.has(view.phase)) throw new Refused(view.code ?? `native_auth_${view.phase}`, name);
      if (Date.now() > deadline) throw new Refused(`native_auth_still_${view.phase}`, name);
    }
  };
  const saveSetup = async (projectId, change) => {
    const stored = await value("desktop_project_setup", { id: projectId });
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team: stored.team, documents: [], ...change };
    applied("update_project_setup", await value("desktop_update_project_setup", { id: projectId, setup, name: null, revision: stored.revision, generation: generation() }));
  };
  const assignPool = async (projectId, poolId) => {
    const opened = await value("desktop_open_project", { id: projectId });
    if (!opened.fresh || opened.project_id !== projectId) throw new Refused("invalid_project");
    const basis = await value("desktop_inspect_model_access", { collection: "pools", id: poolId });
    const now = basis.record.pool;
    const pool = { ...now, projects: now.projects.includes(projectId) ? now.projects : [...now.projects, projectId], roles: now.roles.includes("project_leader") ? now.roles : [...now.roles, "project_leader"] };
    if (JSON.stringify(pool) === JSON.stringify(now)) return;
    applied("replace_pool", await value("desktop_edit_model_access", { ticket: basis.ticket, edit: { kind: "replace_pool", pool }, generation: generation() }));
  };
  const teamStatus = async (project = needs(state.project, "1"), via = bridge) => value("desktop_team_status", { id: project }, via);
  const teamNote = (status) => status.messages.filter((message) => message.author === "engine").at(-1)?.text ?? "no engine note";
  const delegate = async (prompt) => {
    const project = needs(state.project, "1");
    const before = new Set((await teamStatus()).tasks.map((task) => task.id));
    applied("team_delegate", await value("desktop_team_delegate", { id: project, harness: "claude-code", prompt, generation: generation() }));
    for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
      const status = await teamStatus();
      const task = status.tasks.find((item) => !before.has(item.id));
      const agent = status.agents.find((item) => item.id === task?.agent) ?? null;
      if (task && agent) return { task, agent };
      if (task && !["queued", "needs_pool"].includes(task.status)) throw new Refused(`team_task_${task.status}_without_agent`, task.problem ?? teamNote(status));
      if (Date.now() > deadline) throw new Refused(task ? `team_task_still_${task.status}` : "team_task_missing", teamNote(status));
    }
  };
  const working = async (id) => {
    for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
      const member = (await teamStatus()).agents.find((item) => item.id === id);
      if (member?.status === "working") return member;
      if (Date.now() > deadline) throw new Refused(`team_agent_${member?.status ?? "missing"}`, member?.problem ?? `agent ${id}`);
    }
  };
  const recoveryRun = async (agent, done, project = needs(state.project, "1"), via = bridge, limit = TEAM_MS) => {
    let run;
    for (const deadline = Date.now() + limit; ; await pause(POLL_MS * 4)) {
      const banner = await value("desktop_recovery_banner", { projectId: project }, via);
      run = banner.runs.find((item) => item.agent === agent);
      if (run && done(run)) return { run, banner };
      if (Date.now() > deadline) throw new Refused(`recovery_run_${run?.state ?? "missing"}`, run?.reason ?? `agent ${agent}`);
    }
  };
  const tuiScreen = () => tmux(tmuxBinary, tuiTmux, tuiEnv, "capture-pane", "-p", "-t", TUI_SESSION);
  const tuiKey = (key) => tmux(tmuxBinary, tuiTmux, tuiEnv, "send-keys", "-t", TUI_SESSION, key);
  const tuiUntil = async (what, check) => {
    for (const deadline = Date.now() + SCREEN_MS; ; await pause(POLL_MS)) {
      const screen = tuiScreen();
      if (check(screen)) return screen;
      if (Date.now() > deadline) throw new Refused(`tui_never_${what}`, words(screen).slice(-300));
    }
  };

  const realResume = async ({ home, project }) => {
    const socket = join(home, "engine.sock");
    if (await socketLive(socket)) throw new Blocked("approved_engine_running: stop that engine first; this step kills and restarts its own");
    const realEnv = { PATH: process.env.PATH ?? "/usr/bin:/bin", HOME: process.env.HOME ?? userHome, CRAZYAGENT_HOME: home, LANG: "en_US.UTF-8" };
    let realEngine;
    let realBridge;
    let via;
    const up = async () => {
      ({ child: realEngine } = await start(engineBinary, ["engine"], realEnv, evidence, "real-engine", /^ready$/m));
      const started = await start(bridgeBinary, [socket], realEnv, evidence, "real-bridge", BRIDGE_READY);
      realBridge = started.child;
      via = { port: Number(started.match[1]), token: started.match[2] };
      await connected(via);
    };
    try {
      await up();
      const setup = await value("desktop_project_setup", { id: project }, via);
      if (!setup) throw new Blocked("approved_project_missing");
      const status = await teamStatus(project, via);
      if (!status.harnesses.find((item) => item.role === "sub" && item.harness === "claude-code")?.tools) throw new Blocked("approved_project_without_a_tools_on_claude_sub_agent");
      const known = new Set(status.tasks.map((task) => task.id));
      applied("team_delegate", await value("desktop_team_delegate", { id: project, harness: "claude-code", prompt: RAN_PROMPT, generation: generation() }, via));
      let task;
      let agent;
      let counter = null;
      for (const deadline = Date.now() + REAL_TURN_MS; ; await pause(POLL_MS * 4)) {
        const now = await teamStatus(project, via);
        task = now.tasks.find((item) => !known.has(item.id));
        agent = now.agents.find((item) => item.id === task?.agent);
        counter = agent?.worktree ? join(setup.folder, agent.worktree, "ran.txt") : null;
        if (counter && lines(counter) >= 1) break;
        if (task && !OPEN_TASK.has(task.status)) throw new Refused(`real:task_${task.status}_before_its_command`, task.problem ?? teamNote(now));
        if (Date.now() > deadline) throw new Refused("real:command_never_ran", task ? `task ${task.status}` : "no task");
      }
      const group = agent.process_group;
      await crash(realEngine);
      await crash(realBridge);
      tmux(tmuxBinary, join(home, "terminal", "tmux.sock"), realEnv, "kill-window", "-t", `team-${agent.id}`);
      for (const deadline = Date.now() + TEAM_MS; group !== null && groupAlive(group); await pause(POLL_MS)) {
        if (Date.now() > deadline) throw new Refused("real:agent_pane_not_closed", String(group));
      }
      await up();
      const { run, banner } = await recoveryRun(agent.id, (item) => item.state === "resumed" || item.state === "failed", project, via, REAL_TURN_MS);
      if (run.state === "failed") throw new Refused("real:resume_failed", run.reason ?? "no reason");
      if (!bannerGroups(banner).resumed.some((item) => item.agent === agent.id)) throw new Refused("real:desktop_banner_without_resumed_agent");
      for (const deadline = Date.now() + REAL_TURN_MS; ; await pause(POLL_MS * 4)) {
        const now = (await teamStatus(project, via)).tasks.find((item) => item.id === task.id);
        if (!now || !OPEN_TASK.has(now.status)) break;
        if (Date.now() > deadline) throw new Refused(`real:resumed_task_still_${now.status}`);
      }
      const ran = lines(counter);
      if (ran !== 1) throw new Refused("real:command_ran_twice", `ran.txt holds ${ran} lines`);
      return `${run.label} resumed its own session after the kill; its command ran once; desktop banner lists it as resumed`;
    } finally {
      await stop(realBridge);
      await stop(realEngine);
    }
  };

  try {
    await startApp();
    applied("save_owner_profile", await value("desktop_save_owner_profile", { profile: { name: "Recovery Flow" }, generation: generation() }));
    applied("set_projects_home", await value("desktop_set_projects_home", { path: projects, generation: generation() }));

    await step("1", "a project with a pool of two signed-in Claude accounts and a team of three tools-on sub-agents", async () => {
      const accounts = [await signIn("Recovery Claude A"), await signIn("Recovery Claude B")];
      const page = await value("desktop_model_access_page", { collection: "pools", next: false });
      const pool = "pool-recoveryflow0000000";
      applied("create_pool", await value("desktop_edit_model_access", { ticket: page.ticket, edit: { kind: "create_pool", pool: { id: pool, name: "Recovery pool", members: accounts.map((account) => account.id), projects: [], roles: [], fallback_allowed: false } }, generation: generation() }));
      const receipt = applied("start_onboarding", await value("desktop_start_onboarding", { path: repo, pool, generation: generation() }));
      state.project = receipt.project_id;
      for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
        const view = await value("desktop_onboarding", { id: state.project });
        if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
        if (Date.now() > deadline) throw new Refused("onboarding_still_running");
      }
      await assignPool(state.project, pool);
      const team = { master: "claude-code", master_tools: false, sub_harnesses: [{ harness: "claude-code", agents: 3, tools: true }], min_agents: 1, max_agents: 4 };
      await saveSetup(state.project, { about: "A repository the recovery flow sets up.", team });
      const harness = (await teamStatus()).harnesses.find((item) => item.role === "sub" && item.harness === "claude-code");
      if (!harness?.tools) throw new Refused("team_status_without_sub_tools", JSON.stringify(harness ?? null));
      state.name = (await value("desktop_project_setup", { id: state.project })).name;
      return `project ${state.project} "${state.name}"`;
    });

    await step("2", "two agents work on their tasks and a master turn runs", async () => {
      const first = await delegate("List the files in this repository.");
      const second = await delegate("Read the README of this repository.");
      await working(first.agent.id);
      const b = await working(second.agent.id);
      state.a = first.agent.id;
      state.b = second.agent.id;
      state.taskA = first.task.id;
      state.groupB = b.process_group;
      const opened = await value("desktop_open_project", { id: needs(state.project, "1") });
      let master = await value("desktop_master_state", { ticket: opened.ticket });
      if (!master.session || master.session.phase !== "configured") {
        applied("open_master", await value("desktop_open_master", { ticket: opened.ticket, generation: generation() }));
        master = await value("desktop_master_state", { ticket: opened.ticket });
        if (!master.session) throw new Refused("master_session_missing");
      }
      const sessionId = master.session.session_id;
      const receipt = applied("submit_master", await value("desktop_submit_master", { ticket: opened.ticket, sessionId, text: "Summarize this repository in one paragraph.", generation: generation() }));
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS)) {
        const turn = (await value("desktop_master_turns", { ticket: opened.ticket, sessionId, cursor: null })).page.turns.find((item) => item.id === receipt.operation_id);
        if (!turn) throw new Refused("master_turn_missing");
        if (turn.phase === "running") break;
        if (!ACTIVE_TURN.has(turn.phase)) throw new Refused(`master_turn_${turn.phase}`, turn.reason ?? "-");
        if (Date.now() > deadline) throw new Refused(`master_turn_still_${turn.phase}`);
      }
      return `agents ${state.a} and ${state.b} working, master turn running; ${launches(bin).length} harness starts`;
    });

    await step("3", "kill the app mid-turn: SIGKILL the engine and the desktop bridge; agent A's pane goes with it, agent B's pane lives on", async () => {
      const a = needs(state.a, "2");
      const groupA = (await teamStatus()).agents.find((item) => item.id === a)?.process_group ?? null;
      applied("team_delegate", await value("desktop_team_delegate", { id: needs(state.project, "1"), harness: "claude-code", prompt: "Count the lines of app.js.", generation: generation() }));
      await crash(engine);
      await crash(bridgeProcess);
      tmux(tmuxBinary, engineTmux, env, "kill-window", "-t", `team-${a}`);
      for (const deadline = Date.now() + TEAM_MS; groupA !== null && groupAlive(groupA); await pause(POLL_MS)) {
        if (Date.now() > deadline) throw new Refused("agent_pane_not_closed", `team-${a}`);
      }
      if (state.groupB !== null && !groupAlive(state.groupB)) throw new Refused("agent_b_died_with_the_app");
      await pause(SETTLE_MS);
      state.before = launches(bin);
      state.crashedMs = Date.now();
      return `engine and bridge killed; team-${a} closed, team-${state.b} running; a third task delegated just before the kill; ${state.before.length} harness starts so far`;
    });

    await step("4", "the engine starts again and records only the run without a live pane (REC-1)", async () => {
      const a = needs(state.a, "2");
      const b = needs(state.b, "2");
      needs(state.before, "3");
      await startApp();
      let rows = [];
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
        rows = engineRows(database, "SELECT agent, kind, state, reason FROM recovery_runs WHERE project = ?", state.project);
        if (rows.some((row) => Number(row.agent) === a && row.state !== "stopped")) break;
        if (Date.now() > deadline) throw new Refused("interrupted_run_not_recorded", rows.map((row) => `${row.agent}:${row.state}`).join(",") || `agent ${a}`);
      }
      const forA = rows.filter((row) => Number(row.agent) === a);
      if (forA.length !== 1) throw new Refused("interrupted_run_recorded_twice", `${forA.length} rows for agent ${a}`);
      if (rows.some((row) => Number(row.agent) === b)) throw new Refused("adoptable_agent_recorded_interrupted", `agent ${b}`);
      return `recorded ${rows.map((row) => `${row.kind} ${row.agent} ${row.state}`).join(", ")}`;
    });

    await step("5", "the agent whose pane lived on is adopted, not started again", async () => {
      const b = needs(state.b, "2");
      const member = (await teamStatus()).agents.find((item) => item.id === b);
      if (!member || !LIVE_AGENT.has(member.status)) throw new Refused(`adopted_agent_${member?.status ?? "missing"}`, member?.problem ?? "-");
      if (member.process_group !== state.groupB) throw new Refused("adopted_agent_started_again", `group ${state.groupB} became ${member.process_group}`);
      if (state.groupB !== null && !groupAlive(state.groupB)) throw new Refused("adopted_agent_group_gone");
      return `agent ${b} ${member.status} in its first process group`;
    });

    await step("6", "a resume that cannot happen is Failed with its reason and its task never starts fresh (REC-2)", async () => {
      const a = needs(state.a, "2");
      const { run } = await recoveryRun(a, (item) => item.state === "failed" || item.state === "resumed");
      if (run.state === "resumed") throw new Refused("stand_in_session_resumed", "the stand-in keeps no session record");
      if (run.reason !== RECORD_MISSING) throw new Refused("failed_resume_without_its_reason", run.reason ?? "none");
      const status = await teamStatus();
      const task = status.tasks.find((item) => item.id === state.taskA);
      if (task?.agent !== undefined && task.agent !== null && task.agent !== a) throw new Refused("interrupted_task_started_fresh", `agent ${task.agent}`);
      const resumed = launches(bin).filter((item) => item.kind === "--resume");
      if (resumed.length) throw new Refused("resumed_without_a_record", resumed.map((item) => item.id).join(","));
      state.failed = run;
      return `${run.label} (${run.harness}) failed: ${run.reason}; its task ${task?.status ?? "is gone"}, no fresh start`;
    });

    await step("7", "the desktop banner lists the failed resume with Retry and leaves out the adopted agent (REC-3)", async () => {
      const failed = needs(state.failed, "6");
      const banner = await value("desktop_recovery_banner", { projectId: needs(state.project, "1") });
      const groups = bannerGroups(banner);
      if (!groups.failed.some((run) => run.agent === failed.agent)) throw new Refused("desktop_banner_without_the_failed_resume", banner.runs.map((run) => `${run.agent}:${run.state}`).join(","));
      if ([...groups.interrupted, ...groups.resumed].some((run) => run.agent === failed.agent)) throw new Refused("desktop_banner_lists_the_run_twice");
      if (banner.runs.some((run) => run.agent === state.b)) throw new Refused("desktop_banner_lists_the_adopted_agent");
      const markup = renderToStaticMarkup(createElement(RecoveryBannerView, { banner, busy: null, note: "", onRetry: () => {}, onDismiss: () => {} }));
      writeFileSync(join(evidence, "desktop-banner.html"), markup);
      const line = escaped(`${failed.label} (${failed.harness}) didn't resume. ${failed.reason}`);
      if (!markup.includes(line)) throw new Refused("desktop_banner_line_differs", line);
      if (!markup.includes(">Retry<")) throw new Refused("desktop_banner_without_retry");
      return `failed ${groups.failed.length}, interrupted ${groups.interrupted.length}, resumed ${groups.resumed.length}, notices ${groups.notices.length}`;
    });

    await step("8", "the TUI project page shows the same banner with Ctrl-R Retry (REC-3)", async () => {
      const failed = needs(state.failed, "6");
      const name = needs(state.name, "1");
      const socket = join(crazyHome, "engine.sock");
      if ([engineBinary, socket].some((path) => path.includes("'"))) throw new Refused("path_with_quote");
      const item = words(`${failed.label} (${failed.harness}): ${failed.reason}`);
      tmux(tmuxBinary, tuiTmux, tuiEnv, "-f", "/dev/null", "new-session", "-d", "-s", TUI_SESSION, "-x", TUI_COLUMNS, "-y", TUI_ROWS, `'${engineBinary}' tui '${socket}'`);
      try {
        const home = (text) => text.includes("New project") && text.includes(name);
        for (let turn = 0; ; turn += 1) {
          const screen = await tuiUntil("showed_home", (text) => home(text) || text.includes("Welcome to CrazyAgent") || text.includes("c Continue without"));
          if (home(screen)) break;
          if (turn >= SCREEN_STEPS) throw new Refused("tui_never_reached_home", words(screen).slice(-300));
          tuiKey("c");
          await tuiUntil("moved_on", (text) => text !== screen);
        }
        for (let turn = 0; !tuiScreen().includes("Open"); turn += 1) {
          if (turn >= SCREEN_STEPS) throw new Refused("tui_home_never_offered_open");
          tuiKey("Tab");
          await pause(POLL_MS);
        }
        tuiKey("Enter");
        const screen = await tuiUntil("showed_the_failed_resume", (text) => words(text).includes(item));
        writeFileSync(join(evidence, "tui-banner.txt"), screen);
        if (!words(screen).includes("Ctrl-R Retry")) throw new Refused("tui_banner_without_retry");
        if (!/\bFailed\b/.test(screen)) throw new Refused("tui_banner_without_failed_group");
        return `TUI shows "${item}" under Failed with Ctrl-R Retry`;
      } finally {
        tmux(tmuxBinary, tuiTmux, tuiEnv, "kill-server");
      }
    });

    await step("9", "Retry tries the resume again; it fails the same way and still starts nothing", async () => {
      const failed = needs(state.failed, "6");
      const before = launches(bin).length;
      await value("desktop_retry_recovery", { agent: failed.agent });
      const { run } = await recoveryRun(failed.agent, (item) => item.state === "failed" || item.state === "resumed");
      if (run.state !== "failed" || run.reason !== RECORD_MISSING) throw new Refused(`retried_resume_${run.state}`, run.reason ?? "none");
      const after = launches(bin);
      if (after.length !== before) throw new Refused("retry_started_a_harness", after.slice(before).map((item) => `${item.kind} ${item.id}`).join(","));
      return `retried; ${run.label} failed again: ${run.reason}`;
    });

    await step("10", "nothing runs twice: no session starts again, no ledger action is done twice or left pending, one worktree per agent (UO-2a, UO-2b)", async () => {
      const earlier = needs(state.before, "3");
      const all = launches(bin);
      const fresh = all.slice(earlier.length);
      const seen = new Set(earlier.filter((item) => item.id !== "-").map((item) => item.id));
      const again = fresh.filter((item) => seen.has(item.id));
      if (again.length) throw new Refused("session_started_again", again.map((item) => `${item.kind} ${item.id}`).join(","));
      if (all.some((item) => item.kind === "--resume")) throw new Refused("stand_in_session_resumed");
      if (fresh.length > 1) throw new Refused("more_than_the_waiting_task_started", fresh.map((item) => `${item.kind} ${item.id}`).join(","));
      const doneTwice = engineRows(database, "SELECT kind, action_key, COUNT(*) AS n FROM ledger_actions WHERE outcome = 'done' GROUP BY kind, action_key HAVING COUNT(*) > 1");
      if (doneTwice.length) throw new Refused("ledger_action_done_twice", doneTwice.map((row) => `${row.kind} ${row.action_key} x${row.n}`).join(","));
      let pending = [];
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 4)) {
        pending = engineRows(database, "SELECT kind, action_key FROM ledger_actions WHERE outcome IS NULL AND started_ms <= ?", state.crashedMs);
        if (!pending.length) break;
        if (Date.now() > deadline) throw new Refused("ledger_action_left_pending", pending.map((row) => `${row.kind} ${row.action_key}`).join(","));
      }
      const total = engineRows(database, "SELECT COUNT(*) AS n FROM ledger_actions")[0]?.n ?? 0;
      const agents = engineRows(database, "SELECT id FROM team_agents WHERE project = ?", state.project).map((row) => Number(row.id));
      const worktrees = git(repo, "worktree", "list", "--porcelain").split("\n").filter((line) => line.startsWith("worktree ")).map((line) => line.slice("worktree ".length));
      const doubled = agents.filter((id) => worktrees.filter((path) => path.endsWith(`/agent-${id}`)).length > 1);
      if (doubled.length) throw new Refused("agent_worktree_added_twice", doubled.join(","));
      return `${fresh.length} start(s) after the restart, ${all.length} in all, none of a session already started; ledger ${total} actions, none done twice or left pending; ${agents.length} agents, one worktree each at most`;
    });

    await step("11", "with an approved profile: an interrupted agent resumes its own session and its command ran once (REC-2 live)", async () => {
      if (!approved) throw new Blocked("needs_approved_profile: set CRAZYAGENT_RECOVERY_HOME and CRAZYAGENT_RECOVERY_PROJECT to an approved CrazyAgent home whose engine is stopped");
      if (approved.problem) throw new Blocked(approved.problem);
      return realResume(approved);
    });
  } catch (failure) {
    results.push({ id: "setup", title: "start the engine and bridge", result: "FAIL", code: failure instanceof Refused ? failure.code : `script:${failure.message}`, causes: "-" });
    console.log(`FAIL setup [${results.at(-1).code}]`);
  } finally {
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(trail, null, 1).replace(/[0-9a-f]{64}/g, (hex) => hex.slice(0, 8)));
    writeFileSync(join(evidence, LAUNCHES), launches(bin).map((item) => `${item.kind} ${item.id}`).join("\n"));
    await stop(bridgeProcess);
    await stop(engine);
  }
  console.log("\nstep | result | engine code | engine stderr | what");
  for (const row of results) console.log(`${row.id} | ${row.result} | ${row.code} | ${row.causes} | ${row.title}`);
  const failed = results.filter((row) => row.result === "FAIL").length;
  const blocked = results.filter((row) => row.result === "BLOCKED").length;
  const where = keep ? `evidence and temp root kept at ${root}` : process.env.SMOKE_EVIDENCE ? `evidence ${evidence}, temp root removed` : "evidence and temp root removed (SMOKE_EVIDENCE=1 keeps them)";
  console.log(`\n${results.length - failed - blocked} PASS, ${failed} FAIL, ${blocked} BLOCKED; ${where}`);
  process.exitCode = failed > 0 ? 1 : blocked > 0 ? 3 : 0;
}

if (process.argv.length > 2) {
  console.error("usage: node apps/desktop/test/native/recovery-flow.mjs");
  process.exitCode = 2;
} else await main();
