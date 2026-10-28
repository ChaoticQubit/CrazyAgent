import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Engine, encodeFrame, FrameReader, handshake } from "../out/engine.js";
import { GIT_ARGS_MAX, GIT_FORWARD_ENV, GIT_NODE_ENV, GIT_SOCKET_ENV, gitCall, gitOutcome, startRelay } from "../out/git.js";
import { memoryStore, reading, withServer } from "./peer.mjs";

const forwarder = fileURLToPath(new URL("../out/git-forward.js", import.meta.url));
const TOKEN = "ab".repeat(32);

/** Runs the forwarder as the engine's git shim does: node, the script, then git's arguments. */
function forward(args, cwd, env) {
  return new Promise((resolve) => {
    execFile(process.execPath, [forwarder, ...args], { cwd, env: { PATH: process.env.PATH, ...env } }, (error, stdout, stderr) => {
      resolve({ code: error ? error.code : 0, stdout, stderr });
    });
  });
}

/** A private folder standing for the project folder git runs in, by its real path. */
function folder() {
  return realpathSync(mkdtempSync(join(tmpdir(), "ca-ide-git-")));
}

/** Runs `check` with a relay over a bridge whose session is already stored, then closes it. */
async function withRelay(check) {
  return withServer(async (server, socket, capability) => {
    const engine = new Engine(socket, capability, memoryStore([["crazyagent.session.alpha", TOKEN]]));
    const env = {};
    const relay = await startRelay(engine, env, forwarder);
    const project = folder();
    try {
      return await check(server, env, project);
    } finally {
      relay.dispose();
      engine.dispose();
      rmSync(project, { recursive: true, force: true });
      assert.equal(env[GIT_SOCKET_ENV], undefined, "a closed relay is no longer named");
    }
  });
}

/** Reads the one `ide.git` request the engine side receives on a connection. */
async function received(connection) {
  const peer = reading(connection);
  assert.equal((await peer.take(37)).toString("hex"), handshake(Buffer.from(TOKEN, "hex")).toString("hex"));
  return JSON.parse((await peer.frame()).subarray(4).toString("utf8"));
}

test("a push goes to the engine as ide.git and prints the engine's answer as git would", { timeout: 20_000 }, async () => {
  await withRelay(async (server, env, project) => {
    assert.equal(env[GIT_NODE_ENV], process.execPath);
    assert.equal(env[GIT_FORWARD_ENV], forwarder);
    assert.ok(env[GIT_SOCKET_ENV].startsWith(tmpdir()), env[GIT_SOCKET_ENV]);
    const called = once(server, "connection");
    const ran = forward(["push", "-u", "origin", "main:main"], project, env);
    const [connection] = await called;
    assert.deepEqual(await received(connection), {
      project: "alpha",
      call: { name: "ide.git", args: { args: ["push", "-u", "origin", "main:main"], folder: project } },
    });
    const stderr = "To git://127.0.0.1/remote.git\n ! [rejected]        main -> main (fetch first)\n";
    connection.end(encodeFrame({ kind: "ok", data: { code: 1, stdout: "", stderr } }));
    assert.deepEqual(await ran, { code: 1, stdout: "", stderr });
  });
});

const BARE_GIT = { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1" };

/** Runs git for the test's own setup with no owner configuration. Output: its trimmed output. */
function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, env: { PATH: process.env.PATH, ...BARE_GIT } }).toString().trim();
}

/** Commits one file in a repository. Inputs: the folder, file and text. Output: the commit. */
function commit(cwd, file, text) {
  writeFileSync(join(cwd, file), text);
  git(cwd, "add", file);
  git(cwd, "commit", "-q", "-m", `Write ${file}`);
  return git(cwd, "rev-parse", "HEAD");
}

/**
 * Stands in for the engine's side of a pull with real git: each `ide.git` call runs the
 * fetch the engine runs for it in the project and answers with its exit code, or answers
 * `failing` without fetching when that is set. Inputs: the server and the project.
 * Output: the calls it got and the `failing` switch.
 */
function fetchingEngine(server, project) {
  const engine = { calls: [], failing: undefined };
  server.on("connection", async (connection) => {
    const args = (await received(connection)).call.args.args;
    engine.calls.push(args);
    let code = engine.failing ?? 0;
    if (engine.failing === undefined) {
      const rest = args.slice(1).filter((arg) => !arg.startsWith("-") || arg === "--tags" || arg === "--unshallow");
      const fetch = ["fetch", "-q", ...rest, ...(rest.some((arg) => !arg.startsWith("-")) ? [] : ["origin"])];
      try {
        git(project, ...fetch);
      } catch (error) {
        code = error.status ?? 128;
      }
    }
    connection.end(encodeFrame({ kind: "ok", data: { code, stdout: "", stderr: "From upstream\n" } }));
  });
  return engine;
}

/**
 * Makes `upstream` a repository with one commit on `main`, and `project` a repository
 * whose `main` tracks `origin/main` from it. Inputs: the two folders. Output: none.
 */
