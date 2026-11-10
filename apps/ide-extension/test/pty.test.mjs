import assert from "node:assert/strict";
import { once } from "node:events";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { Engine, encodeFrame } from "../out/engine.js";
import { BUSY, INPUT_BYTES_MAX, SandboxedPty, UNAVAILABLE, clampSize, gitFirstOnPath, stopped } from "../out/pty.js";
import { memoryStore, reading, withServer } from "./peer.mjs";

const fixture = JSON.parse(readFileSync(new URL("./frames.json", import.meta.url), "utf8"));
const terminal = fixture.terminal;
const bytes = (entry) => Buffer.from(entry.frame, "hex");
const LIMIT = { timeout: 10_000 };

test("the terminals the server hosts find the folder of the engine's git first on PATH", () => {
  const prepended = [];
  gitFirstOnPath({ prepend: (name, value) => prepended.push([name, value]) }, "/engine/data/Machine/git");
  assert.deepEqual(prepended, [["PATH", "/engine/data/Machine:"]]);
});

test("a git.path that is not an absolute path, or a folder PATH cannot hold, leaves PATH alone", () => {
  const prepended = [];
  const collection = { prepend: (name, value) => prepended.push([name, value]) };
  for (const value of [undefined, 7, "git", "/engine:data/Machine/git"]) gitFirstOnPath(collection, value);
  assert.deepEqual(prepended, []);
});

async function withSession(check) {
  await withServer(async (server, socket, capability) => {
    const store = memoryStore([["crazyagent.session.alpha", fixture.token]]);
    await check(new Engine(socket, capability, store), server, socket);
  });
}

async function until(done) {
  while (!done()) await new Promise((resolve) => setTimeout(resolve, 5));
}

function watched(pty) {
  const seen = { shown: [], closed: 0 };
  pty.onDidWrite((text) => seen.shown.push(text));
  pty.onDidClose(() => (seen.closed += 1));
  return seen;
}

async function opened(engine, server, size) {
  const pty = new SandboxedPty((name, args) => engine.stream(name, args), "sandboxed");
  const seen = watched(pty);
  const accepted = once(server, "connection");
  pty.open(size);
  const [connection] = await accepted;
  const gone = once(connection, "close");
  const peer = reading(connection);
  assert.equal((await peer.take(37)).toString("hex"), fixture.handshake);
  return { pty, seen, connection, gone, peer };
}

test("the Sandboxed terminal opens, types, resizes and closes with the engine's frames", LIMIT, async () => {
  await withSession(async (engine, server) => {
    const { pty, seen, connection, gone, peer } = await opened(engine, server, { columns: 80, rows: 24 });
    assert.equal((await peer.frame()).toString("hex"), terminal.open.frame);
    const [ok, first, second, end] = terminal.frames.map(bytes);
    const stream = Buffer.concat([ok, first, second]);
    for (let at = 0; at < stream.length; at += 5) connection.write(stream.subarray(at, at + 5));
    pty.handleInput("ls ✓\r");
    assert.equal((await peer.frame()).toString("hex"), terminal.requests[0].frame);
    pty.setDimensions({ columns: 120, rows: 40 });
    assert.equal((await peer.frame()).toString("hex"), terminal.requests[1].frame);
    await until(() => seen.shown.join("") === "café ✓\r\n");
    assert.ok(!seen.shown.join("").includes("�"));
    connection.write(end);
    await until(() => seen.closed === 1);
    pty.handleInput("after");
    pty.setDimensions({ columns: 90, rows: 30 });
    pty.close();
    await gone;
    assert.equal(peer.left(), 0);
    assert.equal(seen.shown.join(""), "café ✓\r\n");
    assert.equal(seen.closed, 1);
  });
});

