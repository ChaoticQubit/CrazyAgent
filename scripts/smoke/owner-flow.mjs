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
const POLL_MS = 250;
const MERGE_BASE_KEYS = 50;
const LINKED_WORKTREES = 41;
const REPO = "owner-shaped";
const ACTIVE_AUTH = new Set(["starting", "awaiting_owner", "checking_account", "cancelling"]);
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const NOT_ADMITTED = new Set(["readiness_unknown", "readiness_stale", "invalid_authentication", "connection_changed", "scope_revoked", "repository_missing", "unknown"]);
const STAND_IN = `#!/bin/sh
marker="$CLAUDE_CONFIG_DIR/.owner-flow-signed-in"
if [ "$1" != "auth" ]; then
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

/** Thrown when a bridge call or receipt stops a step; carries the engine code. */
class Refused extends Error {
  constructor(code, detail) {
    super(`${code}${detail ? `: ${detail}` : ""}`);
    this.code = code;
    this.detail = detail;
  }
}

/** Drives the installed-style engine through the owner's path with the desktop bridge and
 * prints PASS or FAIL with the engine code per step. Send mirrors the app: an uncertain turn
 * is stopped first (except in 3b), and a master_source_conflict ends the conversation, opens
 * a new one and resends once. Input: environment paths. Output: exit code 1 on any FAIL. */
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
      row = { id, title, result: "FAIL", code: failure instanceof Refused ? failure.code : `script:${failure.message}` };
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
  } catch (failure) {
    results.push({ id: "setup", title: "start the engine and bridge", result: "FAIL", code: failure instanceof Refused ? failure.code : `script:${failure.message}`, causes: "-" });
    console.log(`FAIL setup [${results.at(-1).code}]`);
  } finally {
    writeFileSync(join(evidence, "answers.json"), JSON.stringify(trail, null, 1).replace(/[0-9a-f]{64}/g, (hex) => hex.slice(0, 8)));
    await stop(bridgeProcess);
    await stop(engine);
    await pause(POLL_MS);
  }
  const shutdown = [...new Set(errText().slice(shutdownMark).match(/cause=[^\n]*/g) ?? [])].join("; ") || "-";
  console.log("\nstep | result | engine code | engine stderr | what");
  for (const row of results) console.log(`${row.id} | ${row.result} | ${row.code} | ${row.causes} | ${row.title}`);
  console.log(`engine stderr at the final shutdown (after the last step): ${shutdown}`);
  const failed = results.filter((row) => row.result === "FAIL").length;
  console.log(`\n${results.length - failed} PASS, ${failed} FAIL; evidence ${process.env.SMOKE_EVIDENCE ? evidence : "removed (set SMOKE_EVIDENCE to keep it)"}`);
  if (!process.env.SMOKE_EVIDENCE) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  process.exitCode = failed === 0 ? 0 : 1;
}

await main();
