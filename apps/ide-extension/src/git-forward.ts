import { execFile, spawn } from "node:child_process";
import { createConnection } from "node:net";
import { encodeFrame, FrameReader, type Reply } from "./engine.js";
import { GIT_OUTCOME_UNKNOWN, GIT_REQUEST_MS, GIT_SECRET_ENV, GIT_SOCKET_ENV, gitOutcome, type GitOutcome } from "./git.js";
import { unconfirmed } from "./scm-model.js";

const ANSWER_GRACE_MS = 5_000;
const SETTING_BYTES_MAX = 4096;
const REBASES = new Set(["true", "yes", "on", "1", "interactive", "i"]);
const REBASE_MERGES = new Set(["merges", "m"]);
const FALSE = new Set(["false", "no", "off", "0"]);
const TRUE = new Set(["true", "yes", "on", "1"]);
const DETACHED = "You are not currently on a branch.\nCrazyAgent pulls only a branch's upstream: switch to a branch that has one.\n";
const NO_UPSTREAM =
  "There is no tracking information for the current branch.\nCrazyAgent pulls only a branch's upstream: set one with git branch --set-upstream-to=<remote>/<branch>.\n";

/** The local step of a pull: git's arguments for it, or why the pull is refused before its fetch. */
export type PullStep = { argv: string[] } | { refused: string };

/** Where the forwarder prints: the shape of `process.stdout` and `process.stderr`. */
export interface Printer {
  write(text: string): unknown;
}

/**
 * Runs one git network command as git would, through the engine: the engine runs the
 * network step, and of a pull it only fetches, so once that fetch succeeded the merge or
 * rebase of the branch's upstream runs here, in the IDE sandbox, where the sandbox's write
 * denies apply. A pull with no upstream to take is refused before anything is fetched.
 * The script hands this a copy of its environment and removes the relay's secret from its
 * own, so no git it starts, and no hook of that git, holds the secret.
 * Inputs: the environment naming the relay, git's arguments, the folder git runs in and
 * the two streams. Output: the exit code.
 */
export async function forward(
  env: Record<string, string | undefined>,
  args: string[],
  folder: string,
  stdout: Printer,
  stderr: Printer,
): Promise<number> {
  const step = args[0] === "pull" ? await pullStep(args, folder) : undefined;
  if (step !== undefined && "refused" in step) {
    stderr.write(step.refused);
    return 1;
  }
  const code = await ask(env, args, folder, stdout, stderr);
  if (step === undefined || code !== 0) return code;
  return local(step.argv, folder, stdout, stderr);
}

/**
 * The local step of a pull after the engine fetched, as git's own pull chooses it, taken
 * only from the current branch's upstream (its remote-tracking ref), never from another
 * fetched branch: a pull on a detached HEAD, of a branch with no upstream or naming another
 * remote branch is refused. With `-r`, or the branch's `rebase` or else `pull.rebase`
 * setting on and `pull.ff` not `only`, a rebase onto the upstream from the fork point
 * found before the fetch, as git's own pull does (`merges` keeps merges); otherwise a merge, fast-forward as `pull.ff` says and only fast-forward when
 * neither setting is set. `--autostash` is kept.
 * Inputs: git's pull arguments and the folder. Output: the step, or why it is refused.
 */
