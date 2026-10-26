import { spawn, spawnSync } from "node:child_process";
import { chmodSync, createWriteStream, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";

const READY_MS = 60000;
const AUTH_MS = 60000;
const TURN_MS = 90000;
const SETUP_MS = 180000;
const TEAM_MS = 90000;
const POLL_MS = 250;
const MERGE_BASE_KEYS = 50;
const LINKED_WORKTREES = 41;
const REPO = "owner-shaped";
const ACTIVE_AUTH = new Set(["starting", "awaiting_owner", "checking_account", "cancelling"]);
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const NOT_ADMITTED = new Set(["readiness_unknown", "readiness_stale", "invalid_authentication", "connection_changed", "scope_revoked", "repository_missing", "unknown"]);
const TMUX_SEARCH = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const SILENT_MARK = "owner-flow-silent";
const SILENT_TURN_S = 300;
const STAND_IN = `#!/bin/sh
marker="$CLAUDE_CONFIG_DIR/.owner-flow-signed-in"
if [ "$1" != "auth" ]; then
  case " $* " in *" --session-id "*|*" --print "*) [ -f "\${0%/*}/${SILENT_MARK}" ] && exec /bin/sleep ${SILENT_TURN_S} ;; esac
  echo "owner-flow stand-in: this claude only answers auth commands, it cannot run a turn" >&2
  exit 64
fi
case "$2" in
  login) : > "$marker"; exit 0 ;;
  logout) rm -f "$marker"; exit 0 ;;
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

/** Runs git in a folder and stops the run when it fails.
 * Input: the folder and the git arguments. Output: git's standard output. */
function git(cwd, ...args) {
  const run = spawnSync("git", args, { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.SMOKE_GIT_HOME, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  if (run.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${run.stderr.trim()}`);
  return run.stdout;
}

/** Builds a repository shaped like the owner's: 50 vscode merge-base branch keys, per-worktree
 * config on, 41 linked worktrees (one with a codex config.worktree) and dirty files.
 * Input: the repository folder and the folder for the linked worktrees. Output: none. */
function ownerRepo(repo, linked) {
  git(repo, "init", "-q", "-b", "main");
  git(repo, "config", "user.name", "Owner Flow");
  git(repo, "config", "user.email", "owner-flow@example.invalid");
  writeFileSync(join(repo, "README.md"), "# Owner shaped\n\nA repository shaped like the owner's.\n");
  writeFileSync(join(repo, "app.js"), "export const answer = 42;\n");
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "first");
  for (let index = 0; index < MERGE_BASE_KEYS; index += 1) git(repo, "config", `branch.feature-${index}.vscode-merge-base`, "origin/main");
  git(repo, "config", "extensions.worktreeconfig", "true");
  for (let index = 0; index < LINKED_WORKTREES; index += 1) git(repo, "worktree", "add", "-q", "-b", `wt-${index}`, join(linked, `wt-${index}`));
  git(join(linked, "wt-0"), "config", "--worktree", "codex.localEnvironmentConfigPath", "__none__");
  writeFileSync(join(repo, "app.js"), "export const answer = 43;\n");
  writeFileSync(join(repo, "notes.txt"), "work in progress\n");
}

/** Starts a child with its output kept as evidence and waits for its ready line.
 * Input: binary, arguments, environment, evidence folder, name and ready pattern.
 * Output: the child and the ready line's match. */
