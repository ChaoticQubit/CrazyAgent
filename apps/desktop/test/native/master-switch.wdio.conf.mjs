import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { chooseHome } from "./run-layout.mjs";

process.umask(0o077);
const app = process.env.WD_APP;
const engineBinary = process.env.MS_ENGINE;
const root = process.env.MS_ROOT;
const evidence = process.env.MS_EVIDENCE;
const port = Number(process.env.WD_PORT);
for (const [name, path] of [["WD_APP", app], ["MS_ENGINE", engineBinary], ["MS_ROOT", root], ["MS_EVIDENCE", evidence]]) {
  assert(path && isAbsolute(path), `${name} must be an absolute path`);
}
assert(Number.isInteger(port) && port > 1024 && port < 65536, "WD_PORT must be a free local port");
const runtime = join(root, "engine");
const socket = join(runtime, "engine.sock");
/** The HOME the engine, the app and the spec's WebKit store share: the prepared folder
 * CRAZYAGENT_E2E_HOME names, as chooseHome checks it, else MS_ROOT/home as before.
 * Input: the environment. Output: the folder. */
export const home = chooseHome(join(root, "home")).home;
assert(Buffer.byteLength(socket) < 100, "MS_ROOT must be short enough for the engine socket path");
const ENGINE_READY_MS = 60000;
const ENGINE_STOP_MS = 20000;

/** How long a stand-in turn for a typed line with "long" in it runs, in seconds: long enough
 * for the owner to save a master harness change while the master is still in its turn. Any
 * other line runs a turn of 2 seconds. */
export const TURN_S = 90;

/** A Codex stand-in that keeps and resumes its conversation in the rollout files Codex
 * writes, and runs each typed line as a turn, of TURN_S seconds for a long one. No model
 * ever runs. Run outside a terminal, as setup runs a harness, it fails at once. */
const CODEX = `#!/bin/sh
[ "$1" = app-server ] && exit 0
[ -t 0 ] || { echo "Codex stand-in runs only in a terminal"; exit 3; }
d="$CODEX_HOME/sessions/2026/11/05"
/bin/mkdir -p "$d" || exit 1
id=""
prev=""
for a in "$@"; do
  [ "$prev" = "resume" ] && id="$a"
  prev="$a"
done
if [ -n "$id" ]; then
  f=""
  for c in "$d"/rollout-*-"$id".jsonl; do [ -f "$c" ] && f="$c"; done
  [ -n "$f" ] || { echo "no conversation $id"; exit 4; }
else
  id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
  f="$d/rollout-2026-11-05T00-00-00-$id.jsonl"
  printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\\n' "$id" "$(/bin/pwd -P)" > "$f"
fi
/bin/stty -echo 2>/dev/null
printf '\\033[?2004hCodex stand-in ready\\n'
e=$(printf '\\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\\n' >> "$f"
  printf 'Codex stand-in turn\\n'
  case "$line" in *long*) /bin/sleep ${TURN_S} ;; *) /bin/sleep 2 ;; esac
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}\\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\\n' >> "$f"
done
`;

/** A Claude Code stand-in that keeps and resumes its conversation in the transcript files
 * Claude Code writes, and runs each typed line as a turn, of TURN_S seconds for a long one.
 * Run outside a terminal, as setup runs a harness, it fails at once. */
const CLAUDE = `#!/bin/sh
[ -t 0 ] || { echo "Claude stand-in runs only in a terminal"; exit 3; }
id=""
prev=""
for a in "$@"; do
  case "$prev" in --resume|--session-id) id="$a" ;; esac
  prev="$a"
done
[ -n "$id" ] || { echo "no session id"; exit 4; }
d="$CLAUDE_CONFIG_DIR/projects/$(/bin/pwd -P | /usr/bin/tr -c 'A-Za-z0-9\\n' '-' | /usr/bin/tr -d '\\n')"
/bin/mkdir -p "$d" || exit 1
f="$d/$id.jsonl"
case " $* " in *" --resume "*) [ -f "$f" ] || { echo "no conversation $id"; exit 4; } ;; esac
/bin/stty -echo 2>/dev/null
printf '\\033[?2004hClaude stand-in ready\\n'
e=$(printf '\\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  printf '{"type":"user","message":{"content":"Owner turn"}}\\n' >> "$f"
  printf 'Claude stand-in turn\\n'
  case "$line" in *long*) /bin/sleep ${TURN_S} ;; *) /bin/sleep 2 ;; esac
  printf '{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}],"stop_reason":"end_turn"}}\\n' >> "$f"
done
`;

/** Writes the engine settings the spec runs on: one local endpoint account for each master
 * harness, Codex and Claude Code, in one pool. The endpoint is never called. Input: the
 * engine root. */
function settings(folder) {
  const endpoint = "endpoint = { kind = \"http\", url = \"http://127.0.0.1:9/v1\" }, model = { id = \"e2e-model\", author = \"openai-compatible\" }, auth = { method = \"none\" }";
  writeFileSync(join(folder, "global.toml"), `version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [
  { id = "local-codex", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", ${endpoint} },
  { id = "local-claude", provider = "openai-compatible", harness = "claude", protocol = "anthropic-messages", account_ref = "local-account", ${endpoint} },
]
pools = [{ id = "local-pool", members = ["local-codex", "local-claude"], projects = [], roles = [], fallback_allowed = false }]
`, { mode: 0o600 });
}

