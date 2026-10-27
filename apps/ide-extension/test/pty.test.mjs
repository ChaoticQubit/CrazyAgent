import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { Engine, encodeFrame } from "../out/engine.js";
import { BUSY, INPUT_BYTES_MAX, SandboxedPty, UNAVAILABLE, clampSize, stopped } from "../out/pty.js";

const fixture = JSON.parse(readFileSync(new URL("./frames.json", import.meta.url), "utf8"));
const terminal = fixture.terminal;
const bytes = (entry) => Buffer.from(entry.frame, "hex");
const LIMIT = { timeout: 10_000 };

async function withServer(check) {
  const root = mkdtempSync(join(tmpdir(), "ca-ide-pty-"));
  const server = createServer();
  try {
    mkdirSync(join(root, "ide", "alpha"), { recursive: true });
    const capability = join(root, "ide", "alpha", "capability");
    writeFileSync(capability, fixture.token, { mode: 0o600 });
    const socket = join(root, "owner.sock");
    server.listen(socket);
    await once(server, "listening");
    await check(new Engine(socket, capability), server, socket);
  } finally {
    server.close();
    rmSync(root, { recursive: true, force: true });
  }
}

function reading(connection) {
  let pending = Buffer.alloc(0);
  let wake = () => undefined;
  connection.on("data", (chunk) => {
    pending = Buffer.concat([pending, chunk]);
    wake();
  });
  const take = async (count) => {
    while (pending.length < count) await new Promise((resolve) => (wake = resolve));
    const taken = pending.subarray(0, count);
    pending = pending.subarray(count);
    return taken;
  };
  return {
    take,
    async frame() {
      const header = await take(4);
      return Buffer.concat([header, await take(header.readUInt32BE(0))]);
    },
    left: () => pending.length,
  };
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
  const pty = new SandboxedPty((name, args) => engine.stream(name, args));
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
  await withServer(async (engine, server) => {
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
  await withServer(async (engine, server) => {
    const { pty, seen, connection, gone, peer } = await opened(engine, server, { columns: 1, rows: 5000 });
    const open = JSON.parse((await peer.frame()).subarray(4).toString("utf8"));
    assert.deepEqual(open.call, { name: "ide.terminal", args: { cols: 2, rows: 1000 } });
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
  await withServer(async (engine, server, socket) => {
    const refused = await opened(engine, server, undefined);
    const open = JSON.parse((await refused.peer.frame()).subarray(4).toString("utf8"));
    assert.deepEqual(open.call.args, { cols: 80, rows: 24 });
    refused.connection.write(encodeFrame({ kind: "rejected", code: "ide_terminal_unavailable" }));
    await until(() => refused.seen.shown.length === 1);
    assert.deepEqual(refused.seen, { shown: [`\r\n${UNAVAILABLE}\r\n`], closed: 0 });
    await refused.gone;

    const dropped = await opened(engine, server, undefined);
    await dropped.peer.frame();
    dropped.connection.write(bytes(terminal.frames[0]));
    dropped.connection.end();
    await until(() => dropped.seen.shown.length === 1);
    assert.deepEqual(dropped.seen, { shown: ["\r\nThe Sandboxed terminal stopped (ide_engine_closed).\r\n"], closed: 0 });

    const unreadable = new SandboxedPty((name, args) => new Engine(socket, join(socket, "missing")).stream(name, args));
    const seen = watched(unreadable);
    unreadable.open(undefined);
    unreadable.handleInput("ignored");
    assert.deepEqual(seen, { shown: ["\r\nThe Sandboxed terminal stopped (ide_capability_invalid).\r\n"], closed: 0 });
  });
});

test("sizes stay in the engine's bounds and stop reasons stay plain", () => {
  assert.deepEqual(clampSize(undefined), { columns: 80, rows: 24 });
  assert.deepEqual(clampSize({ columns: 1, rows: 5000 }), { columns: 2, rows: 1000 });
  assert.deepEqual(clampSize({ columns: Number.NaN, rows: 30.7 }), { columns: 80, rows: 30 });
  assert.equal(stopped("ide_terminal_unavailable"), UNAVAILABLE);
  assert.equal(stopped("ide_terminals_busy"), BUSY);
  assert.equal(stopped("ide_capability_revoked"), "The Sandboxed terminal stopped (ide_capability_revoked).");
  assert.equal(stopped("x\u001b[2J"), "The Sandboxed terminal stopped (unknown).");
  assert.ok(UNAVAILABLE.includes('"IDE shell"'));
});