async function start(binary, args, env, evidence, name, ready) {
  const child = spawn(binary, args, { env, stdio: ["pipe", "pipe", "pipe"] });
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

/** Stops a child and waits for it to exit. Input: the child. Output: none. */
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((done) => child.once("exit", done));
  child.stdin.end();
  child.kill("SIGTERM");
  const timer = setTimeout(() => child.kill("SIGKILL"), 20000);
  await exited;
  clearTimeout(timer);
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

/** Reads the code the engine itself recorded for one owner operation, so a code the bridge
 * collapses (owner_rejected) still shows what the engine said. Input: the engine database
 * path and the operation id. Output: the recorded code, or null when there is none. */
function recordedCode(database, operationId) {
  if (!existsSync(database)) return null;
  const db = new DatabaseSync(database, { readOnly: true });
  try {
    const row = db.prepare("SELECT result FROM owner_operations WHERE operation_id = ?").get(operationId);
    return row ? JSON.parse(Buffer.from(row.result).toString("utf8")).code ?? null : null;
  } finally {
    db.close();
  }
}

/** Reads what the engine's private tmux server still runs after the engine stopped, then ends
 * it so the smoke leaves nothing behind. A stopped engine keeps the server only for team
 * agents that live on. Input: the engine's tmux socket path. Output: null when no server is
 * left, or the names of the terminals it still ran. */
function leftTerminals(socket) {
  if (!existsSync(socket)) return null;
  const tmux = TMUX_SEARCH.map((folder) => join(folder, "tmux")).find((path) => existsSync(path));
  if (!tmux) return [];
  const env = { PATH: "/usr/bin:/bin" };
  const listed = spawnSync(tmux, ["-S", socket, "list-windows", "-a", "-F", "#{window_name}"], { encoding: "utf8", env });
  spawnSync(tmux, ["-S", socket, "kill-server"], { stdio: "ignore", env });
  return (listed.stdout ?? "").split("\n").filter((name) => name && name !== "_crazyagent");
}

/** Thrown when a bridge call or receipt stops a step; carries the engine code. */
class Refused extends Error {
  constructor(code, detail) {
    super(`${code}${detail ? `: ${detail}` : ""}`);
    this.code = code;
    this.detail = detail;
  }
}

/** Encodes a JSON value with every object's keys sorted, so equal values give equal text.
 * Input: a JSON value. Output: its canonical text. */
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}

/** Thrown when a step cannot run here without something the smoke may not fake; carries why. */
class Blocked extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

/** Reads rows from the engine database without writing, as the leader reads the owner's.
 * Input: the database path, the query and its parameters. Output: the rows, none without a database. */
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

/** Drives the installed-style engine through the owner's path with the desktop bridge and
 * prints PASS, FAIL or BLOCKED with the engine code per step: the master, then the team
 * (delegate, agent status, stop, failover, cleanup). Send mirrors the app: an uncertain turn
 * is stopped first (except in 3b), and a master_source_conflict ends the conversation, opens
 * a new one and resends once. Input: environment paths. Output: exit 1 on any FAIL, 3 when
 * only BLOCKED steps remain. */
