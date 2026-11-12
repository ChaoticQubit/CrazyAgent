import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";
import { build } from "vite";
import { withPage } from "../browser.mjs";
import { INTERNALS, bridgeCall, owner, serveApp, start, stop } from "./e2e-harness.mjs";

const appRoot = fileURLToPath(new URL("../..", import.meta.url));
const LIVE_MASTER = fileURLToPath(new URL("./questions-live-master.mjs", import.meta.url));
const ROOT_PREFIX = "ca-qf-";
const REPO = "qf-shop";
const IMPORTED = "qf-imported";
const ABOUT = "A small shop that sells house plants online.";
const CONTROL = "qf-live-master";
const SILENT = "qf-silent";
const SILENT_S = 300;
const STAND_IN_SIGN = "questions-flow stand-in";
const BRIDGE_READY = /^ready (\d+) ([0-9a-f]{64})$/m;
const POLL_MS = 250;
const AUTH_MS = 60000;
const SETUP_MS = 180000;
const LAUNCH_MS = 120000;
const CALL_MS = 150000;
const ASK_SHOWN_MS = 10000;
const TOAST_MS = 30000;
const VIEW_MS = 30000;
const DELIVERY_MS = 90000;
const QUIET_MS = 20000;
const SCAN_MS = 30000;
const TUI_MS = 30000;
const JOB_MS = 30000;
const PLAN_REAL_MS = 30 * 60 * 1000;
const CONTROL_DEPTH_MAX = 10;
const TABS_MAX = 12;
const OPEN_POINTS_MAX = 8;
const TMUX_SEARCH = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const SHIFT_RIGHT = ["1b", "5b", "31", "3b", "32", "43"];
const ACTIVE_AUTH = new Set(["starting", "awaiting_owner", "checking_account", "cancelling"]);
const SLICES = {
  "QL-1": "feat/v2-questions-engine: ask and record_answer tools, the log, delivery, questions.md",
  "QL-2": "feat/v2-questions-desktop: Questions tab, toast, bridge commands (then regenerate routes: node test/native/bridge-routes.mjs)",
  "QL-3": "feat/v2-questions-tui: TUI footer badge and Questions page",
  "QL-4": "feat/v2-questions-screen reworked onto the b01ad309 API: terminal question boxes, permission prompts",
  "QL-5": "feat/v2-questions-notify: macOS system notification",
  "RM-1": "feat/v2-roadmap-job: planning job, context/roadmap.md, context/release-process.md, open points",
  "RM-2": "feat/v2-roadmap-ui: roadmap toggle, Plan roadmap now, planning_busy (then regenerate routes)",
  "OB-9": "fix/v221-ob9-questions-inline: the Questions show setting and the questions pop-up on project tabs",
  "S4": "fix/v232-questions: withdraw_question, record_answer from the master's terminal, ask replaces, Dismiss all",
};
const ASKS = {
  view: { question: "Which database should the plant shop use?", options: ["Postgres", "SQLite", "MySQL"], recommended: "Postgres", context: "The shop keeps orders and stock." },
  dismiss: { question: "Should the shop send a weekly newsletter?", options: ["Yes", "No"], recommended: "No" },
  chat: { question: "Which payment provider should checkout use?" },
  tui: { question: "Which hosting region should the shop use?", options: ["Europe", "North America"], recommended: "Europe" },
  popup: { question: "Should the shop offer gift wrapping?", options: ["Yes", "No"], recommended: "Yes" },
  every: { question: "Should the shop show how many plants are in stock?", options: ["Yes", "No"], recommended: "No" },
  tabOnly: { question: "Should the shop ship plants abroad?", options: ["Yes", "No"], recommended: "No" },
  withdraw: { question: "Should the shop sell seeds too?", options: ["Yes", "No"], recommended: "No" },
  bulkFirst: { question: "Should the shop offer plant care tips?", options: ["Yes", "No"], recommended: "Yes" },
  bulkSecond: { question: "Should the shop take returns?", options: ["Yes", "No"], recommended: "Yes" },
};
const POPUP = "section.qp-popup";
const POPUP_QUIET_MS = 12000;
const CHAT_ANSWER = "Use Stripe for checkout.";
const TUI_ANSWER = "Europe, close to the owner.";
const RESTART_NOTE = "The engine restarted. Carry on.";
const CLEAR = "\x1b[2J\x1b[H";
const PERMISSION = " Bash command\n   cargo test\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for cargo test commands\n   3. No, and tell Claude what to do differently (esc)\n";
const BOX = " ☐ Sign-in\n\n Which sign-in should the app use?\n\n ❯ 1. OAuth\n      The provider's own sign-in page\n   2. API key\n      A key kept in project settings\n   3. Type something.\n\n Enter to select · ↑/↓ to navigate · Esc to cancel\n";
const BOX_QUESTION = "Which sign-in should the app use?";

/** The stand-in `claude` the engine finds on PATH: it signs in and out through a private
 * keychain like Claude Code, runs the live master stand-in for an interactive launch and
 * refuses headless turns, or holds them while the silent mark exists so a job stays running.
 * Inputs: node's path and the bin folder. Output: the script text. */