export async function pullStep(args: string[], folder: string): Promise<PullStep> {
  const branch = await setting(folder, ["symbolic-ref", "--quiet", "--short", "HEAD"]);
  if (branch === undefined) return { refused: DETACHED };
  const upstream = await setting(folder, ["rev-parse", "--symbolic-full-name", "@{upstream}"]);
  if (upstream === undefined || !upstream.startsWith("refs/")) return { refused: NO_UPSTREAM };
  const [remote, ref] = args.slice(1).filter((arg) => !arg.startsWith("-"));
  if (remote !== undefined) {
    const own = await setting(folder, ["config", "--get", `branch.${branch}.remote`]);
    const merge = await setting(folder, ["config", "--get", `branch.${branch}.merge`]);
    if (remote !== own || (ref !== undefined && ref !== merge && `refs/heads/${ref}` !== merge)) {
      return { refused: `CrazyAgent pulls only the upstream of '${branch}' (${upstream}).\n` };
    }
  }
  const autostash = args.includes("--autostash") ? ["--autostash"] : [];
  const ofBranch = await setting(folder, ["config", "--get", `branch.${branch}.rebase`]);
  const rebase = (ofBranch ?? (await setting(folder, ["config", "--get", "pull.rebase"])))?.toLowerCase();
  const ff = (await setting(folder, ["config", "--get", "pull.ff"]))?.toLowerCase();
  const configured = ff !== "only" && rebase !== undefined;
  const merges = configured && REBASE_MERGES.has(rebase) && !args.includes("-r");
  if (args.includes("-r") || merges || (configured && REBASES.has(rebase))) {
    const fork = await setting(folder, ["merge-base", "--fork-point", upstream]);
    const from = fork === undefined || fork === "" ? [upstream] : ["--onto", upstream, fork];
    return { argv: ["rebase", ...(merges ? ["--rebase-merges"] : []), ...autostash, ...from] };
  }
  const mode =
    ff === "only"
      ? ["--ff-only"]
      : ff !== undefined && FALSE.has(ff)
        ? ["--no-ff"]
        : ff !== undefined && TRUE.has(ff)
          ? ["--ff"]
          : rebase === undefined
            ? ["--ff-only"]
            : [];
  return { argv: ["merge", ...mode, ...autostash, upstream] };
}

/**
 * Reads one line git prints for the folder, such as a setting. Inputs: the folder and
 * git's arguments. Output: the trimmed line, or undefined when git exits non-zero.
 */
function setting(folder: string, args: string[]): Promise<string | undefined> {
  return new Promise((resolve) => {
    execFile("git", args, { cwd: folder, maxBuffer: SETTING_BYTES_MAX }, (error, stdout) => {
      resolve(error ? undefined : stdout.trim());
    });
  });
}

/**
 * Runs git in the folder inside the IDE sandbox, printing on the same streams. Inputs:
 * git's arguments, the folder and the two streams. Output: git's exit code, or 128 with a
 * `fatal:` line when it cannot start or was ended by a signal.
 */
function local(args: string[], folder: string, stdout: Printer, stderr: Printer): Promise<number> {
  return new Promise((resolve) => {
    const child = spawn("git", args, { cwd: folder, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.setEncoding("utf8").on("data", (text: string) => stdout.write(text));
    child.stderr.setEncoding("utf8").on("data", (text: string) => stderr.write(text));
    child.on("error", () => {
      stderr.write("fatal: git did not start for the local step of this pull\n");
      resolve(128);
    });
    child.on("close", (code) => resolve(code ?? 128));
  });
}

/**
 * Hands one git network command to the CrazyAgent extension's relay and prints the
 * engine's answer as git itself would have: its output on the same streams and its exit
 * code. A refusal, a missing relay or an unreadable answer prints one `fatal:` line and
 * exits 128, as git does for a fatal error; a command the engine could not confirm
 * finished, and one lost or unanswered after it was sent, says it may have run.
 * Inputs: the environment naming the relay, git's arguments, the folder git runs in and
 * the two streams. Output: the exit code.
 */
function ask(
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
    let sent = false;
    const finish = (code: number, message?: string): void => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      if (message !== undefined) stderr.write(message);
      connection.destroy();
      resolve(code);
    };
    const lost = (): void =>
      finish(
        128,
        sent
          ? `fatal: ${unconfirmed(GIT_OUTCOME_UNKNOWN) ?? GIT_OUTCOME_UNKNOWN}\n`
          : "fatal: the CrazyAgent IDE relay is not reachable\n",
      );
    const timer = setTimeout(lost, GIT_REQUEST_MS + ANSWER_GRACE_MS);
    connection.on("connect", () => {
      connection.write(encodeFrame({ args, folder, secret: env[GIT_SECRET_ENV] }));
      sent = true;
    });
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
    connection.on("error", lost);
    connection.on("close", lost);
  });
}

if (require.main === module) {
  const env = { ...process.env };
  delete process.env[GIT_SECRET_ENV];
  void forward(env, process.argv.slice(2), process.cwd(), process.stdout, process.stderr).then((code) => {
    process.exitCode = code;
  });
}
