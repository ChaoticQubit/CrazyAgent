import { createConnection } from "node:net";
import { encodeFrame, FrameReader, type Reply } from "./engine.js";
import { GIT_REQUEST_MS, GIT_SOCKET_ENV, gitOutcome, type GitOutcome } from "./git.js";
import { unconfirmed } from "./scm-model.js";

const ANSWER_GRACE_MS = 5_000;

/** Where the forwarder prints: the shape of `process.stdout` and `process.stderr`. */
export interface Printer {
  write(text: string): unknown;
}

/**
 * Hands one git network command to the CrazyAgent extension's relay and prints the
 * engine's answer as git itself would have: its output on the same streams and its exit
 * code. A refusal, a missing relay, an unreadable answer or no answer in time prints one
 * `fatal:` line and exits 128, as git does for a fatal error; a command the engine could
 * not confirm finished says it may have run.
 * Inputs: the environment naming the relay, git's arguments, the folder git runs in and
 * the two streams. Output: the exit code.
 */
export function forward(
  env: Record<string, string | undefined>,
  args: string[],
  folder: string,
  stdout: Printer,
  stderr: Printer,
): Promise<number> {
  const socket = env[GIT_SOCKET_ENV];
  if (!socket) {
    stderr.write("fatal: CrazyAgent runs this git command in its engine, and the IDE has not connected to it yet\n");
    return Promise.resolve(128);
  }
  return new Promise((resolve) => {
    const connection = createConnection(socket);
    const reader = new FrameReader();
    let done = false;
    const finish = (code: number, message?: string): void => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      if (message !== undefined) stderr.write(message);
      connection.destroy();
      resolve(code);
    };
    const timer = setTimeout(
      () => finish(128, "fatal: the CrazyAgent engine did not answer this git command in time\n"),
      GIT_REQUEST_MS + ANSWER_GRACE_MS,
    );
    connection.on("connect", () => connection.write(encodeFrame({ args, folder })));
    connection.on("data", (chunk: Buffer) => {
      let replies: unknown[];
      try {
        replies = reader.push(chunk);
      } catch {
        finish(128, "fatal: the CrazyAgent engine sent an unreadable answer\n");
        return;
      }
      const [reply] = replies as Reply<GitOutcome>[];
      if (reply === undefined) return;
      if (reply.kind !== "ok") {
        finish(128, `fatal: ${unconfirmed(reply.code) ?? `CrazyAgent did not run this git command (${reply.code})`}\n`);
        return;
      }
      const outcome = gitOutcome(reply.data);
      if (outcome === undefined) {
        finish(128, "fatal: the CrazyAgent engine sent an unreadable answer\n");
        return;
      }
      stdout.write(outcome.stdout);
      stderr.write(outcome.stderr);
      finish(outcome.code);
    });
    connection.on("error", () => finish(128, "fatal: the CrazyAgent IDE relay is not reachable\n"));
    connection.on("close", () => finish(128, "fatal: the CrazyAgent IDE relay closed before it answered\n"));
  });
}

if (require.main === module) {
  void forward(process.env, process.argv.slice(2), process.cwd(), process.stdout, process.stderr).then((code) => {
    process.exitCode = code;
  });
}