function standIn(node, bin) {
  for (const path of [node, bin]) {
    if (!isAbsolute(path) || /["$`\\\s]/.test(path)) throw new Error(`stand-in path must be plain and absolute: ${path}`);
  }
  return `#!/bin/sh
marker="$CLAUDE_CONFIG_DIR/.questions-flow-signed-in"
keychain="$CLAUDE_CONFIG_DIR/Library/Keychains/login.keychain-db"
service="Claude Code-credentials-$(printf %s "$CLAUDE_CONFIG_DIR" | /usr/bin/shasum -a 256 | /usr/bin/cut -c1-8)"
if [ "$1" != "auth" ]; then
  case " $* " in
    *" --print "*) [ -f "${bin}/${SILENT}" ] && exec /bin/sleep ${SILENT_S} ;;
    *" --session-id "*|*" --resume "*) exec "${node}" "${bin}/live-master.mjs" "$@" ;;
  esac
  echo "${STAND_IN_SIGN}: this claude runs only the live master stand-in and auth commands" >&2
  exit 64
fi
case "$2" in
  login)
    expires=$(( ( $(/bin/date +%s) + 28800 ) * 1000 ))
    record=$(printf '{"claudeAiOauth":{"accessToken":"questions-flow-stand-in","refreshToken":"questions-flow-stand-in","expiresAt":%s,"scopes":["user:inference"]}}' "$expires")
    if [ -x /usr/bin/security ]; then
      hex=$(printf %s "$record" | /usr/bin/od -An -tx1 | /usr/bin/tr -d ' \\n')
      printf 'add-generic-password -U -a questions-flow -s "%s" -X %s "%s"\\n' "$service" "$hex" "$keychain" | /usr/bin/security -i >/dev/null || exit 1
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
}

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

/** Reads the approved real-profile run for the roadmap documents and the clone: the data
 * root of a running engine where a Claude account is signed in and the pool that holds it.
 * Paths and ids only, never a secret. Input: none. Output: null without the variables, the
 * problem when they are incomplete, else the socket and pool. */
function realProfile() {
  const root = process.env.QUESTIONS_FLOW_REAL_RUNTIME;
  const pool = process.env.QUESTIONS_FLOW_REAL_POOL;
  if (!root && !pool) return null;
  if (!root || !pool) return { problem: "real_profile_incomplete: set QUESTIONS_FLOW_REAL_RUNTIME and QUESTIONS_FLOW_REAL_POOL" };
  if (!isAbsolute(root) || !existsSync(join(root, "engine.db"))) return { problem: "real_profile_runtime_without_engine_db" };
  return { socket: join(root, "engine.sock"), database: join(root, "engine.db"), pool, clone: process.env.QUESTIONS_FLOW_CLONE ?? null };
}

/** Runs git in a folder and stops the run when it fails. Inputs: the folder, the home git
 * reads and the git arguments. Output: git's standard output. */
function git(cwd, home, ...args) {
  const run = spawnSync("git", ["-c", "user.name=Questions Flow", "-c", "user.email=owner@example.invalid", "-c", "commit.gpgsign=false", ...args],
    { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: home, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  if (run.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${run.stderr.trim()}`);
  return run.stdout;
}

/** Makes one of the owner's existing projects: a git repository with one commit.
 * Inputs: the folder, its readme title and the git home. Output: none. */
function gitRepo(folder, title, home) {
  mkdirSync(join(folder, "src"), { recursive: true, mode: 0o700 });
  writeFileSync(join(folder, "README.md"), `# ${title}\n\nSells house plants.\n`);
  writeFileSync(join(folder, "src", "main.rs"), "fn main() {\n    println!(\"plants\");\n}\n");
  git(folder, home, "init", "-q", "-b", "main");
  git(folder, home, "add", ".");
  git(folder, home, "commit", "-q", "-m", "Start the plant shop");
}

/** Thrown when a step's check fails; carries the code the summary prints. */
class Refused extends Error {
  constructor(code, detail) {
    super(`${code}${detail ? `: ${detail}` : ""}`);
    this.code = detail ? `${code}: ${detail}` : code;
  }
}

/** Thrown when a step cannot run here without a slice, tool or profile it may not fake. */
class Blocked extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

/** Reads rows from an engine database without writing. A missing table means the slice that
 * makes it is not merged. Inputs: the database path, the query and its parameters. Output:
 * the rows, none without a database. */
function rows(database, sql, ...params) {
  if (!existsSync(database)) return [];
  const db = new DatabaseSync(database, { readOnly: true });
  try {
    return db.prepare(sql).all(...params);
  } catch (error) {
    const table = /no such table: (\w+)/.exec(error.message);
    if (table) throw new Refused(`no_table_${table[1]}`, table[1] === "questions_log" ? "needs QL-1" : null);
    throw error;
  } finally {
    db.close();
  }
}

/** Reads the code the engine recorded for one owner operation, for a refusal the bridge
 * collapses. Inputs: the database path and the operation id. Output: the code or null. */
function recordedCode(database, operationId) {
  const [row] = rows(database, "SELECT result FROM owner_operations WHERE operation_id = ?", operationId);
  return row ? JSON.parse(Buffer.from(row.result).toString("utf8")).code ?? null : null;
}

/** Finds the live master stand-in's control folder inside an account home under the engine
 * root. Input: the engine root. Output: the folder, or null before the master launched. */
function findControl(root) {
  let level = [root];
  for (let depth = 0; depth < CONTROL_DEPTH_MAX && level.length > 0; depth += 1) {
    const next = [];
    for (const folder of level) {
      let entries = [];
      try {
        entries = readdirSync(folder, { withFileTypes: true });
      } catch {
        entries = [];
      }
      for (const entry of entries) {
        if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
        if (entry.name === CONTROL) return join(folder, entry.name);
        next.push(join(folder, entry.name));
      }
    }
    level = next;
  }
  return null;
}

/** Reads the JSON lines the stand-in appended. Inputs: the control folder and file name.
 * Output: the parsed lines. */
function lines(control, name) {
  const path = join(control, name);
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

/** Counts the turns typed into the live master that contain a text. Inputs: the control
 * folder and the text. Output: the count. */
function typedCount(control, text) {
  return lines(control, "turns.jsonl").filter((turn) => turn.text.includes(text)).length;
}

/** Hands the stand-in one request file, written whole. Inputs: the control folder, the
 * request id and suffix, and the text. Output: none. */
function hand(control, id, suffix, text) {
  const staged = join(control, `.${id}${suffix}.partial`);
  writeFileSync(staged, text, { mode: 0o600 });
  renameSync(staged, join(control, `${id}${suffix}`));
}

let handed = 0;

/** Has the live master call one tool of the engine's team tool server, as Claude Code
 * does, and waits for the server's answer. Inputs: the control folder, the tool and its
 * arguments. Output: `{text, error}` from the tool result. */
async function toolCall(control, tool, args) {
  handed += 1;
  const id = `call-${Date.now()}-${handed}`;
  hand(control, id, ".call.json", JSON.stringify({ tool, arguments: args }));
  const reply = join(control, `${id}.reply.json`);
  for (const deadline = Date.now() + CALL_MS; !existsSync(reply); await pause(POLL_MS)) {
    if (Date.now() > deadline) throw new Refused("tool_call_unanswered", tool);
  }
  const answer = JSON.parse(readFileSync(reply, "utf8"));
  if (answer.error) throw new Refused("tool_call_failed", `${tool}: ${answer.error}`);
  const text = (answer.result?.content ?? []).filter((item) => item.type === "text").map((item) => item.text).join("\n");
  return { text, error: answer.result?.isError === true };
}

/** Has the live master print text on its screen. Inputs: the control folder and the text.
 * Output: none. */
function show(control, text) {
  handed += 1;
  hand(control, `show-${Date.now()}-${handed}`, ".show.txt", text);
}

/** Waits until a check returns a value other than null or false.
 * Inputs: what is awaited, the limit and the check. Output: the value. */
async function until(what, ms, check) {
  for (const deadline = Date.now() + ms; ; await pause(POLL_MS)) {
    const found = await check();
    if (found !== null && found !== false && found !== undefined) return found;
    if (Date.now() > deadline) throw new Refused(`timeout_${what}`);
  }
}

/** Drives a TUI in a private tmux server, as the Rust TUI end-to-end harness does.
 * Inputs: the tmux binary, the socket, the command and its environment. Output: the driver. */
function tuiPty(tmux, socket, command, env) {
  const run = (...args) => {
    const done = spawnSync(tmux, ["-S", socket, ...args], { encoding: "utf8", env });
    if (done.status !== 0) throw new Refused("tmux_failed", `${args[0]}: ${(done.stderr ?? "").trim()}`);
    return done.stdout ?? "";
  };
  run("-f", "/dev/null", "new-session", "-d", "-s", "qf", "-x", "120", "-y", "40", command);
  const screen = () => run("capture-pane", "-p", "-t", "qf");
  return {
    screen,
    keys: (...keys) => run("send-keys", "-t", "qf", ...keys),
    bytes: (hex) => run("send-keys", "-t", "qf", "-H", ...hex),
    text: (text) => run("send-keys", "-t", "qf", "-l", text),
    until: (what, check) => until(`tui_${what}`, TUI_MS, () => {
      const now = screen();
      return check(now) ? now : null;
    }),
    close: () => spawnSync(tmux, ["-S", socket, "kill-server"], { stdio: "ignore", env }),
  };
}

/** Reads the TUI's footer badge. Input: the screen. Output: the pending count it shows, or
 * null without a badge. */
function badge(screen) {
  const found = /\b(\d+) questions?\b/.exec(screen);
  return found ? Number(found[1]) : null;
}

/** Reads which project tab the TUI shows from its breadcrumb. Inputs: the screen and the
 * project. Output: the tab name, or null when the project screen is not shown. */
function tabShown(screen, project) {
  const crumb = `Home  /  ${project}`;
  const line = screen.split("\n")[0] ?? "";
  const at = line.indexOf(crumb);
  if (at < 0) return null;
  const rest = line.slice(at + crumb.length);
  return rest.startsWith("  /  ") ? rest.slice(5).split("  ")[0].trim() : "Overview";
}

/** Finds the key a TUI screen offers for an action, from its `<key> <Action>` hint.
 * Inputs: the screen and the action word. Output: the key, or null. */
function hintKey(screen, action) {
  const found = new RegExp(`(?:^|\\s)(Enter|Tab|[a-z0-9])\\s+${action}\\b`, "m").exec(screen);
  return found ? found[1] : null;
}

async function main() {
  const engineBinary = required("CRAZYAGENT_TEST_NATIVE_AUTH_HOST", "crazyagent binary (the engine, its native-auth host and the team tool server)");
  const bridgeBinary = required("SMOKE_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  const real = realProfile();
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(realpathSync("/tmp"), ROOT_PREFIX)));
  const keep = process.env.QUESTIONS_FLOW_EVIDENCE === "1";
  const evidence = join(root, "evidence");
  const crazyHome = join(root, "h");
  const userHome = join(root, "u");
  const bin = join(root, "bin");
  const projects = join(root, "projects");
  const repo = join(root, "repos", REPO);
  const imported = join(root, "repos", IMPORTED);
  for (const folder of [evidence, crazyHome, userHome, bin, projects, repo, imported]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  console.log(`temp root ${root} (${keep ? "kept for QUESTIONS_FLOW_EVIDENCE=1" : "removed on exit; QUESTIONS_FLOW_EVIDENCE=1 keeps it"})`);
  gitRepo(repo, "Plant shop", userHome);
  gitRepo(imported, "Imported plant shop", userHome);
  writeFileSync(join(bin, "claude"), standIn(process.execPath, bin), { mode: 0o700 });
  chmodSync(join(bin, "claude"), 0o700);
  copyFileSync(LIVE_MASTER, join(bin, "live-master.mjs"));
  const tmux = TMUX_SEARCH.map((folder) => join(folder, "tmux")).find((path) => existsSync(path)) ?? null;

  const env = { PATH: `${bin}:/usr/bin:/bin`, HOME: userHome, CRAZYAGENT_HOME: crazyHome, TMPDIR: root, LANG: "en_US.UTF-8" };
  const database = join(crazyHome, "engine.db");
  const socket = join(crazyHome, "engine.sock");
  const trail = [];
  const results = [];
  const state = { generation: 0, account: null, pool: null, project: null, control: null, asked: {}, real: null };
  const bridge = { port: 0, token: "" };
  const children = { engine: null, bridge: null, real: null };
  let restarts = 0;
  let site = null;

  const call = async (cmd, args = {}, via = bridge) => {
    const asked = Date.now();
    let answer;
    try {
      answer = await bridgeCall(via, cmd, args);
    } catch (error) {
      answer = { failure: { code: "bridge_unreachable", message: error.message } };
    }
    trail.push({ at: asked, ms: Date.now() - asked, cmd, args, answer });
    return answer;
  };
  const value = async (cmd, args = {}, via = bridge) => {
    let answer = await call(cmd, args, via);
    for (let attempt = 1; answer.failure?.code === "busy" && attempt < 8; attempt += 1) {
      await pause(250 * attempt);
      answer = await call(cmd, args, via);
    }
    if ("unknown" in answer) throw new Blocked(`unrouted:${cmd}: merge the slice that adds it, then regenerate the e2e bridge routes`);
    if ("invalid" in answer) throw new Refused(`invalid_args:${cmd}`, String(answer.invalid));
    if (!("value" in answer)) throw new Refused(answer.failure?.code ?? "no_value", `${cmd}${answer.failure?.message ? ` ${answer.failure.message}` : ""}`);
    return answer.value;
  };
  const generation = () => (state.generation += 1);
  const applied = (cmd, receipt, db = database) => {
    if (receipt?.outcome === "applied" && !receipt.pending) return receipt;
    const code = receipt?.rejection ?? (receipt?.pending ? "pending" : receipt?.outcome ?? "no_receipt");
    const recorded = receipt?.operation_id ? recordedCode(db, receipt.operation_id) : null;
    throw new Refused(recorded && recorded !== code ? `${code} (engine: ${recorded})` : code, cmd);
  };
  const step = async (id, title, needs, run) => {
    let row;
    const began = Date.now();
    try {
      row = { id, title, needs, result: "PASS", code: (await run()) ?? "ok" };
    } catch (failure) {
      const result = failure instanceof Blocked ? "BLOCKED" : "FAIL";
      row = { id, title, needs, result, code: failure instanceof Refused || failure instanceof Blocked ? failure.code : `script: ${failure.message}` };
    }
    row.ms = Date.now() - began;
    results.push(row);
    console.log(`${row.result} ${id} ${title} [${row.code}] needs: ${needs.join(", ") || "V1 only"} (${row.ms} ms)`);
    return row.result === "PASS";
  };
  const manual = (id, title, needs, instructions) => {
    results.push({ id, title, needs, result: "MANUAL", code: instructions, ms: 0 });
    console.log(`MANUAL ${id} ${title} needs: ${needs.join(", ")}\n  ${instructions}`);
  };
  const needs = (what, from) => {
    if (!what) throw new Refused(`blocked_by_${from}`);
    return what;
  };
  const startEngine = async () => {
    const name = restarts === 0 ? "engine" : `engine-${restarts + 1}`;
    ({ child: children.engine } = await start(engineBinary, ["engine"], env, evidence, name, /^ready$/m));
    const started = await start(bridgeBinary, [socket], env, evidence, restarts === 0 ? "bridge" : `bridge-${restarts + 1}`, BRIDGE_READY);
    children.bridge = started.child;
    bridge.port = Number(started.match[1]);
    bridge.token = started.match[2];
    if ((await value("desktop_connect")).connected !== true) throw new Error("the bridge did not connect to the engine");
  };
  const connection = async (id) => value("desktop_inspect_model_access", { collection: "connections", id });
  const signIn = async (name) => {
    const account = await value("desktop_add_account", { draft: { provider: "claude", method: "subscription", name }, key: null });
    await value("desktop_start_native_auth", { ticket: (await connection(account.id)).ticket, method: "claude_official" });
    const view = await until("native_auth", AUTH_MS, async () => {
      const now = await value("desktop_read_native_auth", { ticket: (await connection(account.id)).ticket });
      return ACTIVE_AUTH.has(now.phase) ? null : now;
    });
    if (view.phase !== "authenticated_observed") throw new Refused(view.code ?? `native_auth_${view.phase}`, "sign_in");
    return account;
  };
  const assignPool = async (projectId, poolId) => {
    const opened = await value("desktop_open_project", { id: projectId });
    if (opened.project_id !== projectId) throw new Refused("invalid_project");
    const basis = await value("desktop_inspect_model_access", { collection: "pools", id: poolId });
    const now = basis.record.pool;
    const pool = { ...now, projects: now.projects.includes(projectId) ? now.projects : [...now.projects, projectId], roles: now.roles.includes("project_leader") ? now.roles : [...now.roles, "project_leader"] };
    if (JSON.stringify(pool) === JSON.stringify(now)) return;
    applied("replace_pool", await value("desktop_edit_model_access", { ticket: basis.ticket, edit: { kind: "replace_pool", pool }, generation: generation() }));
  };
  const saveSetup = async (projectId, change, via = bridge, next = generation, db = database) => {
    const stored = await value("desktop_project_setup", { id: projectId }, via);
    const setup = { about: stored.about, tech_stack: stored.tech_stack, platforms: stored.platforms, research: stored.research, pool: stored.pool, team: stored.team, documents: [], ...change };
    applied("update_project_setup", await value("desktop_update_project_setup", { id: projectId, setup, name: null, revision: stored.revision, generation: next() }, via), db);
  };
  const onboard = async (path, pool, via = bridge, next = generation, db = database, ms = SETUP_MS) => {
    const receipt = applied("start_onboarding", await value("desktop_start_onboarding", { path, pool, generation: next() }, via), db);
    await until("onboarding_settled", ms, async () => {
      const view = await value("desktop_onboarding", { id: receipt.project_id }, via);
      return !["waiting", "running"].includes(view.graph?.kind) && !["waiting", "running"].includes(view.mapping?.kind);
    });
    return receipt.project_id;
  };
  const question = (id) => {
    const [row] = rows(database, "SELECT id, project, asker, status, answer, answered_by, closed_by FROM questions_log WHERE id = ?", id);
    return row ?? null;
  };
  const delivered = (id, verb) => `The owner ${verb} your question ${id} (`;
  const deliveredRows = (id, verb) => rows(database, "SELECT id, text FROM team_messages WHERE project = ? AND text LIKE ?", state.project, `${delivered(id, verb)}%`);
  const ask = async (key) => {
    const control = needs(state.control, "0");
    const asked = await toolCall(control, "ask", ASKS[key]);
    if (asked.error) throw new Refused("ask_refused", `${asked.text} (needs QL-1)`);
    const id = Number(/Question (\d+) is logged/.exec(asked.text)?.[1] ?? 0);
    if (!id) throw new Refused("ask_reply_without_question_number", asked.text);
    state.asked[key] = id;
    return id;
  };
  const deliveredOnce = async (id, verb, mustInclude) => {
    const control = needs(state.control, "0");
    await until(`delivery_of_${id}`, DELIVERY_MS, () => typedCount(control, delivered(id, verb)) > 0);
    await pause(QUIET_MS);
    const typed = lines(control, "turns.jsonl").filter((turn) => turn.text.includes(delivered(id, verb)));
    const stored = deliveredRows(id, verb);
    if (typed.length !== 1) throw new Refused("delivered_more_than_once", `${typed.length} typed turns carry question ${id}`);
    if (stored.length !== 1) throw new Refused("delivery_messages_not_one", `${stored.length} team messages carry question ${id}`);
    if (mustInclude && !typed[0].text.includes(mustInclude)) throw new Refused("delivery_without_answer", typed[0].text.slice(0, 200));
    return typed[0].text;
  };
  const staleAgain = async (cmd, args) => {
    const answer = await call(cmd, args);
    if ("unknown" in answer) throw new Blocked(`unrouted:${cmd}`);
    const code = answer.failure?.code ?? (answer.value === undefined ? "no_value" : "accepted");
    if (code !== "questions_stale") throw new Refused("second_settle_not_stale", `${cmd} answered ${code}`);
  };

  const interrupt = async () => {
    await stop(children.real);
    await stop(children.bridge);
    await stop(children.engine);
    process.exit(130);
  };
  process.once("SIGINT", interrupt);
  process.once("SIGTERM", interrupt);

  try {
    await startEngine();
    applied("save_owner_profile", await value("desktop_save_owner_profile", { profile: { name: "Questions Flow" }, generation: generation() }));
    applied("set_projects_home", await value("desktop_set_projects_home", { path: projects, generation: generation() }));

    await step("0", "a project with a live Claude master whose launch names the engine's team tool server", [], async () => {
      state.account = await signIn("Questions Claude");
      const page = await value("desktop_model_access_page", { collection: "pools", next: false });
      const pool = "pool-questionsflow000000";
      applied("create_pool", await value("desktop_edit_model_access", { ticket: page.ticket, edit: { kind: "create_pool", pool: { id: pool, name: "Questions pool", members: [state.account.id], projects: [], roles: [], fallback_allowed: false } }, generation: generation() }));
      state.pool = pool;
      const project = await onboard(repo, pool);
      await assignPool(project, pool);
      await saveSetup(project, { about: ABOUT, research: [], pool, team: { master: "claude-code", master_tools: true, sub_harnesses: [], min_agents: 0, max_agents: 1 } });
      state.project = project;
      const team = await value("desktop_team_status", { id: project });
      applied("team_live_master", await value("desktop_team_live_master", { id: project, on: true, version: team.settings_version, generation: generation() }));
      const control = await until("live_master_launch", LAUNCH_MS, () => findControl(crazyHome) ?? findControl(userHome));
      const [launch] = await until("launch_record", LAUNCH_MS, () => {
        const launched = lines(control, "launches.jsonl");
        return launched.length > 0 ? launched : null;
      });
      if (!launch.server) throw new Refused("live_master_without_team_server", "the launch named no crazyagent-team server in --mcp-config");
      state.control = control;
      return `project ${project}; live master pid ${launch.pid}, session ${launch.session}`;
    });

    const desktop = async (page) => {
      const me = page ? owner(page, evidence) : null;
      const onPage = () => {
        if (!me) throw new Blocked("chrome_missing: the desktop page needs Google Chrome for DevTools");
        return me;
      };
      if (me) {
        await page.send("Page.enable");
        await page.send("Page.addScriptToEvaluateOnNewDocument", { source: INTERNALS });
        await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/` });
        await me.waitFor("Home with the project", (text) => text.includes(REPO) && !/No projects yet/.test(text));
        await me.capture("home");
      }
      let toast = null;

      await step("1", "an agent's ask is logged at once: the engine log, an owner notification and .crazyagent/questions.md", ["QL-1"], async () => {
        const toastText = `The master: ${ASKS.view.question}`;
        toast = me ? me.waitFor("the question toast", (text) => text.includes(toastText), TOAST_MS).then(() => null, (error) => error) : null;
        const began = Date.now();
        const id = await ask("view");
        const page = join(repo, ".crazyagent", "questions.md");
        const shown = await until("question_logged_everywhere", ASK_SHOWN_MS, async () => {
          const row = question(id);
          const events = await value("desktop_team_events");
          const notice = events.notifications.find((item) => item.project_id === state.project && item.kind === "question" && item.text.includes(ASKS.view.question));
          const text = existsSync(page) ? readFileSync(page, "utf8") : "";
          return row?.status === "pending" && notice && text.includes(`### Question ${id} from the master`) ? { row, notice, text } : null;
        }).catch((failure) => {
          const text = existsSync(page) ? readFileSync(page, "utf8").slice(0, 400) : "no questions.md";
          throw new Refused(failure.code ?? "question_not_logged", `row ${JSON.stringify(question(id))}; ${text}`);
        });
        if (shown.row.asker.split(":")[0] !== "master") throw new Refused("asker_not_master", shown.row.asker);
        if (!shown.text.includes(ASKS.view.question) || !shown.text.includes("1. Postgres (recommended)")) throw new Refused("questions_md_without_question_or_recommendation");
        return `question ${id} logged, notified and on questions.md in ${Date.now() - began} ms`;
      });

      await step("2", "the desktop shows it: the bridge list and counts, a toast, and the Questions tab card with the recommendation", ["QL-1", "QL-2"], async () => {
        const id = needs(state.asked.view, "1");
        const listed = (await value("desktop_questions", { projectId: state.project, after: null })).items.find((item) => item.id === id);
        if (!listed || listed.status !== "pending" || listed.version !== 0 || listed.recommended !== "Postgres") throw new Refused("desktop_questions_mismatch", JSON.stringify(listed ?? null));
        const counted = (await value("desktop_question_counts")).find((item) => item.project_id === state.project);
        if (!counted || counted.pending < 1) throw new Refused("desktop_question_counts_without_project", JSON.stringify(counted ?? null));
        const view = onPage();
        const missed = await toast;
        if (missed) throw new Refused("no_question_toast", missed.message.slice(0, 300));
        await view.follow(`.cards a[href*="/projects/"]`);
        await view.waitFor("the project page", (text) => text.includes(REPO) && /Questions/.test(text));
        await view.click("Questions", "a, button, [role=tab]");
        const card = await view.waitFor("the question card", (text) => text.includes(ASKS.view.question), VIEW_MS);
        await view.capture("questions-pending");
        for (const word of ["Waiting for you", "Recommended", "Postgres", "SQLite", "MySQL", "Answer", "Dismiss"]) {
          if (!card.includes(word)) throw new Refused("question_card_without", word);
        }
        return `listed version ${listed.version}; ${counted.pending} pending; toast and card shown`;
      });

      await step("3", "answer once in the Questions view: the master gets the answer once and a second answer is stale", ["QL-1", "QL-2"], async () => {
        const id = needs(state.asked.view, "1");
        const view = onPage();
        await view.follow(".qs-card .qs-option input[type=radio]");
        await view.click("Answer");
        await view.waitFor("the answered note", (text) => text.includes("Answered. The agent that asked gets your answer once."), VIEW_MS);
        await view.capture("questions-answered");
        const row = question(id);
        if (row?.status !== "answered" || row.answer !== "Postgres") throw new Refused("not_answered", JSON.stringify(row));
        const typed = await deliveredOnce(id, "answered", "Postgres");
        await staleAgain("desktop_answer_question", { id, version: 0, picks: [1], comment: null });
        if (question(id).answer !== "Postgres") throw new Refused("stale_answer_changed_the_record");
        return `delivered once: ${typed.split("\n")[0].slice(0, 120)}`;
      });

      await step("4", "restart the engine: the master keeps its queue and the answer is not delivered again", ["QL-1"], async () => {
        const id = needs(state.asked.view, "1");
        const control = needs(state.control, "0");
        await stop(children.bridge);
        await stop(children.engine);
        restarts += 1;
        await startEngine();
        applied("team_message", await value("desktop_team_message", { id: state.project, text: RESTART_NOTE, generation: generation() }));
        await until("restart_note_typed", DELIVERY_MS * 2, () => typedCount(control, RESTART_NOTE) > 0);
        await pause(QUIET_MS);
        const typed = typedCount(control, delivered(id, "answered"));
        const stored = deliveredRows(id, "answered").length;
        if (typed !== 1 || stored !== 1) throw new Refused("delivered_again_after_restart", `${typed} typed, ${stored} messages`);
        if (question(id)?.status !== "answered") throw new Refused("answer_lost_in_restart", JSON.stringify(question(id)));
        return `${lines(control, "launches.jsonl").length} launches; delivery still once`;
      });

      await step("5", "dismiss in the Questions view: the master is told once and a second dismissal is stale", ["QL-1", "QL-2"], async () => {
        const id = await ask("dismiss");
        const view = onPage();
        await view.waitFor("the second question", (text) => text.includes(ASKS.dismiss.question), VIEW_MS);
        await view.click("Dismiss");
        await view.waitFor("the dismiss confirmation", (text) => text.includes("Dismiss this question?"));
        await view.click("Dismiss", "dialog button");
        await view.waitFor("the dismissed note", (text) => text.includes("Dismissed. The agent that asked is told once."), VIEW_MS);
        await view.capture("questions-dismissed");
        if (question(id)?.status !== "dismissed") throw new Refused("not_dismissed", JSON.stringify(question(id)));
        const typed = await deliveredOnce(id, "dismissed", "No answer comes.");
        await staleAgain("desktop_dismiss_question", { id, version: 0 });
        return `told once: ${typed.split("\n")[0].slice(0, 120)}`;
      });

      await step("6", "answer in the master chat: the master records the owner's message, nothing is delivered back", ["QL-1"], async () => {
        const control = needs(state.control, "0");
        const id = await ask("chat");
        applied("team_message", await value("desktop_team_message", { id: state.project, text: CHAT_ANSWER, generation: generation() }));
        const [message] = await until("owner_message_stored", DELIVERY_MS, () => {
          const found = rows(database, "SELECT id FROM team_messages WHERE project = ? AND author = 'owner' AND text = ?", state.project, CHAT_ANSWER);
          return found.length > 0 ? found : null;
        });
        await until("owner_message_typed", DELIVERY_MS, () => typedCount(control, CHAT_ANSWER) > 0);
        const recorded = await toolCall(control, "record_answer", { question: id, version: 0, owner_turn: Number(message.id) });
        if (recorded.error) throw new Refused("record_answer_refused", recorded.text);
        const row = question(id);
        if (row?.status !== "answered" || row.answered_by !== "chat" || !row.answer?.includes("Stripe")) throw new Refused("chat_answer_not_recorded", JSON.stringify(row));
        const again = await toolCall(control, "record_answer", { question: id, version: 0, owner_turn: Number(message.id) });
        await pause(QUIET_MS);
        const back = typedCount(control, delivered(id, "answered")) + deliveredRows(id, "answered").length;
        if (back !== 0) throw new Refused("chat_answer_delivered_back", `${back} deliveries`);
        if (question(id).answer !== row.answer) throw new Refused("second_record_changed_the_answer");
        return `recorded from owner message ${message.id}; second record ${again.error ? "refused" : "answered"}: ${again.text.slice(0, 80)}`;
      });

      await step("6a", "the master withdraws its question: it leaves the list as withdrawn by the master, a second withdrawal is refused and nothing is sent back", ["S4"], async () => {
        const control = needs(state.control, "0");
        const id = await ask("withdraw");
        const view = onPage();
        await view.waitFor("the question to withdraw", (text) => text.includes(ASKS.withdraw.question), VIEW_MS);
        const withdrawn = await toolCall(control, "withdraw_question", { question: id, version: 0 });
        if (withdrawn.error) throw new Refused("withdraw_refused", withdrawn.text);
        const row = question(id);
        if (row?.status !== "dismissed" || row.closed_by !== "master") throw new Refused("not_withdrawn", JSON.stringify(row));
        const again = await toolCall(control, "withdraw_question", { question: id, version: 0 });
        if (!again.error) throw new Refused("second_withdrawal_taken", again.text);
        await view.waitFor("the withdrawn card", (text) => text.includes(ASKS.withdraw.question) && text.includes("Withdrawn by the master"), VIEW_MS);
        await view.capture("questions-withdrawn");
        await pause(QUIET_MS);
        const back = typedCount(control, delivered(id, "dismissed")) + deliveredRows(id, "dismissed").length;
        if (back !== 0) throw new Refused("withdrawal_delivered_back", `${back} deliveries`);
        return `withdrawn; second withdrawal refused: ${again.text.slice(0, 80)}`;
      });

      await step("6a2", "Dismiss all: two waiting questions leave the list after one confirm, each asker is told once and a second Dismiss all of the same versions is stale", ["S4"], async () => {
        const ids = [await ask("bulkFirst"), await ask("bulkSecond")];
        const view = onPage();
        await view.waitFor("two waiting questions and Dismiss all", (text) => text.includes(ASKS.bulkFirst.question) && text.includes(ASKS.bulkSecond.question) && text.includes("Dismiss all"), VIEW_MS);
        await view.click("Dismiss all");
        await view.waitFor("the Dismiss all confirmation", (text) => text.includes("Dismiss 2 questions?") && text.includes("Keep them"));
        await view.capture("questions-dismiss-all-confirm");
        await view.click("Dismiss all", "dialog button");
        await view.waitFor("the Dismiss all note", (text) => text.includes("Dismissed 2 questions."), VIEW_MS);
        await view.capture("questions-dismissed-all");
        for (const id of ids) {
          if (question(id)?.status !== "dismissed") throw new Refused("not_dismissed_by_all", JSON.stringify(question(id)));
          await deliveredOnce(id, "dismissed", "No answer comes.");
        }
        const again = await value("desktop_dismiss_questions", { items: ids.map((id) => [id, 0]) });
        if (again.settled.length !== 0 || again.stale.length !== 2) throw new Refused("second_dismiss_all_not_stale", JSON.stringify(again));
        const empty = await call("desktop_dismiss_questions", { items: [] });
        if (empty.failure?.code !== "questions_bulk_invalid") throw new Refused("empty_dismiss_all_taken", JSON.stringify(empty));
        return `dismissed ${ids.join(", ")} with one confirm; each told once; a repeat is stale`;
      });

      const popupText = () => page.evaluate(`document.querySelector(${JSON.stringify(POPUP)})?.innerText ?? ""`);
      const projectTab = async (tab) => {
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/projects/${encodeURIComponent(state.project)}/${tab}` });
        await until(`project_tab_${tab}`, VIEW_MS, async () => (await page.evaluate("location.hash")).endsWith(`/${tab}`)
          && await page.evaluate(`document.getElementById("pj-tab-panel")?.getAttribute("aria-labelledby") === ${JSON.stringify(`tab-${tab}`)}`));
      };
      const chooseShown = async (label) => {
        const view = onPage();
        await page.send("Page.navigate", { url: `http://127.0.0.1:${site.port}/#/settings/profile` });
        await view.click(label, `[role=radiogroup][aria-label="Questions show"] button`);
        await until("questions_show_saved", VIEW_MS, async () => (await page.evaluate(`document.querySelector('[role=radiogroup][aria-label="Questions show"] button[aria-checked="true"]')?.textContent?.trim() ?? ""`)) === label);
        for (const type of ["keyDown", "keyUp"]) await page.send("Input.dispatchKeyEvent", { type, key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 });
        await until("profile_dialog_closed", VIEW_MS, async () => page.evaluate("document.querySelector('dialog[open]') === null"));
      };
      const noneWaiting = () => {
        const waiting = rows(database, "SELECT id, question FROM questions_log WHERE project = ? AND status = 'pending'", state.project);
        if (waiting.length > 0) throw new Refused("other_questions_waiting", JSON.stringify(waiting));
      };

      await step("6b", "Questions show, Pop-up on Conversation (the default): a master question pops up on the Conversation tab, an answer there is the Questions tab's answer and the master gets it once", ["QL-1", "OB-9"], async () => {
        const view = onPage();
        noneWaiting();
        await projectTab("conversation");
        const field = await page.evaluate(`(() => { const field = document.querySelector("#pj-tab-panel .xterm-helper-textarea, #pj-tab-panel textarea"); field?.focus(); return field && document.activeElement === field ? field.className || field.tagName : null; })()`);
        const id = await ask("popup");
        await until("popup_shows_question", VIEW_MS, async () => (await popupText()).includes(ASKS.popup.question));
        await view.capture("questions-popup-conversation");
        if (field !== null) {
          const kept = await page.evaluate(`document.activeElement?.closest("#pj-tab-panel") !== null && document.activeElement.matches(".xterm-helper-textarea, textarea")`);
          if (!kept) throw new Refused("popup_took_the_keyboard", `${field} lost it to ${await page.evaluate("document.activeElement?.outerHTML?.slice(0, 120) ?? null")}`);
        }
        const over = await page.evaluate(`(() => { const card = document.querySelector(${JSON.stringify(POPUP)}).getBoundingClientRect(); const panel = document.getElementById("pj-tab-panel").getBoundingClientRect(); return card.bottom <= panel.top + 1 ? null : { card: card.bottom, panel: panel.top }; })()`);
        if (over !== null) throw new Refused("popup_over_the_tab", JSON.stringify(over));
        for (const word of ["A question for you", "Recommended", "Answer", "Dismiss", "Hide", "Questions tab"]) {
          if (!(await popupText()).includes(word)) throw new Refused("popup_without", word);
        }
        await view.follow(`${POPUP} .qs-option input[type=radio]`);
        await view.click("Answer", `${POPUP} button`);
        await until("popup_answer_recorded", VIEW_MS, () => question(id)?.status === "answered");
        if (question(id).answer !== "Yes") throw new Refused("popup_answer_mismatch", JSON.stringify(question(id)));
        await until("popup_closes", VIEW_MS, async () => !(await popupText()).includes(ASKS.popup.question));
        const typed = await deliveredOnce(id, "answered", "Yes");
        await projectTab("questions");
        const card = await until("answered_in_questions_tab", VIEW_MS, async () => {
          const text = await page.evaluate(`[...document.querySelectorAll(".qs-card")].map((item) => item.innerText).find((text) => text.includes(${JSON.stringify(ASKS.popup.question)})) ?? ""`);
          return text.includes("Answered") && !text.includes("Waiting for you") ? text : null;
        });
        await view.capture("questions-popup-answered-in-tab");
        return `pop-up above the tab, ${field === null ? "no tab field held the keyboard" : `keyboard kept in ${field}`}; answered in the pop-up, ${card.includes("Answer: Yes") ? "shown answered" : "answered"} in the Questions tab; delivered once: ${typed.split("\n")[0].slice(0, 80)}`;
      });

      await step("6c", "Questions show, Pop-up on every tab: a master question pops up on Overview, and Dismiss there is the Questions tab's dismissal", ["QL-1", "OB-9"], async () => {
        const view = onPage();
        noneWaiting();
        await chooseShown("Pop-up on every tab");
        await projectTab("overview");
        const id = await ask("every");
        await until("popup_on_overview", VIEW_MS, async () => (await popupText()).includes(ASKS.every.question));
        await view.capture("questions-popup-overview");
        await view.click("Dismiss", `${POPUP} button`);
        await view.waitFor("the dismiss confirmation", (text) => text.includes("Dismiss this question?"));
        await view.click("Dismiss", "dialog button");
        await until("popup_dismiss_recorded", VIEW_MS, () => question(id)?.status === "dismissed");
        await until("popup_closes_after_dismiss", VIEW_MS, async () => !(await popupText()).includes(ASKS.every.question));
        const typed = await deliveredOnce(id, "dismissed", "No answer comes.");
        return `dismissed in the pop-up on Overview; told once: ${typed.split("\n")[0].slice(0, 80)}`;
      });

      await step("6d", "Questions show, Questions tab only: no pop-up on the Conversation tab, and the question waits in the Questions tab", ["QL-1", "OB-9"], async () => {
        const view = onPage();
        noneWaiting();
        await chooseShown("Questions tab only");
        await projectTab("conversation");
        const id = await ask("tabOnly");
        await until("listed_for_the_tab", VIEW_MS, async () => (await value("desktop_questions", { projectId: state.project, after: null })).items.some((item) => item.id === id && item.status === "pending"));
        await pause(POPUP_QUIET_MS);
        const dock = await page.evaluate(`document.querySelector(".qp-dock")?.outerHTML ?? null`);
        if (dock !== null) throw new Refused("popup_with_questions_tab_only", dock.slice(0, 200));
        await view.capture("questions-tab-only-conversation");
        await projectTab("questions");
        await until("waiting_in_questions_tab", VIEW_MS, async () => (await page.evaluate(`[...document.querySelectorAll(".qs-card")].some((item) => item.innerText.includes(${JSON.stringify(ASKS.tabOnly.question)}) && item.innerText.includes("Waiting for you"))`)));
        await view.follow(".qs-card .qs-option input[type=radio]");
        await view.click("Answer");
        await until("tab_only_answer_recorded", VIEW_MS, () => question(id)?.status === "answered");
        await chooseShown("Pop-up on Conversation");
        return `no pop-up for ${POPUP_QUIET_MS} ms on Conversation; answered in the Questions tab`;
      });
    };

    let paged = false;
    if (state.control) {
      const dist = join(root, "dist");
      await build({ root: appRoot, configLoader: "runner", logLevel: "silent", build: { outDir: dist, emptyOutDir: true } });
      const log = { calls: [], answers: new Map(), trail: [], unrouted: new Set(), invalid: [], dropped: [], picked: 0, closing: false };
      site = await serveApp(dist, bridge, [], log);
      try {
        await withPage(async (page) => {
          paged = true;
          await desktop(page);
        });
      } catch (failure) {
        if (paged) {
          results.push({ id: "page", title: "the desktop page opens Home with the project", needs: [], result: "FAIL", code: `script: ${failure.message}`, ms: 0 });
          console.log(`FAIL page the desktop page opens Home with the project [${failure.message}]`);
        } else {
          console.log(`no desktop page: ${failure.message}`);
          await desktop(null);
        }
      } finally {
        log.closing = true;
        writeFileSync(join(evidence, "page-calls.json"), JSON.stringify(log.trail, null, 1));
      }
    } else {
      await desktop(null);
    }

    await step("7", "the TUI shows the footer badge on every screen and the Questions page, and an answer there reaches the master once", ["QL-1", "QL-3"], async () => {
      if (!tmux) throw new Blocked("tmux_missing");
      const id = await ask("tui");
      const pending = Number(rows(database, "SELECT COUNT(*) AS n FROM questions_log WHERE status = 'pending'")[0].n);
      const pty = tuiPty(tmux, join(root, "tui.sock"), `'${engineBinary}' tui '${socket}'`, { ...env, TERM: "xterm-256color" });
      try {
        let home = null;
        for (let pressed = 0; pressed <= TABS_MAX && !home; pressed += 1) {
          const now = await pty.until("home_or_first_run", (screen) => screen.includes(REPO) || screen.includes("c Continue without"));
          if (now.includes(REPO) && !now.includes("c Continue without")) home = now;
          else pty.keys("c");
          await pause(POLL_MS);
        }
        if (!home) throw new Refused("tui_never_reached_home");
        const atHome = await pty.until("badge_on_home", (screen) => badge(screen) === pending).catch(() => {
          throw new Refused("tui_home_without_badge", `wanted ${pending} questions: ${pty.screen().split("\n").slice(-3).join(" | ")}`);
        });
        for (let pressed = 0; pressed < TABS_MAX && !pty.screen().includes("Open"); pressed += 1) {
          pty.keys("Tab");
          await pause(POLL_MS);
        }
        pty.keys("Enter");
        await pty.until("project_screen", (screen) => screen.includes(REPO) && screen.includes("Tabs"));
        for (let pressed = 0; tabShown(pty.screen(), REPO) !== "Questions"; pressed += 1) {
          if (pressed >= TABS_MAX) throw new Refused("tui_without_questions_tab", pty.screen().split("\n")[0]);
          const before = tabShown(pty.screen(), REPO);
          pty.bytes(SHIFT_RIGHT);
          await pty.until("next_tab", (screen) => tabShown(screen, REPO) !== before);
        }
        const listed = await pty.until("question_on_page", (screen) => screen.includes(ASKS.tui.question));
        if (badge(listed) !== pending) throw new Refused("tui_project_screen_without_badge", `wanted ${pending}`);
        const key = hintKey(listed, "Answer");
        if (!key) throw new Refused("tui_questions_page_without_answer_hint", listed.split("\n").slice(-3).join(" | "));
        pty.keys(key);
        await pause(POLL_MS * 2);
        pty.text(TUI_ANSWER);
        pty.keys("Enter");
        await until("tui_answer_recorded", VIEW_MS, () => question(id)?.status === "answered");
        if (question(id).answer !== TUI_ANSWER) throw new Refused("tui_answer_mismatch", JSON.stringify(question(id)));
        await deliveredOnce(id, "answered", TUI_ANSWER);
        writeFileSync(join(evidence, "tui-home.txt"), atHome);
        writeFileSync(join(evidence, "tui-questions.txt"), listed);
        return `badge ${pending} on Home and the project screen; answered with key ${key}; delivered once`;
      } finally {
        pty.close();
      }
    });

    await step("8", "a permission prompt on the master's screen is never logged, while a real question box is logged once as asked in the terminal", ["QL-1", "QL-4"], async () => {
      const control = needs(state.control, "0");
      const terminal = () => rows(database, "SELECT id, question, status FROM questions_log WHERE project = ? AND asker LIKE 'terminal:%'", state.project);
      const before = terminal().length;
      show(control, CLEAR + PERMISSION);
      await pause(SCAN_MS);
      const afterPrompt = terminal();
      if (afterPrompt.length !== before) throw new Refused("permission_prompt_logged", JSON.stringify(afterPrompt.slice(before)));
      show(control, CLEAR + BOX);
      const logged = await until("terminal_box_logged", SCAN_MS, () => {
        const found = terminal().filter((row) => row.question.includes(BOX_QUESTION));
        return found.length > 0 ? found : null;
      }).catch(() => {
        throw new Refused("terminal_box_not_logged", "the screen watch logged no question box, so the permission check proves nothing (needs QL-4)");
      });
      await pause(SCAN_MS);
      const once = terminal().filter((row) => row.question.includes(BOX_QUESTION));
      show(control, `${CLEAR}Done.\n`);
      if (once.length !== 1) throw new Refused("terminal_box_logged_more_than_once", `${once.length} rows`);
      if (terminal().some((row) => row.question.includes("Do you want to proceed?"))) throw new Refused("permission_prompt_logged");
      return `permission prompt unlogged over ${SCAN_MS} ms; box logged once as question ${logged[0].id}`;
    });

    manual("9", "a macOS system notification while the window is in the background", ["QL-1", "QL-5"],
      "Build and open the desktop app with an approved profile, put it in the background, have the master ask a question, then screenshot the macOS notification and click it: the app must open the project's Questions tab.");

    await step("10a", "an imported project with the roadmap toggle on queues the roadmap planning job", ["RM-1"], async () => {
      const project = await onboard(imported, needs(state.pool, "0"));
      await assignPool(project, state.pool);
      await saveSetup(project, { about: ABOUT, research: ["roadmap_planning"], pool: state.pool, team: null });
      const job = await until("imported_planning_job", JOB_MS, async () => (await value("desktop_research_jobs", { id: project })).jobs.find((item) => item.kind === "roadmap_planning") ?? null);
      return `planning job ${job.id} ${job.status}`;
    });

    await step("10b", "a new project with the toggle on queues the roadmap planning job", ["RM-1"], async () => {
      writeFileSync(join(bin, SILENT), "", { mode: 0o600 });
      const receipt = applied("setup_project", await value("desktop_setup_project", { spec: { name: "qf-roadmap-on", repositories: [] }, setup: { about: ABOUT, tech_stack: [], platforms: ["website"], research: ["roadmap_planning"], pool: needs(state.pool, "0"), team: null, documents: [] }, generation: generation() }));
      await assignPool(receipt.project_id, state.pool);
      const job = await until("new_planning_job", JOB_MS, async () => (await value("desktop_research_jobs", { id: receipt.project_id })).jobs.find((item) => item.kind === "roadmap_planning") ?? null);
      return `project ${receipt.project_id}; planning job ${job.id} ${job.status}`;
    });

    await step("10c", "a new project with the toggle off gets no plan until Plan roadmap now, and a second press is planning_busy", ["RM-1", "RM-2"], async () => {
      writeFileSync(join(bin, SILENT), "", { mode: 0o600 });
      const receipt = applied("setup_project", await value("desktop_setup_project", { spec: { name: "qf-roadmap-off", repositories: [] }, setup: { about: ABOUT, tech_stack: [], platforms: ["website"], research: [], pool: needs(state.pool, "0"), team: null, documents: [] }, generation: generation() }));
      const project = receipt.project_id;
      await assignPool(project, state.pool);
      await pause(JOB_MS / 3);
      const planning = async () => (await value("desktop_research_jobs", { id: project })).jobs.filter((item) => item.kind === "roadmap_planning");
      if ((await planning()).length !== 0) throw new Refused("toggle_off_queued_planning");
      const planned = (await value("desktop_project_context", { id: project })).items.filter((item) => /(^|\/)(roadmap|release-process)\.md$/.test(item.path));
      if (planned.length !== 0) throw new Refused("toggle_off_has_plan_documents", planned.map((item) => item.path).join(", "));
      applied("plan_roadmap", await value("desktop_plan_roadmap", { id: project, generation: generation() }));
      const job = await until("plan_now_job", JOB_MS, async () => (await planning())[0] ?? null);
      const second = await call("desktop_plan_roadmap", { id: project, generation: generation() });
      if ("unknown" in second) throw new Blocked("unrouted:desktop_plan_roadmap");
      const code = second.failure?.code ?? second.value?.rejection ?? (second.value?.operation_id ? recordedCode(database, second.value.operation_id) : null) ?? second.value?.outcome;
      const recorded = second.value?.operation_id ? recordedCode(database, second.value.operation_id) : null;
      if (code !== "planning_busy" && recorded !== "planning_busy") throw new Refused("second_plan_not_busy", `${code} (engine: ${recorded})`);
      if ((await planning()).length !== 1) throw new Refused("second_plan_queued_another_job");
      return `no plan until pressed; job ${job.id} ${job.status}; second press planning_busy`;
    });

    const planDocuments = async (via, project, next) => {
      const job = await until("real_planning_done", PLAN_REAL_MS, async () => {
        const found = (await value("desktop_research_jobs", { id: project }, via)).jobs.find((item) => item.kind === "roadmap_planning");
        return found && ["done", "failed", "cancelled"].includes(found.status) ? found : null;
      });
      if (job.status !== "done") throw new Refused(`planning_${job.status}`, job.problem ?? job.error ?? "");
      const context = await value("desktop_project_context", { id: project }, via);
      const read = async (name) => {
        const item = context.items.find((entry) => entry.path.endsWith(name));
        if (!item) throw new Refused("plan_document_missing", name);
        const chunk = await value("desktop_project_context_item", { id: project, path: item.path, offset: 0 }, via);
        return chunk.text ?? "";
      };
      const roadmap = await read("roadmap.md");
      const release = await read("release-process.md");
      if (!roadmap.startsWith("# Roadmap") || !roadmap.includes("Written by the CrazyAgent engine")) throw new Refused("roadmap_md_shape", roadmap.slice(0, 120));
      if (!release.startsWith("# Release process") || !release.includes("Written by the CrazyAgent engine")) throw new Refused("release_process_md_shape", release.slice(0, 120));
      const asked = (await value("desktop_questions", { projectId: project, after: null }, via)).items.filter((item) => item.asker === "engine:planning");
      if (asked.length > OPEN_POINTS_MAX) throw new Refused("too_many_open_points", String(asked.length));
      applied("remove_project", await value("desktop_remove_project", { id: project, generation: next() }, via), real.database);
      return `roadmap.md ${roadmap.length} B, release-process.md ${release.length} B, ${asked.length} open points asked`;
    };
    const realBridge = async () => {
      if (!real) throw new Blocked("needs_real_signed_in_profile: set QUESTIONS_FLOW_REAL_RUNTIME and QUESTIONS_FLOW_REAL_POOL (an approved profile; one planning job spends provider tokens)");
      if (real.problem) throw new Blocked(real.problem);
      if (!state.real) {
        const started = await start(bridgeBinary, [real.socket], env, evidence, "bridge-real", BRIDGE_READY);
        children.real = started.child;
        state.real = { port: Number(started.match[1]), token: started.match[2], generation: 0 };
        if ((await value("desktop_connect", {}, state.real)).connected !== true) throw new Blocked("real_engine_not_reachable");
      }
      return state.real;
    };

    await step("10d", "a new project with the toggle on gets context/roadmap.md, context/release-process.md and its open points as questions (real profile)", ["RM-1", "QL-1", "QL-2"], async () => {
      const via = await realBridge();
      const next = () => (via.generation += 1);
      const receipt = applied("setup_project", await value("desktop_setup_project", { spec: { name: `qf-roadmap-${Date.now()}`, repositories: [] }, setup: { about: ABOUT, tech_stack: [], platforms: ["website"], research: ["roadmap_planning"], pool: real.pool, team: null, documents: [] }, generation: next() }, via), real.database);
      return planDocuments(via, receipt.project_id, next);
    });

    await step("10e", "a cloned project with the toggle on gets the same plan documents and open points (real profile and approved repository)", ["RM-1", "QL-1", "QL-2"], async () => {
      const via = await realBridge();
      if (!real.clone) throw new Blocked("needs_approved_clone: set QUESTIONS_FLOW_CLONE to an approved owner/name repository the real engine's GitHub sign-in can clone");
      const next = () => (via.generation += 1);
      const cloned = await value("desktop_clone_repo", { fullName: real.clone }, via);
      const project = await onboard(cloned.path, real.pool, via, next, real.database, PLAN_REAL_MS);
      await saveSetup(project, { about: ABOUT, research: ["roadmap_planning"], pool: real.pool }, via, next, real.database);
      return `${await planDocuments(via, project, next)}; the clone stays at ${cloned.path}`;
    });
  } catch (failure) {
    console.error(`questions-flow stopped: ${failure.stack ?? failure.message}`);
    results.push({ id: "-", title: "run", needs: [], result: "FAIL", code: `script: ${failure.message}`, ms: 0 });
  } finally {
    rmSync(join(bin, SILENT), { force: true });
    writeFileSync(join(evidence, "bridge-trail.json"), JSON.stringify(trail, null, 1));
    site?.server.close();
    await stop(children.real);
    await stop(children.bridge);
    await stop(children.engine);
    if (tmux) spawnSync(tmux, ["-S", join(crazyHome, "terminal", "tmux.sock"), "kill-server"], { stdio: "ignore", env: { PATH: "/usr/bin:/bin" } });
  }

  console.log("\nsummary");
  for (const row of results) console.log(`${row.result.padEnd(7)} ${row.id.padEnd(4)} ${row.title} [${row.code}]`);
  const slices = [...new Set(results.filter((row) => row.result === "FAIL" || row.result === "BLOCKED").flatMap((row) => row.needs))].sort();
  if (slices.length > 0) console.log(`\nslices the failed or blocked steps need:\n${slices.map((slice) => `  ${slice}: ${SLICES[slice]}`).join("\n")}`);
  if (keep) console.log(`evidence kept in ${evidence}`);
  else rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  const failed = results.some((row) => row.result === "FAIL");
  const blocked = results.some((row) => row.result === "BLOCKED");
  process.exit(failed ? 1 : blocked ? 3 : 0);
}

await main();