function tracking(project, upstream) {
  for (const repository of [project, upstream]) {
    git(repository, "init", "-q", "-b", "main");
    git(repository, "config", "user.name", "Owner");
    git(repository, "config", "user.email", "owner@example.invalid");
    git(repository, "config", "commit.gpgsign", "false");
  }
  commit(upstream, "README.md", "# Seed shop\n");
  git(project, "remote", "add", "origin", upstream);
  git(project, "fetch", "-q", "origin");
  git(project, "reset", "-q", "--hard", "origin/main");
  git(project, "branch", "-q", "--set-upstream-to=origin/main", "main");
}

test("a pull merges or rebases in its own folder only what the engine fetched", { timeout: 30_000 }, async () => {
  await withRelay(async (server, env, project) => {
    const upstream = folder();
    try {
      tracking(project, upstream);
      const engine = fetchingEngine(server, project);
      const pull = async (args) => {
        const ran = await forward(args, project, { ...env, ...BARE_GIT });
        assert.deepEqual(engine.calls.at(-1), args);
        return ran;
      };

      const theirs = commit(upstream, "beds.md", "Six beds.\n");
      const forwarded = await pull(["pull", "--tags", "origin", "main"]);
      assert.equal(forwarded.code, 0, forwarded.stderr);
      assert.match(forwarded.stderr, /^From upstream\n/);
      assert.equal(git(project, "rev-parse", "HEAD"), theirs);

      const mine = commit(project, "notes.md", "Planting notes.\n");
      const next = commit(upstream, "seeds.md", "Tomatoes.\n");
      const diverged = await pull(["pull", "origin", "main"]);
      assert.notEqual(diverged.code, 0);
      assert.equal(git(project, "rev-parse", "HEAD"), mine, "divergent branches are not merged unasked");

      engine.failing = 1;
      const failed = await pull(["pull", "-r", "origin", "main"]);
      engine.failing = undefined;
      assert.equal(failed.code, 1);
      assert.equal(git(project, "rev-parse", "HEAD"), mine, "a failed fetch runs no local step");

      const rebased = await pull(["pull", "--autostash", "-r", "origin", "main"]);
      assert.equal(rebased.code, 0, rebased.stderr);
      assert.equal(git(project, "rev-parse", "HEAD~1"), next);
      assert.equal(git(project, "log", "-1", "--format=%s"), "Write notes.md");

      const later = commit(upstream, "paths.md", "Gravel.\n");
      git(project, "config", "pull.rebase", "false");
      const merged = await pull(["pull", "origin"]);
      assert.equal(merged.code, 0, merged.stderr);
      assert.equal(git(project, "rev-parse", "HEAD^2"), later);

      const ahead = commit(upstream, "water.md", "Daily.\n");
      git(project, "config", "pull.rebase", "true");
      const configured = await pull(["pull"]);
      assert.equal(configured.code, 0, configured.stderr);
      assert.equal(git(project, "rev-list", "--merges", "--count", `${ahead}..HEAD`), "0");
      assert.equal(git(project, "merge-base", "--is-ancestor", ahead, "HEAD"), "");
    } finally {
      rmSync(upstream, { recursive: true, force: true });
    }
  });
});

/**
 * Runs `check` with a project tracking `origin/main` of an upstream that also has the
 * branch `aaa-other`, and an engine that fetches for real. Input: the check, given a
 * pull function and the engine. Output: none.
 */
async function withUpstream(check) {
  await withRelay(async (server, env, project) => {
    const upstream = folder();
    try {
      tracking(project, upstream);
      git(upstream, "branch", "aaa-other");
      git(upstream, "switch", "-q", "aaa-other");
      commit(upstream, "other.md", "Another team's branch.\n");
      git(upstream, "switch", "-q", "main");
      const engine = fetchingEngine(server, project);
      await check((args) => forward(args, project, { ...env, ...BARE_GIT }), project, upstream, engine);
    } finally {
      rmSync(upstream, { recursive: true, force: true });
    }
  });
}

test("a pull of a branch with no upstream is refused, not rebased or merged onto another branch", { timeout: 30_000 }, async () => {
  await withUpstream(async (pull, project, upstream, engine) => {
    git(project, "switch", "-q", "-c", "topic");
    const topic = commit(project, "topic.md", "Unpublished work.\n");
    git(project, "config", "pull.rebase", "true");
    const rebasing = await pull(["pull"]);
    assert.equal(git(project, "rev-parse", "HEAD"), topic, "a branch with no upstream is never rebased onto another remote branch");
    assert.equal(rebasing.code, 1);
    assert.match(rebasing.stderr, /^There is no tracking information for the current branch\.\n/);

    git(project, "config", "--unset", "pull.rebase");
    const merging = await pull(["pull"]);
    assert.equal(merging.code, 1, "a pull with nothing to merge never reports success");
    assert.equal(git(project, "rev-parse", "HEAD"), topic);
    assert.match(merging.stderr, /^There is no tracking information for the current branch\.\n/);
    assert.equal(engine.calls.length, 0, "a refused pull asks the engine for nothing");
  });
});