async function main() {
  const engineBinary = required("CRAZYAGENT_TEST_NATIVE_AUTH_HOST", "crazyagent binary (the engine and its native-auth host)");
  const bridgeBinary = required("SMOKE_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(realpathSync("/tmp"), "ca-of-")));
  const crazyHome = join(root, "h");
  const userHome = join(root, "u");
  const bin = join(root, "bin");
  const repo = join(root, "repos", REPO);
  const linked = join(root, "repos", `${REPO}-worktrees`);
  const projects = join(root, "projects");
  const evidence = process.env.SMOKE_EVIDENCE && isAbsolute(process.env.SMOKE_EVIDENCE) ? process.env.SMOKE_EVIDENCE : join(root, "evidence");
  for (const folder of [crazyHome, userHome, bin, repo, linked, projects, evidence]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  process.env.SMOKE_GIT_HOME = userHome;
  writeFileSync(join(bin, "claude"), STAND_IN, { mode: 0o700 });
  chmodSync(join(bin, "claude"), 0o700);
  ownerRepo(repo, linked);

  const env = { PATH: `${bin}:/usr/bin:/bin`, HOME: userHome, CRAZYAGENT_HOME: crazyHome, TMPDIR: root, LANG: "en_US.UTF-8" };
  const trail = [];
  const results = [];
  const state = { generation: 0, accounts: [], pool: null, project: null };
  let engine;
  let bridgeProcess;
  let bridge;
  const startEngine = async () => {
    ({ child: engine } = await start(engineBinary, ["engine"], env, evidence, "engine", /^ready$/m));
  };
  const call = (cmd, args) => bridgeCall(bridge, trail, cmd, args);
  const value = async (cmd, args) => {
    let answer = await call(cmd, args);
    for (let attempt = 1; answer.failure?.code === "busy" && attempt < 8; attempt += 1) {
      await pause(250 * attempt);
      answer = await call(cmd, args);
    }
    if (!("value" in answer)) throw new Refused(failureCode(cmd, answer), answer.failure?.message);
    return answer.value;
  };
  const generation = () => (state.generation += 1);
  const applied = (cmd, receipt) => {
    if (receipt?.outcome === "applied" && !receipt.pending) return receipt;
    const code = receipt?.rejection ?? (receipt?.pending ? "pending" : receipt?.outcome ?? "no_receipt");
    const recorded = receipt?.operation_id ? recordedCode(join(crazyHome, "engine.db"), receipt.operation_id) : null;
    throw new Refused(recorded && recorded !== code ? `${code} (engine: ${recorded})` : code, cmd);
  };
  const engineErr = join(evidence, "engine.err");
  const errText = () => (existsSync(engineErr) ? readFileSync(engineErr, "utf8") : "");
  let shutdownMark = 0;
  const step = async (id, title, run) => {
    const mark = errText().length;
    let row;
    try {
      row = { id, title, result: "PASS", code: (await run()) ?? "ok" };
    } catch (failure) {
      const result = failure instanceof Blocked ? "BLOCKED" : "FAIL";
      row = { id, title, result, code: failure instanceof Refused || failure instanceof Blocked ? failure.code : `script:${failure.message}` };
    }
    await pause(POLL_MS);
    const after = errText();
    shutdownMark = after.length;
    row.causes = [...new Set(after.slice(mark).match(/cause=[^\n]*/g) ?? [])].join("; ") || "-";
    results.push(row);
    console.log(`${row.result} ${id} ${title} [${row.code}] engine stderr: ${row.causes}`);
  };
  const needs = (what, from) => {
    if (!what) throw new Refused(`blocked_by_${from}`);
    return what;
  };

  const connection = async (id) => value("desktop_inspect_model_access", { collection: "connections", id });
  const settleAuth = async (id, action, want) => {
    let view;
    for (const deadline = Date.now() + AUTH_MS; ; await pause(POLL_MS)) {
      view = await value("desktop_read_native_auth", { ticket: (await connection(id)).ticket });
      if (!ACTIVE_AUTH.has(view.phase)) break;
      if (Date.now() > deadline) throw new Refused(`native_auth_still_${view.phase}`, action);
    }
    if (view.phase !== want) throw new Refused(view.code ?? `native_auth_${view.phase}`, action);
    return view;
  };
  const signIn = async (name) => {
    const account = await value("desktop_add_account", { draft: { provider: "claude", method: "subscription", name }, key: null });
    await value("desktop_start_native_auth", { ticket: (await connection(account.id)).ticket, method: "claude_official" });
    await settleAuth(account.id, "sign_in", "authenticated_observed");
    return account;
  };
  const memberStatus = async (accountId) => {
    const pools = await value("desktop_pool_summaries");
    return pools.flatMap((pool) => pool.members).find((member) => member.id === accountId)?.status ?? "absent";
  };
  const poolRecord = async (id) => {
    const detail = await value("desktop_inspect_model_access", { collection: "pools", id });
    if (detail.record.kind !== "pool" || !detail.writable) throw new Refused("invalid_pool");
    return detail;
  };
  const assignPool = async (projectId, poolId, remove) => {
    const opened = await value("desktop_open_project", { id: projectId });
    if (!opened.fresh || opened.project_id !== projectId) throw new Refused("invalid_project");
    const basis = await poolRecord(poolId);
    const now = basis.record.pool;
    const pool = remove ? { ...now, projects: now.projects.filter((id) => id !== projectId) }
      : { ...now, projects: now.projects.includes(projectId) ? now.projects : [...now.projects, projectId], roles: now.roles.includes("project_leader") ? now.roles : [...now.roles, "project_leader"] };
    if (JSON.stringify(pool) === JSON.stringify(now)) return "unchanged";
    applied("replace_pool", await value("desktop_edit_model_access", { ticket: basis.ticket, edit: { kind: "replace_pool", pool }, generation: generation() }));
    const saved = (await poolRecord(poolId)).record.pool;
    if (JSON.stringify(saved) !== JSON.stringify(pool)) throw new Refused("pool_unconfirmed");
    return "applied";
  };
  const saveSetup = async (projectId, change) => {
    const stored = await value("desktop_project_setup", { id: projectId });
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team: stored.team, documents: [], ...change };
    applied("update_project_setup", await value("desktop_update_project_setup", { id: projectId, setup, name: null, revision: stored.revision, generation: generation() }));
    return stored;
  };
  const endConversation = async () => {
    const opened = await value("desktop_open_project", { id: needs(state.project, "1") });
    const session = (await value("desktop_master_state", { ticket: opened.ticket })).session;
    if (session?.phase !== "configured") return;
    try {
      applied("close_master", await value("desktop_close_master", { ticket: opened.ticket, sessionId: session.session_id, generation: generation() }));
    } catch (failure) {
      throw failure instanceof Refused ? new Refused(`end_conversation:${failure.code}`) : failure;
    }
  };
  const isolated = async (previous) => {
    if (results.find((row) => row.id === previous)?.result !== "FAIL") return "";
    await endConversation();
    console.log(`note: ended the conversation first, since ${previous} failed (the owner's End conversation)`);
    return `fresh conversation since ${previous} failed; `;
  };
  const send = async (text, serving = null, release = true) => {
    const projectId = needs(state.project, "1");
    const opened = await value("desktop_open_project", { id: projectId });
    let master = await value("desktop_master_state", { ticket: opened.ticket });
    if (!master.session || master.session.phase !== "configured") {
      applied("open_master", await value("desktop_open_master", { ticket: opened.ticket, generation: generation() }));
      master = await value("desktop_master_state", { ticket: opened.ticket });
      if (!master.session) throw new Refused("master_session_missing");
    }
    let sessionId = master.session.session_id;
    const current = master.session.current_turn;
    if (release && current) {
      const held = (await value("desktop_master_turns", { ticket: opened.ticket, sessionId, cursor: null })).page.turns.find((item) => item.id === current);
      if (held?.phase === "uncertain") {
        try {
          applied("cancel_master_turn", await value("desktop_cancel_master_turn", { ticket: opened.ticket, session: sessionId, turn: current, generation: generation() }));
        } catch (failure) {
          throw failure instanceof Refused ? new Refused(`release_uncertain_turn:${failure.code}`) : failure;
        }
      }
    }
    let receipt = await value("desktop_submit_master", { ticket: opened.ticket, sessionId, text, generation: generation() });
    let recovered = "";
    if (receipt.outcome === "rejected" && receipt.rejection === "master_source_conflict") {
      applied("close_master", await value("desktop_close_master", { ticket: opened.ticket, sessionId, generation: generation() }));
      applied("open_master", await value("desktop_open_master", { ticket: opened.ticket, generation: generation() }));
      const fresh = (await value("desktop_master_state", { ticket: opened.ticket })).session;
      if (!fresh || fresh.phase !== "configured") throw new Refused("master_session_missing", "after settings changed");
      sessionId = fresh.session_id;
      receipt = await value("desktop_submit_master", { ticket: opened.ticket, sessionId, text, generation: generation() });
      recovered = "new conversation after settings changed, ";
    }
    applied("submit_master", receipt);
    let turn;
    for (const deadline = Date.now() + TURN_MS; ; await pause(POLL_MS)) {
      const turns = await value("desktop_master_turns", { ticket: opened.ticket, sessionId, cursor: null });
      turn = turns.page.turns.find((item) => item.id === receipt.operation_id);
      if (!turn) throw new Refused("master_turn_missing");
      if (!ACTIVE_TURN.has(turn.phase)) break;
      if (Date.now() > deadline) throw new Refused(`master_turn_still_${turn.phase}`);
    }
    const reason = turn.reason ?? "no_reason";
    const admitted = turn.phase === "succeeded" && turn.reply || turn.attempt !== null && !NOT_ADMITTED.has(reason);
    if (!admitted) throw new Refused(`${turn.phase}:${reason}`, turn.attempt === null ? "never admitted" : "admitted");
    if (serving && turn.attempt?.connection?.id !== serving) throw new Refused("served_by_signed_out_account", turn.attempt?.connection?.id);
    return `${recovered}${turn.phase === "succeeded" ? "replied" : `admitted, harness not runnable here (${turn.phase}:${reason})`}`;
  };

  const database = join(crazyHome, "engine.db");
  const team = { accounts: [], pool: null, agents: [], view: null };
  const teamStatus = async () => value("desktop_team_status", { id: needs(state.project, "1") });
  const teamNote = (status) => status.messages.filter((message) => message.author === "engine").at(-1)?.text ?? "no engine note";
  const delegate = async (prompt) => {
    const project = needs(state.project, "1");
    const before = new Set((await teamStatus()).tasks.map((task) => task.id));
    applied("team_delegate", await value("desktop_team_delegate", { id: project, harness: "claude-code", prompt, generation: generation() }));
    for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
      const status = await teamStatus();
      const task = status.tasks.find((item) => !before.has(item.id));
      const agent = status.agents.find((item) => item.id === task?.agent) ?? null;
      if (task && (agent || !["queued", "needs_pool"].includes(task.status))) return { status, task, agent };
      if (Date.now() > deadline) throw new Refused(task ? `team_task_still_${task.status}` : "team_task_missing", teamNote(status));
    }
  };
  const launched = (agent, task) => {
    const problem = agent.problem ?? task.problem;
    return `${agent.status}${problem ? `: ${problem}` : ""}`;
  };
  const liveTeamGroups = () => engineRows(database, "SELECT id, process_group FROM team_agents WHERE process_group IS NOT NULL")
    .filter((row) => groupAlive(Number(row.process_group)));

  try {
    await startEngine();
    const started = await start(bridgeBinary, [join(crazyHome, "engine.sock")], env, evidence, "bridge", /^ready (\d+) ([0-9a-f]{64})$/m);
    bridgeProcess = started.child;
    bridge = { port: Number(started.match[1]), token: started.match[2] };
    if ((await value("desktop_connect")).connected !== true) throw new Error("the bridge did not connect to the engine");
    applied("save_owner_profile", await value("desktop_save_owner_profile", { profile: { name: "Owner Flow" }, generation: generation() }));
    applied("set_projects_home", await value("desktop_set_projects_home", { path: projects, generation: generation() }));

    await step("2a", "two Claude accounts signed in through the native-auth host", async () => {
      state.accounts.push(await signIn("Owner Claude A"));
      state.accounts.push(await signIn("Owner Claude B"));
      return "authenticated_observed x2";
    });
    await step("2b", "a pool with both accounts", async () => {
      const members = state.accounts.map((account) => account.id);
      if (members.length !== 2) throw new Refused("blocked_by_2a");
      const page = await value("desktop_model_access_page", { collection: "pools", next: false });
      const id = "pool-ownerflow0000000000";
      applied("create_pool", await value("desktop_edit_model_access", { ticket: page.ticket, edit: { kind: "create_pool", pool: { id, name: "Owner pool", members, projects: [], roles: [], fallback_allowed: false } }, generation: generation() }));
      state.pool = id;
      return "applied";
    });
    await step("1", "project from the owner-shaped repository (Add folder, Set up here)", async () => {
      const pool = needs(state.pool, "2b");
      const receipt = applied("start_onboarding", await value("desktop_start_onboarding", { path: repo, pool, generation: generation() }));
      state.project = receipt.project_id;
      for (const deadline = Date.now() + SETUP_MS; ; await pause(POLL_MS * 4)) {
        const view = await value("desktop_onboarding", { id: state.project });
        if (!["waiting", "running"].includes(view.graph.kind) && !["waiting", "running"].includes(view.mapping.kind)) break;
        if (Date.now() > deadline) throw new Refused("onboarding_still_running");
      }
      await saveSetup(state.project, { about: "A repository shaped like the owner's." });
      return "applied";
    });
    await step("2c", "assign the pool to the project", async () => assignPool(needs(state.project, "1"), needs(state.pool, "2b"), false));
    await step("3", "open the conversation and send (readiness and worktree admission)", () => send("Hello master, what is in this repository?"));
    await step("3b", "send again after a turn the harness could not finish, with no extra owner action", () => send("Are you still there?", null, false));
    await step("4a", "change project settings (details and pool), send again (LB-14)", async () => {
      const project = needs(state.project, "1");
      const before = needs(state.pool, "2b");
      await saveSetup(project, { about: "A repository shaped like the owner's, with a changed description." });
      const page = await value("desktop_model_access_page", { collection: "pools", next: false });
      const after = "pool-ownerflow0000000001";
      applied("create_pool", await value("desktop_edit_model_access", { ticket: page.ticket, edit: { kind: "create_pool", pool: { id: after, name: "Owner pool 2", members: state.accounts.map((account) => account.id), projects: [], roles: [], fallback_allowed: false } }, generation: generation() }));
      await assignPool(project, after, false);
      await assignPool(project, before, true);
      await saveSetup(project, { pool: after });
      state.pool = after;
      return send("Settings changed. Still there?");
    });
    await step("4b", "restart the engine, send again (LB-15)", async () => {
      await stop(engine);
      await startEngine();
      let connected = false;
      for (const deadline = Date.now() + READY_MS; !connected; await pause(POLL_MS)) {
        connected = (await call("desktop_connect")).value?.connected === true;
        if (!connected && Date.now() > deadline) throw new Refused("bridge_reconnect_failed");
      }
      const note = await isolated("4a");
      return `${note}${await send("The engine restarted. Still there?")}`;
    });
    await step("4c", "first account signed out, send again (LB-17 failover)", async () => {
      const first = needs(state.accounts[0], "2a");
      await value("desktop_logout_native_auth", { ticket: (await connection(first.id)).ticket });
      await settleAuth(first.id, "logout", "unauthenticated_observed");
      const status = await memberStatus(first.id);
      if (status !== "not_signed_in") throw new Refused(`first_account_${status}`, "logout not shown");
      const note = await isolated("4b");
      return `${note}${await send("The first account is signed out. Still there?", needs(state.accounts[1], "2a").id)}`;
    });
    await step("5a", "delete an account that is in a pool (LB-16)", async () => {
      const second = needs(state.accounts[1], "2a");
      await value("desktop_remove_account", { id: second.id });
      return "removed";
    });
    await step("5b", "unassign the pool from the project (LB-16)", async () => {
      const project = needs(state.project, "1");
      await assignPool(project, needs(state.pool, "2b"), true);
      await saveSetup(project, { pool: null });
      const pool = (await value("desktop_project_setup", { id: project })).pool;
      if (pool !== null) throw new Refused("project_still_names_pool", pool);
      return "applied";
    });
    await step("5c", "delete a pool in use (LB-16)", async () => {
      const pool = needs(state.pool, "2b");
      const record = (await poolRecord(pool)).record.pool;
      if (!record.projects.includes(state.project)) await assignPool(needs(state.project, "1"), pool, false);
      if ((await value("desktop_project_setup", { id: state.project })).pool !== pool) await saveSetup(state.project, { pool });
      const detail = await poolRecord(pool);
      applied("remove_pool", await value("desktop_edit_model_access", { ticket: detail.ticket, edit: { kind: "remove_pool", id: pool }, generation: generation() }));
      return "applied";
    });
    await step("6", "two more Claude accounts in a new pool assigned to the project, for the team", async () => {
      const project = needs(state.project, "1");
      team.accounts.push(await signIn("Team Claude C"));
      team.accounts.push(await signIn("Team Claude D"));
      const page = await value("desktop_model_access_page", { collection: "pools", next: false });
      const id = "pool-ownerflowteam000000";
      applied("create_pool", await value("desktop_edit_model_access", { ticket: page.ticket, edit: { kind: "create_pool", pool: { id, name: "Team pool", members: team.accounts.map((account) => account.id), projects: [], roles: [], fallback_allowed: false } }, generation: generation() }));
      team.pool = id;
      await assignPool(project, id, false);
      await saveSetup(project, { pool: id });
      return "applied";
    });
    await step("6a", "team settings with sub-agents and tools on", async () => {
      const project = needs(state.project, "1");
      needs(team.pool, "6");
      const plan = { master: "claude-code", master_tools: true, sub_harnesses: [{ harness: "claude-code", agents: 2, tools: true }], min_agents: 1, max_agents: 3 };
      await saveSetup(project, { team: plan });
      const saved = (await value("desktop_project_setup", { id: project })).team;
      if (canonical(saved) !== canonical(plan)) throw new Refused("team_plan_unconfirmed", JSON.stringify(saved));
      const harness = (await teamStatus()).harnesses.find((item) => item.role === "sub" && item.harness === "claude-code");
      if (!harness?.tools) throw new Refused("team_status_without_sub_tools", JSON.stringify(harness ?? null));
      return `applied; sub-agent harness runnable=${harness.runnable}`;
    });
    await step("6b", "delegate a task: the engine prepares the sub-agent's worktree and launch", async () => {
      writeFileSync(join(bin, SILENT_MARK), "", { mode: 0o600 });
      const { task, agent } = await delegate("List the files in this repository.");
      if (!agent) throw new Refused(`team_task_${task.status}`, task.problem ?? "no agent");
      team.agents.push(agent.id);
      if (!agent.worktree) throw new Refused("team_agent_without_worktree", launched(agent, task));
      const folder = join(repo, agent.worktree);
      const listed = existsSync(folder) && git(repo, "worktree", "list", "--porcelain").split("\n").some((line) => line === `worktree ${realpathSync(folder)}`);
      if (!listed) throw new Refused("team_worktree_not_added", agent.worktree);
      return `worktree ${agent.worktree}; launch ${launched(agent, task)}`;
    });
    await step("6c", "the agent shows in the team status and in the agents view", async () => {
      const id = needs(team.agents[0], "6b");
      const member = (await teamStatus()).agents.find((item) => item.id === id);
      if (!member || member.role !== "sub") throw new Refused("team_status_without_agent");
      const view = (await value("desktop_agents", { projectId: state.project })).find((item) => item.label === `Team sub-agent ${id}`);
      if (!view) throw new Refused("agents_view_without_team_agent", `team ${member.status}`);
      team.view = view.id;
      if (member.status !== "working" || view.state !== "running" || !view.can_stop) throw new Refused(`team_agent_not_live_${member.status}`, `agents view ${view.state}, can_stop ${view.can_stop}, ${member.problem ?? "no problem"}`);
      return `team ${member.status}, agents view ${view.state}`;
    });
    await step("6d", "stop the agent: it ends and its worktree is no longer held", async () => {
      const id = needs(team.agents[0], "6b");
      const stopped = await value("desktop_stop_agent", { id: needs(team.view, "6c") });
      let member;
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
        member = (await teamStatus()).agents.find((item) => item.id === id);
        if (member && ["stopped", "failed"].includes(member.status)) break;
        if (Date.now() > deadline) throw new Refused(`team_agent_still_${member?.status ?? "missing"}`, `stop answered ${stopped.state}`);
      }
      if (member.process_group !== null && groupAlive(member.process_group)) throw new Refused("team_agent_group_alive", String(member.process_group));
      const held = engineRows(database, "SELECT task FROM worktree_leases WHERE path LIKE ?1 OR body LIKE ?1", `%agent-${id}%`);
      if (held.length) throw new Refused("worktree_lease_kept", held.map((row) => row.task).join(","));
      const record = git(repo, "worktree", "list", "--porcelain").split("\n\n").find((entry) => entry.includes(`/agent-${id}\n`));
      if (record?.includes("\nlocked")) throw new Refused("worktree_still_locked", member.worktree);
      return `stop answered ${stopped.state}; team ${member.status}`;
    });
    await step("6e", "one pool account rate-limited: the next agent launch uses the other member (DEC-091)", async () => {
      throw new Blocked("needs_a_real_rate_limit: only a provider reply pauses an account, and the smoke may not fabricate one");
    });
    await step("6e2", "one pool account signed out: the next agent launch uses the other member", async () => {
      const [first, second] = [needs(team.accounts[0], "6"), needs(team.accounts[1], "6")];
      await value("desktop_logout_native_auth", { ticket: (await connection(first.id)).ticket });
      await settleAuth(first.id, "logout", "unauthenticated_observed");
      const { task, agent } = await delegate("Read the README of this repository.");
      if (!agent) throw new Refused(`team_task_${task.status}`, task.problem ?? "no agent");
      team.agents.push(agent.id);
      if (agent.account !== second.id) throw new Refused("team_agent_on_signed_out_account", agent.account ?? "no account");
      return `agent on the other member; launch ${launched(agent, task)}`;
    });
    await step("6f", "End conversation and Remove project clean up the team's agents", async () => {
      const project = needs(state.project, "1");
      if (!team.agents.length) throw new Refused("blocked_by_6b");
      await endConversation();
      applied("remove_project", await value("desktop_remove_project", { id: project, generation: generation() }));
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
        const running = (await value("desktop_agents", { projectId: null })).filter((item) => item.project === project && item.state === "running");
        const groups = liveTeamGroups();
        if (!running.length && !groups.length) break;
        if (Date.now() > deadline) throw new Refused("team_agents_left_running", `views ${running.map((item) => item.id).join(",") || "-"}, groups ${groups.map((row) => row.id).join(",") || "-"}`);
      }
      const after = await call("desktop_team_status", { id: project });
      if ("value" in after) throw new Refused("team_status_after_remove", `${after.value.agents.length} agents`);
      return `removed; team status ${failureCode("desktop_team_status", after)}`;
    });
  } catch (failure) {
    results.push({ id: "setup", title: "start the engine and bridge", result: "FAIL", code: failure instanceof Refused ? failure.code : `script:${failure.message}`, causes: "-" });
    console.log(`FAIL setup [${results.at(-1).code}]`);
  } finally {
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(trail, null, 1).replace(/[0-9a-f]{64}/g, (hex) => hex.slice(0, 8)));
    await stop(bridgeProcess);
    await stop(engine);
    await pause(POLL_MS);
    const left = leftTerminals(join(crazyHome, "terminal", "tmux.sock"));
    const kept = left?.filter((name) => name.startsWith("team-")) ?? [];
    const row = { id: "7", title: "The engine stop leaves no tmux server without a live team agent", causes: "-" };
    if (left === null) results.push({ ...row, result: "PASS", code: "no_server" });
    else if (kept.length) results.push({ ...row, result: "PASS", code: `kept_for_${kept.join(",")}` });
    else results.push({ ...row, result: "FAIL", code: "tmux_server_left", causes: left.join(",") || "-" });
  }
  const shutdown = [...new Set(errText().slice(shutdownMark).match(/cause=[^\n]*/g) ?? [])].join("; ") || "-";
  console.log("\nstep | result | engine code | engine stderr | what");
  for (const row of results) console.log(`${row.id} | ${row.result} | ${row.code} | ${row.causes} | ${row.title}`);
  console.log(`engine stderr at the final shutdown (after the last step): ${shutdown}`);
  const failed = results.filter((row) => row.result === "FAIL").length;
  const blocked = results.filter((row) => row.result === "BLOCKED").length;
  console.log(`\n${results.length - failed - blocked} PASS, ${failed} FAIL, ${blocked} BLOCKED; evidence ${process.env.SMOKE_EVIDENCE ? evidence : "removed (set SMOKE_EVIDENCE to keep it)"}`);
  if (!process.env.SMOKE_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  process.exitCode = failed > 0 ? 1 : blocked > 0 ? 3 : 0;
}

await main();
