import assert from "node:assert/strict";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  CAPABILITY_ENV,
  Engine,
  FRAME_BYTES_MAX,
  FrameReader,
  SOCKET_ENV,
  encodeFrame,
  handshake,
  readCapability,
} from "../out/engine.js";
import { memoryStore, reading, withServer } from "./peer.mjs";

const fixture = JSON.parse(readFileSync(new URL("./frames.json", import.meta.url), "utf8"));

function withCapability(contents, check) {
  const root = mkdtempSync(join(tmpdir(), "ca-ide-ext-"));
  try {
    const folder = join(root, "ide", "alpha");
    mkdirSync(folder, { recursive: true });
    const path = join(folder, "capability");
    writeFileSync(path, contents, { mode: 0o600 });
    check(path);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("requests encode to exactly the engine's frame bytes", () => {
  assert.equal(fixture.requests.length, 5);
  for (const { value, frame } of fixture.requests) {
    assert.equal(encodeFrame(value).toString("hex"), frame);
  }
});

test("engine replies and events decode whole and from any chunking", () => {
  const stream = Buffer.concat(fixture.replies.map(({ frame }) => Buffer.from(frame, "hex")));
  const values = fixture.replies.map(({ value }) => value);
  assert.deepEqual(new FrameReader().push(stream), values);
  const bytewise = new FrameReader();
  const one = [];
  for (const byte of stream) one.push(...bytewise.push(Buffer.from([byte])));
  assert.deepEqual(one, values);
  for (let split = 1; split < stream.length; split += 1) {
    const reader = new FrameReader();
    const frames = [...reader.push(stream.subarray(0, split)), ...reader.push(stream.subarray(split))];
    assert.deepEqual(frames, values, `split at ${split}`);
  }
});

test("frames past the engine bound are refused both ways", () => {
  const largest = "x".repeat(FRAME_BYTES_MAX - 2);
  const frame = encodeFrame(largest);
  assert.equal(frame.length, FRAME_BYTES_MAX + 4);
  assert.deepEqual(new FrameReader().push(frame), [largest]);
  assert.throws(() => encodeFrame(`${largest}x`), /ide_frame_bounds/);
  for (const length of [0, FRAME_BYTES_MAX + 1]) {
    const header = Buffer.alloc(4);
    header.writeUInt32BE(length, 0);
    assert.throws(() => new FrameReader().push(header), /ide_frame_bounds/);
  }
  const invalid = Buffer.from([0, 0, 0, 3, 0x22, 0xff, 0x22]);
  assert.throws(() => new FrameReader().push(invalid), /ide_frame_invalid/);
});

test("the handshake is CAI1, slot zero and the 32-byte token", () => {
  assert.equal(handshake(Buffer.from(fixture.token, "hex")).toString("hex"), fixture.handshake);
  assert.throws(() => handshake(Buffer.alloc(31)), /ide_capability_invalid/);
});

test("a capability file holds exactly 64 lowercase hex digits", () => {
  withCapability(fixture.token, (path) => {
    assert.deepEqual(readCapability(path), Buffer.from(fixture.token, "hex"));
  });
  for (const contents of [`${fixture.token}\n`, fixture.token.toUpperCase(), fixture.token.slice(2), ""]) {
    withCapability(contents, (path) => {
      assert.throws(() => readCapability(path), /ide_capability_invalid/);
    });
  }
});

test("the engine bridge comes from the launch environment and names its project", () => {
  assert.equal(Engine.fromEnv({}), undefined);
  assert.equal(Engine.fromEnv({ [CAPABILITY_ENV]: "/a/ide/alpha/capability" }), undefined);
  assert.equal(Engine.fromEnv({ [SOCKET_ENV]: "/a/o00000000/owner.sock" }), undefined);
  const engine = Engine.fromEnv({
    [CAPABILITY_ENV]: "/a/ide/alpha/capability",
    [SOCKET_ENV]: "/a/o00000000/owner.sock",
  });
  assert.equal(engine.project, "alpha");
  engine.dispose();
});

test("the first call swaps the capability file for a session the bridge keeps", { timeout: 10_000 }, async () => {
  const { session, requests, replies } = fixture;
  const sessionHandshake = handshake(Buffer.from(session.token, "hex")).toString("hex");
  await withServer(async (server, socket, capability) => {
    let connections = 0;
    server.on("connection", () => (connections += 1));
    writeFileSync(capability, fixture.token, { mode: 0o600 });
    const store = memoryStore();
    const engine = new Engine(socket, capability, store);
    const swapped = once(server, "connection");
    const answered = engine.request("ide.workspace");
    const [swap] = await swapped;
    const swapping = reading(swap);
    assert.equal((await swapping.take(37)).toString("hex"), fixture.handshake);
    const called = once(server, "connection");
    assert.equal((await swapping.frame()).toString("hex"), session.request.frame);
    rmSync(capability);
    swap.end(Buffer.from(session.reply.frame, "hex"));
    const [call] = await called;
    const calling = reading(call);
    assert.equal((await calling.take(37)).toString("hex"), sessionHandshake);
    assert.equal((await calling.frame()).toString("hex"), requests[0].frame);
    call.end(Buffer.from(replies[0].frame, "hex"));
    assert.deepEqual(await answered, replies[0].value);
    assert.deepEqual([...store.values], [["crazyagent.session.alpha", session.token]]);

    const reloaded = new Engine(socket, capability, store);
    const again = once(server, "connection");
    const second = reloaded.request("ide.workspace");
    const [direct] = await again;
    const directly = reading(direct);
    assert.equal((await directly.take(37)).toString("hex"), sessionHandshake);
    assert.equal((await directly.frame()).toString("hex"), requests[0].frame);
    direct.end(Buffer.from(replies[2].frame, "hex"));
    assert.deepEqual(await second, replies[2].value);
    assert.equal(connections, 3);

    writeFileSync(capability, fixture.token, { mode: 0o600 });
    const late = new Engine(socket, capability, memoryStore());
    const raced = once(server, "connection");
    const refused = late.request("ide.workspace");
    const [taken] = await raced;
    const taking = reading(taken);
    await taking.take(37);
    assert.equal((await taking.frame()).toString("hex"), session.request.frame);
    taken.end(encodeFrame({ kind: "rejected", code: "ide_session_taken" }));
    await assert.rejects(refused, /ide_session_taken/);
    assert.ok(existsSync(capability));

    rmSync(capability);
    await assert.rejects(new Engine(socket, capability, memoryStore()).request("ide.workspace"), /ide_session_missing/);
    await assert.rejects(
      new Engine(socket, capability, memoryStore([["crazyagent.session.alpha", "not-hex"]])).request("ide.workspace"),
      /ide_session_missing/,
    );
    assert.equal(connections, 4);
  });
});