test("a pull on a detached HEAD is refused, not rebased onto a remote branch", { timeout: 30_000 }, async () => {
  await withUpstream(async (pull, project, upstream, engine) => {
    const head = commit(project, "notes.md", "Planting notes.\n");
    git(project, "switch", "-q", "--detach", "HEAD");
    const detached = await pull(["pull", "-r"]);
    assert.equal(git(project, "rev-parse", "HEAD"), head, "a detached HEAD is never rebased onto a remote branch");
    assert.equal(detached.code, 1);
    assert.match(detached.stderr, /^You are not currently on a branch\.\n/);
    assert.equal(engine.calls.length, 0);
  });
});

test("a pull naming a remote branch other than the upstream is refused", { timeout: 30_000 }, async () => {
  await withUpstream(async (pull, project, upstream, engine) => {
    const main = git(project, "rev-parse", "HEAD");
    const elsewhere = await pull(["pull", "origin", "aaa-other"]);
    assert.equal(git(project, "rev-parse", "HEAD"), main, "a pull of another remote branch never moves this one");
    assert.equal(elsewhere.code, 1);
    assert.match(elsewhere.stderr, /only the upstream of 'main' \(refs\/remotes\/origin\/main\)/);
    assert.equal(engine.calls.length, 0);
  });
});

test("a refusal or a lost engine ends git with a fatal line and code 128", { timeout: 20_000 }, async () => {
  await withRelay(async (server, env, project) => {
    const called = once(server, "connection");
    const ran = forward(["fetch", "evil"], project, env);
    const [connection] = await called;
    await received(connection);
    connection.end(encodeFrame({ kind: "rejected", code: "ide_git_transport_refused" }));
    const refused = await ran;
    assert.equal(refused.code, 128);
    assert.equal(refused.stdout, "");
    assert.equal(refused.stderr, "fatal: CrazyAgent did not run this git command (ide_git_transport_refused)\n");

    const closing = once(server, "connection");
    const lost = forward(["fetch", "origin"], project, env);
    const [dropped] = await closing;
    await received(dropped);
    dropped.destroy();
    const closed = await lost;
    assert.equal(closed.code, 128);
    assert.match(closed.stderr, /^fatal: CrazyAgent did not run this git command \(ide_engine_closed\)\n$/);

    const odd = once(server, "connection");
    const unreadable = forward(["fetch", "origin"], project, env);
    const [strange] = await odd;
    await received(strange);
    strange.end(encodeFrame({ kind: "ok", data: { code: 999, stdout: "", stderr: "" } }));
    const answered = await unreadable;
    assert.equal(answered.code, 128);
    assert.match(answered.stderr, /unreadable answer/);
  });
});

test("before the extension names its relay, git network commands fail plainly", { timeout: 10_000 }, async () => {
  const project = folder();
  try {
    const ran = await forward(["push", "origin", "main"], project, {});
    assert.equal(ran.code, 128);
    assert.match(ran.stderr, /has not connected/);
  } finally {
    rmSync(project, { recursive: true, force: true });
  }
});

test("the relay refuses a malformed command without asking the engine", { timeout: 10_000 }, async () => {
  await withRelay(async (server, env) => {
    let asked = 0;
    server.on("connection", () => (asked += 1));
    for (const call of [
      { args: ["push", 7], folder: "/tmp/project" },
      { args: [], folder: "/tmp/project" },
      { args: ["push"], folder: "project" },
      { args: ["push"] },
    ]) {
      const connection = createConnection(env[GIT_SOCKET_ENV]);
      await once(connection, "connect");
      connection.write(encodeFrame(call));
      const reader = new FrameReader();
      const [chunk] = await once(connection, "data");
      assert.deepEqual(reader.push(chunk), [{ kind: "rejected", code: "ide_git_refused" }], JSON.stringify(call));
      connection.destroy();
    }
    assert.equal(asked, 0);
  });
});

test("forwarded commands and answers are bounded", () => {
  assert.deepEqual(gitCall({ args: ["push", "origin", "main"], folder: "/p" }), { args: ["push", "origin", "main"], folder: "/p" });
  for (const value of [
    null,
    "push",
    { args: "push", folder: "/p" },
    { args: Array(GIT_ARGS_MAX + 1).fill("fetch"), folder: "/p" },
    { args: ["push", ""], folder: "/p" },
    { args: ["push", "x".repeat(1025)], folder: "/p" },
    { args: ["push", "or\0igin"], folder: "/p" },
    { args: ["push"], folder: "p" },
    { args: ["push"], folder: `/${"a".repeat(4096)}` },
  ]) {
    assert.equal(gitCall(value), undefined, JSON.stringify(value)?.slice(0, 80));
  }
  assert.deepEqual(gitOutcome({ code: 0, stdout: "", stderr: "" }), { code: 0, stdout: "", stderr: "" });
  for (const value of [{ code: -1, stdout: "", stderr: "" }, { code: 256, stdout: "", stderr: "" }, { code: 1.5, stdout: "", stderr: "" }, { code: 0, stdout: 1, stderr: "" }, null]) {
    assert.equal(gitOutcome(value), undefined, JSON.stringify(value));
  }
});
