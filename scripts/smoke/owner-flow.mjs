import { spawn, spawnSync } from "node:child_process";
import { chmodSync, createWriteStream, existsSync, lstatSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, readlinkSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync, writeSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { constants } from "node:os";
import { isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";

const READY_MS = 60000;
const AUTH_MS = 60000;
const TURN_MS = 90000;
const REAL_TURN_MS = 300000;
const SETUP_MS = 180000;
const TEAM_MS = 90000;
const POLL_MS = 250;
const MERGE_BASE_KEYS = 50;
const LINKED_WORKTREES = 41;
const TREE_FOLDERS = 16;
const TREE_SUBFOLDERS = 8;
const TREE_FILES = 16;
const LARGE_BYTES = 50 * 1024 * 1024;
const LFS_POINTER = "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";
const REPO = "owner-shaped";
const ROOT_PREFIX = "ca-of-";
const ROOT_NAME = /^ca-of-[A-Za-z0-9]{6}$/;
const ROOT_MARK = "owner-flow-root";
const ROOT_QUIET_MS = 15 * 60 * 1000;
const ROOT_ACTIVITY = [".", "h/engine.db", "h/engine.db-wal", `repos/${REPO}/.git`, `repos/${REPO}/.git/index`];
const STOP_SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];
const STAND_IN_SIGN = "owner-flow stand-in";
const ACTIVE_AUTH = new Set(["starting", "awaiting_owner", "checking_account", "cancelling"]);
const ACTIVE_TURN = new Set(["queued", "prepared", "delivering", "running"]);
const NOT_ADMITTED = new Set(["readiness_unknown", "readiness_stale", "invalid_authentication", "connection_changed", "scope_revoked", "repository_missing", "unknown"]);
const TMUX_SEARCH = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
const SILENT_MARK = "owner-flow-silent";
const SILENT_TURN_S = 300;
const BRIDGE_READY = /^ready (\d+) ([0-9a-f]{64})$/m;
const STAND_IN = `#!/bin/sh
marker="$CLAUDE_CONFIG_DIR/.owner-flow-signed-in"
keychain="$CLAUDE_CONFIG_DIR/Library/Keychains/login.keychain-db"
service="Claude Code-credentials-$(printf %s "$CLAUDE_CONFIG_DIR" | /usr/bin/shasum -a 256 | /usr/bin/cut -c1-8)"
if [ "$1" != "auth" ]; then
  case " $* " in *" --session-id "*|*" --print "*) [ -f "\${0%/*}/${SILENT_MARK}" ] && exec /bin/sleep ${SILENT_TURN_S} ;; esac
  echo "${STAND_IN_SIGN}: this claude only answers auth commands, it cannot run a turn" >&2
  exit 64
fi
case "$2" in
  login)
    expires=$(( ( $(/bin/date +%s) + 28800 ) * 1000 ))
    record=$(printf '{"claudeAiOauth":{"accessToken":"owner-flow-stand-in","refreshToken":"owner-flow-stand-in","expiresAt":%s,"scopes":["user:inference"]}}' "$expires")
    if [ -x /usr/bin/security ]; then
      hex=$(printf %s "$record" | /usr/bin/od -An -tx1 | /usr/bin/tr -d ' \\n')
      printf 'add-generic-password -U -a owner-flow -s "%s" -X %s "%s"\\n' "$service" "$hex" "$keychain" | /usr/bin/security -i >/dev/null || exit 1
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

/** Reads the approved real-profile run: the data root of a running engine where a Claude account
 * is already signed in, and the project whose assigned pool holds it. Paths and ids only, never a
 * secret; the smoke only sends master turns there. Input: none. Output: null without either
 * variable, the problem when the run cannot start, else the engine's socket, database and project. */
function realProfile() {
  const root = process.env.CRAZYAGENT_MANAGED_RUNTIME;
  const project = process.env.CRAZYAGENT_MANAGED_CLAUDE_PROJECT;
  if (!root && !project) return null;
  if (!root || !project) return { problem: "real_profile_incomplete: set CRAZYAGENT_MANAGED_RUNTIME and CRAZYAGENT_MANAGED_CLAUDE_PROJECT" };
  if (!isAbsolute(root) || !existsSync(join(root, "engine.db"))) return { problem: "real_profile_runtime_without_engine_db" };
  return { socket: join(root, "engine.sock"), database: join(root, "engine.db"), project, bridge: null, turnMs: REAL_TURN_MS };
}

/** Runs git in a folder and stops the run when it fails.
 * Input: the folder and the git arguments. Output: git's standard output. */
function git(cwd, ...args) {
  const run = spawnSync("git", args, { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.SMOKE_GIT_HOME, GIT_CONFIG_NOSYSTEM: "1", LANG: "C" } });
  if (run.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${run.stderr.trim()}`);
  return run.stdout;
}

