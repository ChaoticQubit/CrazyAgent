import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
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

    const pulled = once(server, "connection");
    const pulling = forward(["pull", "--tags", "origin", "main"], project, env);
    const [again] = await pulled;
    assert.deepEqual((await received(again)).call.args.args, ["pull", "--tags", "origin", "main"]);
    again.end(encodeFrame({ kind: "ok", data: { code: 0, stdout: "Already up to date.\n", stderr: "" } }));
    assert.deepEqual(await pulling, { code: 0, stdout: "Already up to date.\n", stderr: "" });
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
