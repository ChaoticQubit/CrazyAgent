import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type * as vscode from "vscode";
import { type Disposable, type Engine, encodeFrame, FrameReader, type Reply } from "./engine.js";

/** Variable naming the Unix socket the forwarder hands one git network command to. */
export const GIT_SOCKET_ENV = "CRAZYAGENT_IDE_GIT_SOCKET";

/** Variable naming the node binary the engine's `git.path` shim runs the forwarder with. */
export const GIT_NODE_ENV = "CRAZYAGENT_IDE_GIT_NODE";

/** Variable naming the forwarder script the engine's `git.path` shim runs. */
export const GIT_FORWARD_ENV = "CRAZYAGENT_IDE_GIT_FORWARD";

/** Variable naming the engine's `git.path` shim, which runs a pull's local step. */
export const GIT_SHIM_ENV = "CRAZYAGENT_IDE_GIT_SHIM";

/** Variable holding the secret of one relay start, which every forwarded command carries. */
export const GIT_SECRET_ENV = "CRAZYAGENT_IDE_GIT_SECRET";

/**
 * How long the relay waits for the engine to run one git network command: past the
 * engine's own bound for `ide.git`, so git always gets the engine's answer or its refusal.
 */
export const GIT_REQUEST_MS = 700_000;

/** The refusal for a git command that may have run: the engine got it but never answered. */
export const GIT_OUTCOME_UNKNOWN = "ide_git_outcome_unknown";

/** The most arguments one forwarded git command carries after `git`. */
export const GIT_ARGS_MAX = 16;

const ARG_BYTES_MAX = 1024;
const FOLDER_BYTES_MAX = 4096;
const CONNECTIONS_MAX = 4;
const PLAIN_CODE = /^[a-z][a-z0-9_]{0,63}$/;
const ENGINE_LOST = new Set(["ide_request_timeout", "ide_engine_closed"]);
const NEVER_CONNECTED = new Set(["ENOENT", "ECONNREFUSED"]);

/** What the relay needs from the engine bridge: one call with its own timer. */
export interface Requester {
  request<T>(name: string, args: Record<string, unknown>, timeMs?: number): Promise<Reply<T>>;
}

/** One git network command as the forwarder sends it: git's arguments and its folder. */
export interface GitCall {
  args: string[];
  folder: string;
}

/** The engine's answer to one git command: git's exit code and what it printed. */
export interface GitOutcome {
  code: number;
  stdout: string;
  stderr: string;
}

/**
 * Reads one forwarded command, refusing anything but 1 to `GIT_ARGS_MAX` non-empty string
 * arguments of at most 1024 bytes and an absolute folder path. Input: the parsed frame.
 * Output: the command, or undefined.
 */
export function gitCall(value: unknown): GitCall | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const { args, folder } = value as { args?: unknown; folder?: unknown };
  const plain = (text: unknown, max: number): text is string =>
    typeof text === "string" && text.length > 0 && Buffer.byteLength(text) <= max && !text.includes("\0");
  if (!Array.isArray(args) || args.length === 0 || args.length > GIT_ARGS_MAX) return undefined;
  if (!args.every((arg) => plain(arg, ARG_BYTES_MAX))) return undefined;
  if (!plain(folder, FOLDER_BYTES_MAX) || !folder.startsWith("/")) return undefined;
  return { args, folder };
}

/**
 * Reads the engine's answer to one git command: a whole exit code from 0 to 255 and two
 * texts. Input: the reply data. Output: the outcome, or undefined.
 */
export function gitOutcome(value: unknown): GitOutcome | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const { code, stdout, stderr } = value as Partial<Record<keyof GitOutcome, unknown>>;
  if (typeof code !== "number" || !Number.isInteger(code) || code < 0 || code > 255) return undefined;
  if (typeof stdout !== "string" || typeof stderr !== "string") return undefined;
  return { code, stdout, stderr };
}

/**
 * Starts the relay the engine's `git.path` shim reaches: a Unix socket in this server's
 * temp folder that takes one git network command per connection, sends it to the engine
 * as `ide.git` and answers with the engine's reply. VS Code's git inherits this process's
 * environment, so the socket, node, forwarder and secret are named there once it listens.
 * Inputs: the engine bridge, the environment to name them in and the forwarder's path.
 * Output: the relay, which closes its socket and clears the names when disposed.
 */