test("closing detaches, and long typing goes in frames the engine accepts", LIMIT, async () => {
  await withSession(async (engine, server) => {
    const { pty, seen, connection, gone, peer } = await opened(engine, server, { columns: 1, rows: 5000 });
    const open = JSON.parse((await peer.frame()).subarray(4).toString("utf8"));
    assert.deepEqual(open.call, { name: "ide.terminal", args: { cols: 2, rows: 1000, level: "sandboxed" } });
    connection.write(bytes(terminal.frames[0]));
    pty.handleInput("x".repeat(INPUT_BYTES_MAX + 1));
    for (const length of [INPUT_BYTES_MAX, 1]) {
      const sent = JSON.parse((await peer.frame()).subarray(4).toString("utf8"));
      assert.equal(sent.kind, "input");
      assert.equal(sent.bytes.length, length);
      assert.ok(sent.bytes.every((byte) => byte === 0x78));
    }
    pty.close();
    assert.equal((await peer.frame()).toString("hex"), terminal.requests[2].frame);
    await gone;
    assert.equal(peer.left(), 0);
    assert.deepEqual(seen, { shown: [], closed: 0 });
  });
});

test("a refused or broken terminal stays open and says why", LIMIT, async () => {
  await withSession(async (engine, server, socket) => {
    const refused = await opened(engine, server, undefined);
    const open = JSON.parse((await refused.peer.frame()).subarray(4).toString("utf8"));
    assert.deepEqual(open.call.args, { cols: 80, rows: 24, level: "sandboxed" });
    refused.connection.write(encodeFrame({ kind: "rejected", code: "ide_terminal_unavailable" }));
    await until(() => refused.seen.shown.length === 1);
    assert.deepEqual(refused.seen, { shown: [`\r\n${UNAVAILABLE}\r\n`], closed: 0 });
    await refused.gone;

    const dropped = await opened(engine, server, undefined);
    await dropped.peer.frame();
    dropped.connection.write(bytes(terminal.frames[0]));
    dropped.connection.end();
    await until(() => dropped.seen.shown.length === 1);
    assert.deepEqual(dropped.seen, { shown: ["\r\nThis terminal stopped (ide_engine_closed).\r\n"], closed: 0 });

    const unready = new Engine(socket, join(socket, "missing"), memoryStore());
    const sessionless = new SandboxedPty((name, args) => unready.stream(name, args));
    const seen = watched(sessionless);
    sessionless.open(undefined);
    sessionless.handleInput("ignored");
    await until(() => seen.shown.length === 1);
    assert.deepEqual(seen, { shown: ["\r\nThis terminal stopped (ide_session_missing).\r\n"], closed: 0 });
  });
});

test("sizes stay in the engine's bounds and stop reasons stay plain", () => {
  assert.deepEqual(clampSize(undefined), { columns: 80, rows: 24 });
  assert.deepEqual(clampSize({ columns: 1, rows: 5000 }), { columns: 2, rows: 1000 });
  assert.deepEqual(clampSize({ columns: Number.NaN, rows: 30.7 }), { columns: 80, rows: 30 });
  assert.equal(stopped("ide_terminal_unavailable"), UNAVAILABLE);
  assert.equal(stopped("ide_terminals_busy"), BUSY);
  assert.equal(stopped("ide_capability_revoked"), "This terminal stopped (ide_capability_revoked).");
  assert.equal(stopped("x\u001b[2J"), "This terminal stopped (unknown).");
  assert.ok(!UNAVAILABLE.includes("IDE shell"), UNAVAILABLE);
  assert.equal(UNAVAILABLE, "This terminal is unavailable: this system has no qualified sandbox for it.");
  assert.equal(BUSY, "This terminal can't start: too many are open. Close one, then open a new terminal.");
  assert.equal(stopped("ide_terminal_above_project"), "Raise project access to use this");
});

test("a terminal asks the engine for its profile's level and shows a refusal to raise access", LIMIT, async () => {
  const asked = [];
  const pty = new SandboxedPty((name, args) => {
    asked.push([name, args]);
    return Promise.reject(new Error("ide_terminal_above_project"));
  }, "own_branch");
  const seen = watched(pty);
  pty.open({ columns: 80, rows: 24 });
  await new Promise((done) => setImmediate(done));
  assert.deepEqual(asked, [["ide.terminal", { cols: 80, rows: 24, level: "own_branch" }]]);
  assert.ok(seen.shown.join("").includes("Raise project access to use this"), seen.shown.join(""));
});