/** Builds a repository shaped like the owner's: 50 vscode merge-base branch keys, per-worktree
 * config on, 41 linked worktrees (one with a codex config.worktree), then a real-size commit
 * (2,048 files in nested folders, a CLAUDE.md symlink to AGENTS.md, a 50 MiB file and an LFS
 * pointer) and dirty files. Input: the repository folder and the linked worktrees' folder.
 * Output: none. */
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
  for (let folder = 0; folder < TREE_FOLDERS; folder += 1) {
    for (let sub = 0; sub < TREE_SUBFOLDERS; sub += 1) {
      const path = join(repo, "src", `pkg-${folder}`, `mod-${sub}`);
      mkdirSync(path, { recursive: true });
      for (let file = 0; file < TREE_FILES; file += 1) writeFileSync(join(path, `file-${file}.js`), `export const id = "${folder}-${sub}-${file}";\n`);
    }
  }
  writeFileSync(join(repo, "AGENTS.md"), "# Agent rules\n");
  symlinkSync("AGENTS.md", join(repo, "CLAUDE.md"));
  mkdirSync(join(repo, "assets"));
  const large = Buffer.alloc(LARGE_BYTES);
  for (let index = 0; index < LARGE_BYTES; index += 1) large[index] = (index * 31) % 251;
  writeFileSync(join(repo, "assets", "model.bin"), large);
  writeFileSync(join(repo, ".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text\n");
  writeFileSync(join(repo, "assets", "cover.psd"), LFS_POINTER);
  git(repo, "add", ".");
  git(repo, "commit", "-q", "-m", "real size");
  writeFileSync(join(repo, "app.js"), "export const answer = 43;\n");
  writeFileSync(join(repo, "notes.txt"), "work in progress\n");
}

/** Checks that the master's Ready lease holds the whole commit: Git sees no change or missing
 * file, CLAUDE.md is the in-tree symlink and the large file has every byte. Input: the engine
 * database and the repository. Output: a summary, or a Refused naming what differs. */