export async function startRelay(
  engine: Requester,
  env: Record<string, string | undefined>,
  forwarder: string,
): Promise<Disposable> {
  const path = join(tmpdir(), `cai-git-${randomBytes(8).toString("hex")}.sock`);
  const secret = randomBytes(32).toString("hex");
  const server = createServer((connection) => relay(engine, connection, secret));
  server.maxConnections = CONNECTIONS_MAX;
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(path, () => {
      server.off("error", reject);
      resolve();
    });
  });
  env[GIT_SOCKET_ENV] = path;
  env[GIT_NODE_ENV] = process.execPath;
  env[GIT_FORWARD_ENV] = forwarder;
  env[GIT_SECRET_ENV] = secret;
  return {
    dispose: () => {
      if (env[GIT_SOCKET_ENV] === path) {
        delete env[GIT_SOCKET_ENV];
        delete env[GIT_NODE_ENV];
        delete env[GIT_FORWARD_ENV];
        delete env[GIT_SECRET_ENV];
      }
      server.close();
    },
  };
}

/**
 * Says whether a forwarded frame carries this relay's secret, in constant time for a secret
 * of the right length. Inputs: the parsed frame and the secret. Output: whether it matches.
 */
function carries(value: unknown, secret: string): boolean {
  if (typeof value !== "object" || value === null) return false;
  const given = (value as { secret?: unknown }).secret;
  if (typeof given !== "string") return false;
  const [held, wanted] = [Buffer.from(given), Buffer.from(secret)];
  return held.length === wanted.length && timingSafeEqual(held, wanted);
}

/**
 * Answers one relay connection: its first frame is the command, refused without this
 * relay's secret, and anything after it is ignored.
 * Inputs: the engine bridge, the connection and the secret. Output: none.
 */
function relay(engine: Requester, connection: Socket, secret: string): void {
  const reader = new FrameReader();
  let asked = false;
  connection.on("error", () => undefined);
  connection.on("data", (chunk: Buffer) => {
    if (asked) return;
    let frames: unknown[];
    try {
      frames = reader.push(chunk);
    } catch {
      connection.destroy();
      return;
    }
    if (frames.length === 0) return;
    asked = true;
    const call = carries(frames[0], secret) ? gitCall(frames[0]) : undefined;
    if (call === undefined) {
      answer(connection, { kind: "rejected", code: "ide_git_refused" });
      return;
    }
    engine.request<GitOutcome>("ide.git", { args: call.args, folder: call.folder }, GIT_REQUEST_MS).then(
      (reply) => answer(connection, reply.kind === "ok" ? reply : { kind: "rejected", code: reply.code }),
      (error: unknown) => answer(connection, { kind: "rejected", code: gitFailure(error) }),
    );
  });
}

/**
 * The refusal git gets when the engine call failed without an answer. An engine that never
 * took the connection, or a session that was never set up, did not run it; an engine that
 * timed out, closed or broke the reply after the call was sent may have run it.
 * Input: the failure. Output: the refusal code.
 */
export function gitFailure(error: unknown): string {
  if (!(error instanceof Error) || ENGINE_LOST.has(error.message)) return GIT_OUTCOME_UNKNOWN;
  if (PLAIN_CODE.test(error.message)) return error.message;
  const code = (error as NodeJS.ErrnoException).code;
  return code !== undefined && NEVER_CONNECTED.has(code) ? "ide_git_bridge_failed" : GIT_OUTCOME_UNKNOWN;
}

/** Sends the one answer and ends the connection, unless git already went away. */
function answer(connection: Socket, reply: Reply<GitOutcome>): void {
  if (!connection.destroyed) connection.end(encodeFrame(reply));
}

/**
 * The IDE's push, pull, fetch and ls-remote through the engine (DEC-098 addendum 13): the
 * IDE sandbox has no network, so the engine's `git.path` shim sends them to this relay.
 * Input: the extension context and the engine bridge. Output: none; the relay closes with
 * the context.
 */
export function remoteGit(context: vscode.ExtensionContext, engine: Engine): void {
  startRelay(engine, process.env, join(context.extensionPath, "out", "git-forward.js")).then(
    (relay) => context.subscriptions.push(relay),
    () => undefined,
  );
}