/** Writes one stand-in executable. Inputs: its name and script. Output: its absolute path. */
function standIn(name, script) {
  const path = join(root, "bin", name);
  writeFileSync(path, script, { mode: 0o700 });
  return path;
}

let engine;

/** Starts the engine in the spec's engine folder with the two stand-ins, a home and Codex
 * home of its own and only the path from the outer environment, so it finds tmux and
 * graphify, and waits for its ready line. Output: a promise that settles once it is ready;
 * rejects when it exits or never answers. */
function startEngine() {
  mkdirSync(join(root, "bin"), { mode: 0o700 });
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: join(home, ".codex"), TMPDIR: process.env.TMPDIR ?? "/tmp", LANG: "en_US.UTF-8" };
  const args = ["engine", runtime, join(runtime, "global.toml"), join(runtime, "project.toml"), "engine.sock", "--writable-settings",
    "--codex-executable", standIn("codex", CODEX), "--claude-executable", standIn("claude", CLAUDE)];
  engine = spawn(engineBinary, args, { cwd: runtime, env, stdio: ["ignore", "pipe", "pipe"] });
  engine.stdout.pipe(createWriteStream(join(evidence, "engine.out"), { mode: 0o600 }));
  engine.stderr.pipe(createWriteStream(join(evidence, "engine.err"), { mode: 0o600 }));
  return new Promise((resolve, reject) => {
    let seen = "";
    const timer = setTimeout(() => reject(new Error(`engine did not print ready within ${ENGINE_READY_MS} ms; see engine.err`)), ENGINE_READY_MS);
    engine.stdout.on("data", (chunk) => {
      seen = (seen + chunk).slice(-4096);
      if (/^ready$/m.test(seen)) {
        clearTimeout(timer);
        resolve();
      }
    });
    engine.once("exit", (code, signal) => {
      clearTimeout(timer);
      reject(new Error(`engine exited before ready: ${code ?? signal}; see engine.err`));
    });
  });
}

/** Stops the engine with SIGTERM, and SIGKILL when it is still running after
 * ENGINE_STOP_MS. Output: a promise that settles once it exited. */
async function stopEngine() {
  if (!engine || engine.exitCode !== null || engine.signalCode !== null) return;
  const exited = new Promise((resolve) => engine.once("exit", resolve));
  engine.kill("SIGTERM");
  const timer = setTimeout(() => engine.kill("SIGKILL"), ENGINE_STOP_MS);
  await exited;
  clearTimeout(timer);
}

/** Ends the tmux server the engine ran the masters' terminals in, which outlives the
 * engine. Output: none; a server already gone is fine. */
function stopTerminals() {
  const server = join(runtime, "terminal", "tmux.sock");
  if (existsSync(server)) spawnSync("tmux", ["-S", server, "kill-server"], { env: { PATH: process.env.PATH }, stdio: "ignore" });
}

export const config = {
  runner: "local",
  specs: [fileURLToPath(new URL("master-switch.e2e.mjs", import.meta.url))],
  maxInstances: 1,
  capabilities: [{ browserName: "tauri", "tauri:options": { application: app } }],
  services: [["@wdio/tauri-service", {
    driverProvider: "embedded", autoInstallTauriDriver: false, embeddedPort: port,
    appArgs: [], startTimeout: 60000, captureBackendLogs: true, backendLogLevel: "info",
    env: { CRAZYAGENT_DESKTOP_SOCKET: socket, HOME: home, CFFIXED_USER_HOME: home },
  }]],
  framework: "mocha",
  reporters: ["spec"],
  logLevel: "warn",
  outputDir: join(evidence, "webdriver"),
  connectionRetryCount: 0,
  connectionRetryTimeout: 180000,
  waitforTimeout: 15000,
  specFileRetries: 0,
  mochaOpts: { timeout: 900000, retries: 0 },
  onPrepare: async function () {
    mkdirSync(evidence, { recursive: true, mode: 0o700 });
    try {
      mkdirSync(root, { mode: 0o700 });
    } catch {
      throw new Error(`MS_ROOT must name a new folder: ${root}`);
    }
    mkdirSync(join(home, ".codex"), { recursive: true, mode: 0o700 });
    mkdirSync(runtime, { mode: 0o700 });
    settings(runtime);
    try {
      await startEngine();
    } catch (failure) {
      await stopEngine();
      throw failure;
    }
  },
  before: async function () {
    await globalThis.browser.setWindowSize(1280, 900);
  },
  afterTest: async function (test, context, result) {
    if (!result.passed) await globalThis.browser.saveScreenshot(join(evidence, `failure-${test.title.replace(/[^A-Za-z0-9]+/g, "-").slice(0, 60)}.png`));
  },
  onComplete: async function () {
    await stopEngine();
    stopTerminals();
  },
};
