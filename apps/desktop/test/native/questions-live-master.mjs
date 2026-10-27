import { spawn } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";

const CONTROL = "qf-live-master";
const SERVER_NAME = "crazyagent-team";
const POLL_MS = 200;
const CALL_MS = 120000;
const REPLY_MS = 300;
const PASTE_OPEN = "\x1b[200~";
const PASTE_CLOSE = "\x1b[201~";
const CALL_SUFFIX = ".call.json";
const SHOW_SUFFIX = ".show.txt";
const NAME = /^[a-z0-9-]{1,64}$/;

/** Reads the value that follows one flag in the launch arguments.
 * Inputs: the arguments and the flag. Output: the value, or null when the flag is absent. */
function flag(args, name) {
  const at = args.indexOf(name);
  return at >= 0 && at + 1 < args.length ? args[at + 1] : null;
}

/** Finds the engine's team tool server in the `--mcp-config` values, given as JSON text or
 * as a file, exactly as Claude Code reads them. Input: the launch arguments. Output: the
 * server's command and arguments, or null when the launch named none. */
function teamServer(args) {
  for (let at = 0; at + 1 < args.length; at += 1) {
    if (args[at] !== "--mcp-config") continue;
    const value = args[at + 1];
    const text = value.trimStart().startsWith("{") ? value : existsSync(value) ? readFileSync(value, "utf8") : "";
    let server = null;
    try {
      server = JSON.parse(text).mcpServers?.[SERVER_NAME] ?? null;
    } catch {
      server = null;
    }
    if (server && typeof server.command === "string") return { command: server.command, args: Array.isArray(server.args) ? server.args : [] };
  }
  return null;
}

/** Writes one file whole under the control folder, so the smoke never reads half of it.
 * Inputs: the folder, the file name and the text. Output: none. */
function publish(folder, name, text) {
  const staged = join(folder, `.${name}.partial`);
  writeFileSync(staged, text, { mode: 0o600 });
  renameSync(staged, join(folder, name));
}

const args = process.argv.slice(2);
const home = process.env.CLAUDE_CONFIG_DIR;
const session = flag(args, "--session-id") ?? flag(args, "--resume");
if (!home || !session) {
  process.stderr.write("questions-flow live master: needs CLAUDE_CONFIG_DIR and --session-id or --resume\n");
  process.exit(64);
}
const control = join(home, CONTROL);
mkdirSync(control, { recursive: true, mode: 0o700 });
const cwd = realpathSync(process.cwd());
const transcript = join(home, "projects", cwd.replace(/[^A-Za-z0-9]/g, "-"), `${session}.jsonl`);
mkdirSync(dirname(transcript), { recursive: true, mode: 0o700 });
const server = teamServer(args);
appendFileSync(join(control, "launches.jsonl"), `${JSON.stringify({ at: Date.now(), pid: process.pid, session, resumed: args.includes("--resume"), cwd, server: server !== null })}\n`, { mode: 0o600 });

let turns = 0;
let paste = null;
const taken = new Set();

/** Takes one typed turn the way Claude Code records it: the typed text as a user entry,
 * then a finished assistant reply, and keeps the text for the smoke to count deliveries.
 * Input: the typed text. Output: none. */
function turn(typed) {
  turns += 1;
  appendFileSync(join(control, "turns.jsonl"), `${JSON.stringify({ at: Date.now(), pid: process.pid, n: turns, text: typed })}\n`, { mode: 0o600 });
  appendFileSync(transcript, `${JSON.stringify({ type: "user", message: { role: "user", content: typed } })}\n`, { mode: 0o600 });
  setTimeout(() => {
    appendFileSync(transcript, `${JSON.stringify({ type: "assistant", message: { role: "assistant", content: [{ type: "text", text: "Done." }], stop_reason: "end_turn" } })}\n`, { mode: 0o600 });
    process.stdout.write("Done.\n");
  }, REPLY_MS);
}

/** Calls one tool of the engine's team tool server over MCP stdio, as Claude Code does,
 * and publishes the server's answer for the smoke. Inputs: the call id and the request
 * `{tool, arguments}`. Output: none; the answer lands in `<id>.reply.json`. */
function callTool(id, request) {
  if (!server) {
    publish(control, `${id}.reply.json`, JSON.stringify({ error: "the launch named no crazyagent-team server" }));
    return;
  }
  const child = spawn(server.command, server.args, { stdio: ["pipe", "pipe", "pipe"], env: process.env });
  let out = "";
  let done = false;
  const finish = (answer) => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    child.stdin.end();
    publish(control, `${id}.reply.json`, JSON.stringify(answer));
  };
  const timer = setTimeout(() => finish({ error: `no tool reply in ${CALL_MS} ms` }), CALL_MS);
  child.stdout.on("data", (chunk) => {
    out += chunk;
    for (let end = out.indexOf("\n"); end >= 0; end = out.indexOf("\n")) {
      const line = out.slice(0, end);
      out = out.slice(end + 1);
      let message = null;
      try {
        message = JSON.parse(line);
      } catch {
        message = null;
      }
      if (message?.id === 2) finish(message.error ? { error: message.error.message } : { result: message.result });
    }
  });
  child.on("error", (error) => finish({ error: error.message }));
  child.on("exit", (code, signal) => finish({ error: `the tool server exited ${code ?? signal}` }));
  const send = (message) => child.stdin.write(`${JSON.stringify(message)}\n`);
  send({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "questions-flow-stand-in", version: "1" } } });
  send({ jsonrpc: "2.0", method: "notifications/initialized" });
  send({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: request.tool, arguments: request.arguments } });
}

/** Takes the smoke's new requests from the control folder: tool calls to make and screens
 * to print. Each file is renamed before it is read, so it is taken once. Output: none. */
function poll() {
  for (const name of readdirSync(control).sort()) {
    const suffix = name.endsWith(CALL_SUFFIX) ? CALL_SUFFIX : name.endsWith(SHOW_SUFFIX) ? SHOW_SUFFIX : null;
    const id = suffix ? name.slice(0, -suffix.length) : null;
    if (!id || !NAME.test(id) || taken.has(name)) continue;
    taken.add(name);
    const held = join(control, `${name}.taken`);
    renameSync(join(control, name), held);
    const text = readFileSync(held, "utf8");
    if (suffix === SHOW_SUFFIX) process.stdout.write(text);
    else callTool(id, JSON.parse(text));
  }
}

process.stdout.write("\x1b[?2004hQuestions stand-in ready\n");
setInterval(poll, POLL_MS);
const lines = createInterface({ input: process.stdin, terminal: false, crlfDelay: Infinity });
lines.on("line", (line) => {
  let text = line;
  const opened = text.indexOf(PASTE_OPEN);
  if (opened >= 0) {
    paste = [];
    text = text.slice(opened + PASTE_OPEN.length);
  }
  if (paste === null) return;
  const closed = text.indexOf(PASTE_CLOSE);
  if (closed < 0) {
    paste.push(text);
    return;
  }
  paste.push(text.slice(0, closed));
  const typed = paste.join("\n");
  paste = null;
  turn(typed);
});
lines.on("close", () => process.exit(0));