function leaseCopy(database, repo) {
  const db = new DatabaseSync(database, { readOnly: true });
  let lease;
  try {
    lease = db.prepare("SELECT body FROM worktree_leases").all().map((row) => JSON.parse(row.body).lease).find((item) => item.repository === repo && item.state === "Ready");
  } finally {
    db.close();
  }
  if (!lease) throw new Refused("lease_not_ready");
  const changed = git(lease.path, "--no-optional-locks", "status", "--porcelain");
  if (changed !== "") throw new Refused("lease_differs_from_commit", changed.slice(0, 200));
  if (readlinkSync(join(lease.path, "CLAUDE.md")) !== "AGENTS.md") throw new Refused("lease_symlink_differs");
  if (statSync(join(lease.path, "assets", "model.bin")).size !== LARGE_BYTES) throw new Refused("lease_large_file_differs");
  if (readFileSync(join(lease.path, "assets", "cover.psd"), "utf8") !== LFS_POINTER) throw new Refused("lease_lfs_pointer_differs");
  const tracked = git(lease.path, "ls-files", "-z").split("\0").filter(Boolean).length;
  return `lease holds ${tracked} tracked files, the CLAUDE.md symlink and the 50 MiB file`;
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

/** Marks a temp root as this smoke's and removes it when the process ends: a normal exit, a thrown
 * failure, or SIGINT, SIGTERM or SIGHUP once the engine and bridges have stopped. The marker stays
 * open for the whole run, so --clean sees the root in use. Input: the root and whether
 * SMOKE_EVIDENCE keeps it. Output: none. */
function claimRoot(root, keep) {
  writeSync(openSync(join(root, ROOT_MARK), "wx"), keep ? "kept\n" : "temporary\n");
  process.once("exit", () => {
    for (const child of children) child.kill("SIGTERM");
    if (!keep) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  });
  for (const signal of STOP_SIGNALS) {
    process.once(signal, async () => {
      console.log(`\n${signal}: stopping the engine and bridges`);
      await Promise.all([...children].map(stop));
      process.exit(128 + constants.signals[signal]);
    });
  }
}

/** Reads a regular file inside a temp root. Input: the path.
 * Output: its text, or "" when it is missing or not a regular file. */
function rootText(path) {
  return lstatSync(path, { throwIfNoEntry: false })?.isFile() ? readFileSync(path, "utf8") : "";
}

/** Says why --clean keeps a ca-of-* folder: gone, not this user's folder, held open by a live
 * process, without this smoke's stand-in claude, kept for SMOKE_EVIDENCE, or changed within 15
 * minutes. Input: the folder, the pid holding a file in it (if any) and the time now.
 * Output: the reason, or null when the folder may be removed. */
function keepReason(root, holder, now) {
  const folder = lstatSync(root, { throwIfNoEntry: false });
  if (!folder) return "already gone";
  if (!folder.isDirectory() || folder.uid !== process.getuid()) return "not this smoke's";
  if (holder) return `in use by pid ${holder}`;
  if (!rootText(join(root, "bin", "claude")).includes(STAND_IN_SIGN)) return "not this smoke's";
  if (rootText(join(root, ROOT_MARK)) === "kept\n") return "kept for SMOKE_EVIDENCE";
  const changed = Math.max(...ROOT_ACTIVITY.map((path) => lstatSync(join(root, path), { throwIfNoEntry: false })?.mtimeMs ?? 0));
  if (now - changed < ROOT_QUIET_MS) return `changed ${Math.round((now - changed) / 60000)} min ago`;
  return null;
}

/** Removes the temp roots that earlier smoke runs left in /tmp, keeping every folder keepReason
 * names. lsof lists the open files of this user's processes once, so a running smoke's root is
 * never touched. Input: whether to list only. Output: one line per ca-of-* folder and the counts;
 * exit 2 when lsof cannot list, 1 when a root could not be removed. */
function clean(dryRun) {
  const base = realpathSync("/tmp");
  const listed = spawnSync("lsof", ["-n", "-P", "-w", "-u", String(process.getuid()), "-F", "pn"], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (listed.status !== 0 || !listed.stdout) {
    console.error(`BLOCKED: lsof could not list open files (${listed.error?.code ?? listed.status}), so no root is known to be unused`);
    process.exitCode = 2;
    return;
  }
  const holders = new Map();
  let pid = "";
  for (const line of listed.stdout.split("\n")) {
    if (line.startsWith("p")) pid = line.slice(1);
    else if (line.startsWith(`n${base}/${ROOT_PREFIX}`)) {
      const name = line.slice(base.length + 2).split("/")[0];
      if (!holders.has(name)) holders.set(name, pid);
    }
  }
  const counts = { removed: 0, kept: 0, failed: 0 };
  const now = Date.now();
  for (const name of readdirSync(base).filter((entry) => ROOT_NAME.test(entry)).sort()) {
    const root = join(base, name);
    const reason = keepReason(root, holders.get(name), now);
    if (reason) {
      counts.kept += 1;
      console.log(`kept ${root} (${reason})`);
      continue;
    }
    try {
      if (!dryRun) rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
      counts.removed += 1;
      console.log(`${dryRun ? "would remove" : "removed"} ${root}`);
    } catch (error) {
      counts.failed += 1;
      console.log(`could not remove ${root}: ${error.code ?? error.message}`);
    }
  }
  console.log(`\n${counts.removed} ${dryRun ? "would be removed" : "removed"}, ${counts.kept} kept, ${counts.failed} could not be removed`);
  if (counts.failed) process.exitCode = 1;
}

/** Drives the installed-style engine through the owner's path with the desktop bridge and prints
 * PASS, FAIL or BLOCKED with the engine code per step. Send mirrors the app (stop an uncertain turn
 * except in 3b; on master_source_conflict end, reopen and resend once). A master step whose harness
 * never replied is BLOCKED; with a real profile (realProfile) 3 and 3b pass only on its stored reply.
 * Input: environment paths. Output: exit 1 on any FAIL, 3 when only BLOCKED steps remain. */
async function main() {
  const engineBinary = required("CRAZYAGENT_TEST_NATIVE_AUTH_HOST", "crazyagent binary (the engine and its native-auth host)");
  const bridgeBinary = required("SMOKE_BRIDGE", "desktop-e2e-bridge binary (cargo build -p crazyagent-desktop --bin desktop-e2e-bridge --features e2e-bridge)");
  const real = realProfile();
  process.umask(0o077);
  const root = realpathSync(mkdtempSync(join(realpathSync("/tmp"), ROOT_PREFIX)));
  const evidence = process.env.SMOKE_EVIDENCE && isAbsolute(process.env.SMOKE_EVIDENCE) ? process.env.SMOKE_EVIDENCE : join(root, "evidence");
  const keep = Boolean(process.env.SMOKE_EVIDENCE) && evidence === join(root, "evidence");
  claimRoot(root, keep);
  console.log(`temp root ${root} (${keep ? "kept for SMOKE_EVIDENCE" : "removed on exit; SMOKE_EVIDENCE=1 keeps it"})`);
  const crazyHome = join(root, "h");
  const userHome = join(root, "u");
  const bin = join(root, "bin");
  const repo = join(root, "repos", REPO);
  const linked = join(root, "repos", `${REPO}-worktrees`);
  const projects = join(root, "projects");
  for (const folder of [crazyHome, userHome, bin, repo, linked, projects, evidence]) mkdirSync(folder, { recursive: true, mode: 0o700 });
  process.env.SMOKE_GIT_HOME = userHome;
  writeFileSync(join(bin, "claude"), STAND_IN, { mode: 0o700 });
  chmodSync(join(bin, "claude"), 0o700);
  ownerRepo(repo, linked);

  const env = { PATH: `${bin}:/usr/bin:/bin`, HOME: userHome, CRAZYAGENT_HOME: crazyHome, TMPDIR: root, LANG: "en_US.UTF-8" };
  const trail = [];
  const results = [];
  const state = { generation: 0, accounts: [], pool: null, project: null, session: null };
  let engine;
  let bridgeProcess;
  let realBridgeProcess;
  let bridge;
  const startEngine = async () => {
    ({ child: engine } = await start(engineBinary, ["engine"], env, evidence, "engine", /^ready$/m));
  };
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
  const applied = (cmd, receipt, database = join(crazyHome, "engine.db")) => {
    if (receipt?.outcome === "applied" && !receipt.pending) return receipt;
    const code = receipt?.rejection ?? (receipt?.pending ? "pending" : receipt?.outcome ?? "no_receipt");
    const recorded = receipt?.operation_id ? recordedCode(database, receipt.operation_id) : null;
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
  const local = { get bridge() { return bridge; }, get project() { return needs(state.project, "1"); }, database: join(crazyHome, "engine.db"), turnMs: TURN_MS };
  const submit = async (text, release, at = local) => {
    const ask = (cmd, args) => value(cmd, args, at.bridge);
    const done = (cmd, receipt) => applied(cmd, receipt, at.database);
    const opened = await ask("desktop_open_project", { id: at.project });
    let master = await ask("desktop_master_state", { ticket: opened.ticket });
    if (!master.session || master.session.phase !== "configured") {
      done("open_master", await ask("desktop_open_master", { ticket: opened.ticket, generation: generation() }));
      master = await ask("desktop_master_state", { ticket: opened.ticket });
      if (!master.session) throw new Refused("master_session_missing");
    }
    let sessionId = master.session.session_id;
    const current = master.session.current_turn;
    if (release && current) {
      const held = (await ask("desktop_master_turns", { ticket: opened.ticket, sessionId, cursor: null })).page.turns.find((item) => item.id === current);
      if (held?.phase === "uncertain") {
        try {
          done("cancel_master_turn", await ask("desktop_cancel_master_turn", { ticket: opened.ticket, session: sessionId, turn: current, generation: generation() }));
        } catch (failure) {
          throw failure instanceof Refused ? new Refused(`release_uncertain_turn:${failure.code}`) : failure;
        }
      }
    }
    let receipt = await ask("desktop_submit_master", { ticket: opened.ticket, sessionId, text, generation: generation() });
    let recovered = "";
    if (receipt.outcome === "rejected" && receipt.rejection === "master_source_conflict") {
      done("close_master", await ask("desktop_close_master", { ticket: opened.ticket, sessionId, generation: generation() }));
      done("open_master", await ask("desktop_open_master", { ticket: opened.ticket, generation: generation() }));
      const fresh = (await ask("desktop_master_state", { ticket: opened.ticket })).session;
      if (!fresh || fresh.phase !== "configured") throw new Refused("master_session_missing", "after settings changed");
      sessionId = fresh.session_id;
      receipt = await ask("desktop_submit_master", { ticket: opened.ticket, sessionId, text, generation: generation() });
      recovered = "new conversation after settings changed, ";
    }
    done("submit_master", receipt);
    state.session = sessionId;
    return { opened, sessionId, receipt, recovered };
  };
  const send = async (text, serving = null, release = true, at = local) => {
    const { opened, sessionId, receipt, recovered } = await submit(text, release, at);
    let turn;
    for (const deadline = Date.now() + at.turnMs; ; await pause(POLL_MS)) {
      const turns = await value("desktop_master_turns", { ticket: opened.ticket, sessionId, cursor: null }, at.bridge);
      turn = turns.page.turns.find((item) => item.id === receipt.operation_id);
      if (!turn) throw new Refused("master_turn_missing");
      if (!ACTIVE_TURN.has(turn.phase)) break;
      if (Date.now() > deadline) throw new Refused(`master_turn_still_${turn.phase}`);
    }
    const reason = turn.reason ?? "no_reason";
    const replied = turn.phase === "succeeded" && typeof turn.reply?.text === "string" && turn.reply.text.trim() !== "";
    const admitted = replied || turn.attempt !== null && !NOT_ADMITTED.has(reason);
    if (!admitted) throw new Refused(`${turn.phase}:${reason}`, turn.attempt === null ? "never admitted" : "admitted");
    if (serving && turn.attempt?.connection?.id !== serving) throw new Refused("served_by_signed_out_account", turn.attempt?.connection?.id);
    return { replied, text: `${recovered}${replied ? `replied, ${turn.reply.text.length} characters stored` : `admitted, harness did not reply (${turn.phase}:${reason})`}` };
  };
  const ran = (sent, note = "") => {
    if (sent.replied) return `${note}${sent.text}`;
    throw new Blocked(`harness_not_run: ${note}${sent.text}`);
  };
  const realSend = async (text, release, here) => {
    if (!real) throw new Blocked(`needs_real_signed_in_profile: stand-in ${here}`);
    if (real.problem) throw new Blocked(`${real.problem}; stand-in ${here}`);
    if (!real.bridge) {
      if (!existsSync(real.socket)) throw new Blocked(`real_engine_not_running; stand-in ${here}`);
      const started = await start(bridgeBinary, [real.socket], env, evidence, "real-bridge", BRIDGE_READY);
      realBridgeProcess = started.child;
      const via = { port: Number(started.match[1]), token: started.match[2] };
      if ((await call("desktop_connect", {}, via)).value?.connected !== true) throw new Blocked(`real_engine_not_reachable; stand-in ${here}`);
      real.bridge = via;
    }
    let sent;
    try {
      sent = await send(text, null, release, real);
    } catch (failure) {
      throw failure instanceof Refused ? new Refused(`real:${failure.code}`, failure.detail) : failure;
    }
    if (!sent.replied) throw new Refused(`real:${sent.text}`);
    return `real claude ${sent.text}; stand-in ${here}`;
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
    const started = await start(bridgeBinary, [join(crazyHome, "engine.sock")], env, evidence, "bridge", BRIDGE_READY);
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
    await step("3", "open the conversation and send (readiness and worktree admission)", async () => {
      const sent = await send("Hello master, what is in this repository?");
      return realSend("This is the owner-flow smoke. Reply with one short sentence and use no tools.", true, `${sent.text}; ${leaseCopy(join(crazyHome, "engine.db"), repo)}`);
    });
    await step("3b", "send again after a turn the harness could not finish, with no extra owner action", async () => {
      const sent = await send("Are you still there?", null, false);
      return realSend("Still there? Reply with one short sentence and use no tools.", false, sent.text);
    });
    await step("3c", "a tools-on master starts a sub-agent through the engine: it shows in the team status and the Agents tab with its task and project (LB-43)", async () => {
      if (!real) throw new Blocked("needs_real_signed_in_profile: a stand-in master cannot run a turn");
      if (real.problem) throw new Blocked(real.problem);
      if (!real.bridge) throw new Blocked("blocked_by_3: the real engine was not reached");
      const realTeam = async () => value("desktop_team_status", { id: real.project }, real.bridge);
      const status = await realTeam();
      if (!status.harnesses.find((item) => item.role === "master")?.tools) throw new Blocked("real_project_master_tools_off: turn the master's tools on in the project's Team settings");
      const known = new Set(status.tasks.map((task) => task.id));
      const sent = await realSend("This is the owner-flow smoke. Start one sub-agent that lists the files in this repository. Do not list them yourself. Then reply with one short sentence.", true, "no stand-in turn");
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
        const current = await realTeam();
        const task = current.tasks.find((item) => !known.has(item.id));
        const member = current.agents.find((item) => item.id === task?.agent && item.role === "sub");
        const view = member && (await value("desktop_agents", { projectId: real.project }, real.bridge)).find((item) => item.label === `Team sub-agent ${member.id}`);
        if (member?.status === "working" && view?.state === "running") {
          const stopped = await value("desktop_stop_agent", { id: view.id }, real.bridge);
          return `${sent}; task ${task.id} "${task.prompt.split("\n")[0].slice(0, 80)}" in project ${real.project}, team ${member.status}, agents view ${view.state}, then stopped (${stopped.state})`;
        }
        if (Date.now() > deadline) throw new Refused(task ? `team_task_${task.status}_agent_${member?.status ?? "missing"}` : "master_started_no_team_task", teamNote(current));
      }
    });
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
      return ran(await send("Settings changed. Still there?"));
    });
    await step("4b", "restart the engine, send again in the same conversation (LB-15, LB-23)", async () => {
      const before = state.session;
      await stop(engine);
      await startEngine();
      let connected = false;
      for (const deadline = Date.now() + READY_MS; !connected; await pause(POLL_MS)) {
        connected = (await call("desktop_connect")).value?.connected === true;
        if (!connected && Date.now() > deadline) throw new Refused("bridge_reconnect_failed");
      }
      const note = await isolated("4a");
      const sent = await send("The engine restarted. Still there?");
      if (note === "" && state.session !== before) throw new Refused("conversation_not_continued", `${before} became ${state.session}`);
      return ran(sent, note);
    });
    await step("4c", "first account signed out, send again (LB-17 failover)", async () => {
      const first = needs(state.accounts[0], "2a");
      await value("desktop_logout_native_auth", { ticket: (await connection(first.id)).ticket });
      await settleAuth(first.id, "logout", "unauthenticated_observed");
      const status = await memberStatus(first.id);
      if (status !== "not_signed_in") throw new Refused(`first_account_${status}`, "logout not shown");
      const note = await isolated("4b");
      return ran(await send("The first account is signed out. Still there?", needs(state.accounts[1], "2a").id), note);
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
      if (stopped.state !== "stopped" || stopped.can_stop) throw new Refused(`stop_answered_${stopped.state}`, `${stopped.status}, can_stop ${stopped.can_stop}`);
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
    await step("6e2", "sign out an account an agent is using: the refusal names the agent and its project, Stop agents and sign out ends it, the next launch uses the other member", async () => {
      const project = needs(state.project, "1");
      needs(team.accounts[1], "6");
      const { task, agent } = await delegate("Read the README of this repository.");
      if (!agent) throw new Refused(`team_task_${task.status}`, task.problem ?? "no agent");
      team.agents.push(agent.id);
      const used = team.accounts.find((account) => account.id === agent.account);
      const other = team.accounts.find((account) => account.id !== agent.account);
      if (!used || !other) throw new Refused("team_agent_account_unknown", agent.account ?? "no account");
      for (const deadline = Date.now() + TEAM_MS; ; await pause(POLL_MS * 2)) {
        const member = (await teamStatus()).agents.find((item) => item.id === agent.id);
        if (member?.status === "working") break;
        if (Date.now() > deadline) throw new Refused(`team_agent_not_working_${member?.status ?? "missing"}`, member?.problem ?? "-");
      }
      const refused = await call("desktop_logout_native_auth", { ticket: (await connection(used.id)).ticket });
      if (failureCode("desktop_logout_native_auth", refused) !== "native_profile_in_use") {
        if ("value" in refused) await settleAuth(used.id, "logout", "unauthenticated_observed").catch(() => null);
        throw new Refused("sign_out_not_refused_while_in_use", JSON.stringify(refused));
      }
      const using = (await value("desktop_account_use", { accountId: used.id })).agents;
      if (!using.some((view) => view.label === `Team sub-agent ${agent.id}` && view.project === project)) throw new Refused("in_use_without_the_agent", using.map((view) => `${view.label}@${view.project}`).join(",") || "none");
      const left = await value("desktop_release_account", { accountId: used.id, generation: generation() });
      if (left.agents.length || left.holders.length) throw new Refused("account_still_in_use", [...left.agents.map((view) => `${view.label} ${view.status}`), ...left.holders.map((holder) => holder.kind)].join(","));
      await value("desktop_logout_native_auth", { ticket: (await connection(used.id)).ticket });
      await settleAuth(used.id, "logout", "unauthenticated_observed");
      const next = await delegate("Summarize the README of this repository.");
      if (!next.agent) throw new Refused(`team_task_${next.task.status}`, next.task.problem ?? "no agent");
      team.agents.push(next.agent.id);
      if (next.agent.account !== other.id) throw new Refused("team_agent_on_signed_out_account", next.agent.account ?? "no account");
      return `refusal named ${using.map((view) => view.label).join(", ")}; stopped and signed out; next agent on the other member, ${launched(next.agent, next.task)}`;
    });
    await step("6e3", "sign out the account a master turn is using: the refusal names the master conversation and its project, End conversation and sign out ends it and signs out", async () => {
      const project = needs(state.project, "1");
      const { opened, sessionId, receipt } = await submit("List every file in this repository with a one-paragraph summary of each, reading each file first.", true);
      let turn;
      for (const deadline = Date.now() + TURN_MS; ; await pause(POLL_MS)) {
        turn = (await value("desktop_master_turns", { ticket: opened.ticket, sessionId, cursor: null })).page.turns.find((item) => item.id === receipt.operation_id);
        if (!turn) throw new Refused("master_turn_missing");
        if (turn.phase === "running") break;
        if (!ACTIVE_TURN.has(turn.phase)) throw new Refused(`master_turn_ended_before_sign_out_${turn.phase}`, turn.reason ?? "-");
        if (Date.now() > deadline) throw new Refused(`master_turn_still_${turn.phase}`);
      }
      const accountId = turn.attempt?.connection?.id;
      if (!accountId) throw new Refused("master_turn_without_account");
      const refused = await call("desktop_logout_native_auth", { ticket: (await connection(accountId)).ticket });
      if (failureCode("desktop_logout_native_auth", refused) !== "native_profile_in_use") throw new Refused("sign_out_not_refused_while_in_use", JSON.stringify(refused));
      const holders = (await value("desktop_account_use", { accountId })).holders;
      if (!holders.some((holder) => holder.kind === "master" && holder.project === project && holder.session === sessionId)) throw new Refused("in_use_without_the_master", JSON.stringify(holders));
      const left = await value("desktop_release_account", { accountId, generation: generation() });
      if (left.agents.length || left.holders.length) throw new Refused("account_still_in_use", JSON.stringify(left));
      const ended = (await value("desktop_master_state", { ticket: (await value("desktop_open_project", { id: project })).ticket })).session;
      if (ended?.session_id === sessionId && ended.phase === "configured") throw new Refused("master_conversation_not_ended");
      await value("desktop_logout_native_auth", { ticket: (await connection(accountId)).ticket });
      await settleAuth(accountId, "logout", "unauthenticated_observed");
      return `refusal named the master conversation in the project; conversation ended and ${accountId} signed out`;
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
    await stop(realBridgeProcess);
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
  const where = keep ? `evidence and temp root kept at ${root}` : process.env.SMOKE_EVIDENCE ? `evidence ${evidence}, temp root removed` : "evidence and temp root removed (SMOKE_EVIDENCE=1 keeps them)";
  console.log(`\n${results.length - failed - blocked} PASS, ${failed} FAIL, ${blocked} BLOCKED; ${where}`);
  process.exitCode = failed > 0 ? 1 : blocked > 0 ? 3 : 0;
}

const mode = process.argv.slice(2).join(" ");
if (mode === "--clean" || mode === "--clean --dry-run") clean(mode !== "--clean");
else if (mode) {
  console.error("usage: node scripts/smoke/owner-flow.mjs [--clean [--dry-run]]");
  process.exitCode = 2;
} else await main();
