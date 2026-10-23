import assert from "node:assert/strict";
import { PassThrough, Readable } from "node:stream";
import { once } from "node:events";
import { chmod, mkdir, mkdtemp, realpath, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { readControls } from "./host.codec.ts";
import { SdkTurnProjection } from "./host.turns.ts";
import { HostChannel, admitLaunch, parseLaunch, within } from "./host.runtime.ts";

const control = (id, body) => ({ protocol_version: 1, session_id: "session", request_id: id, control: body });
function frame(value) {
  const body = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length);
  return Buffer.concat([header, body]);
}

test("stdin staging rejects an oversized actual stream chunk before decoding valid controls", async () => {
  const bytes = Buffer.concat(Array(700).fill(frame(control("shutdown", { kind: "shutdown" }))));
  assert(bytes.length > 65540);
  const source = Readable.from([bytes], { objectMode: false });
  await assert.rejects(async () => {
    for await (const value of readControls(source, "session")) assert.equal(value.control.kind, "shutdown");
  }, /host_control_rejected/);
});

test("an actual SDK abort-response shape is accepted only for a cancelling turn", () => {
  const projection = new SdkTurnProjection();
  projection.admit(control("prompt", { kind: "prompt", turn_id: "turn", text: "unit data" }));
  projection.start("turn");
  projection.event({ type: "turn:start", turnId: "sdk-turn" });
  projection.event({ type: "input:request", request: { id: "ask", questions: [{ question: "Choose", options: [] }] } });
  assert.throws(() => projection.event({ type: "input:response", response: { id: "ask", answers: [] } }), /host_turn_rejected/);
  projection.admit(control("cancel", { kind: "cancel", turn_id: "turn" }));
  assert.throws(() => projection.event({ type: "input:response", response: { id: "foreign", answers: [] } }), /host_turn_rejected/);
  assert.throws(() => projection.event({ type: "input:response", response: { id: "ask", answers: ["invented"] } }), /host_turn_rejected/);
  assert.deepEqual(projection.event({ type: "input:response", response: { id: "ask", answers: [] } }), []);
  assert.throws(() => projection.event({ type: "input:response", response: { id: "ask", answers: [] } }), /host_turn_rejected/);
  projection.event({ type: "turn:cancelled", turnId: "sdk-turn" });
  projection.event({ type: "turn:end", turnId: "sdk-turn", status: "cancelled", durationMs: 1 });
  assert.deepEqual(projection.complete("turn", "cancelled"), [{ kind: "completed", turn_id: "turn", status: "cancelled" }]);
});

const argv = ["--session-id", "session", "--instance-id", "instance", "--home-dir", "/private/entry/home", "--cwd", "/private/entry/project"];
function outputChannel() {
  const output = new PassThrough();
  const bytes = [];
  output.on("data", (chunk) => bytes.push(chunk));
  return { output, bytes, channel: new HostChannel(output, "session", "instance") };
}
function packets(chunks) {
  const joined = Buffer.concat(chunks);
  const result = [];
  for (let offset = 0; offset < joined.length;) {
    const length = joined.readUInt32BE(offset);
    assert(length > 0 && length <= 65536 && offset + 4 + length <= joined.length);
    result.push(JSON.parse(joined.subarray(offset + 4, offset + 4 + length).toString("utf8")));
    offset += length + 4;
  }
  return result;
}

test("launch accepts only exact identities and explicit absolute private paths", () => {
  assert.deepEqual(parseLaunch(argv), { sessionId: "session", instanceId: "instance", homeDir: "/private/entry/home", cwd: "/private/entry/project" });
  for (const args of [[], [...argv, "--model", "anything"], [...argv.slice(0, 6), "--home-dir", "/private/entry/project"], argv.map((x) => x === "session" ? ".." : x), argv.map((x) => x === "/private/entry/home" ? "relative" : x), argv.map((x) => x === "/private/entry/home" ? "/private/entry/../home" : x), argv.map((x) => x === "/private/entry/home" ? "/private/entry/\u0000home" : x)]) {
    assert.throws(() => parseLaunch(args), /host_launch_rejected/);
  }
});

test("launch admission checks real owned directories without loading the SDK", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "rcli-entry-")));
  try {
    await chmod(root, 0o700);
    const homeDir = join(root, "home");
    const cwd = join(root, "project");
    await mkdir(homeDir, { mode: 0o700 });
    await mkdir(cwd, { mode: 0o700 });
    await mkdir(join(cwd, ".git"), { mode: 0o700 });
    const launch = { sessionId: "session", instanceId: "instance", homeDir, cwd };
    await admitLaunch(launch, homeDir);
    await assert.rejects(admitLaunch(launch, cwd), /host_launch_rejected/);
    await chmod(homeDir, 0o755);
    await assert.rejects(admitLaunch(launch, homeDir), /host_launch_rejected/);
    await chmod(homeDir, 0o700);
    const alias = join(root, "alias");
    await symlink(homeDir, alias);
    await assert.rejects(admitLaunch({ ...launch, homeDir: alias }, alias), /host_launch_rejected/);
    await assert.rejects(admitLaunch({ ...launch, cwd: homeDir }, homeDir), /host_launch_rejected/);
  } finally { await rm(root, { recursive: true }); }
});

test("channel emits no Ready during construction and rejects controls before startup", async () => {
  const { channel, bytes } = outputChannel();
  assert.deepEqual(bytes, []);
  assert.throws(() => channel.receive(control("early", { kind: "shutdown" })), /host_not_ready/);
  await channel.ready();
  assert.deepEqual(packets(bytes).map((p) => p.event), [{ kind: "ready" }]);
  const first = channel.close();
  assert.equal(channel.close(), first);
  await first;
});

test("channel keeps controls responsive and maps old and successor completions separately", async () => {
  const { channel, bytes } = outputChannel();
  await channel.ready();
  assert.equal(channel.receive(control("one", { kind: "prompt", turn_id: "one", text: "data" })).kind, "submit");
  channel.start("one");
  channel.observe({ type: "turn:start", turnId: "sdk-one" });
  assert.equal(channel.receive(control("two", { kind: "steer", turn_id: "two", text: "data" })).kind, "submit");
  channel.observe({ type: "turn:end", turnId: "sdk-one", status: "cancelled", durationMs: 1 });
  channel.start("two");
  channel.observe({ type: "turn:start", turnId: "sdk-two" });
  channel.complete("one", "cancelled");
  channel.observe({ type: "assistant:message", turnId: "sdk-two", messageId: "message", text: "final" });
  channel.observe({ type: "turn:end", turnId: "sdk-two", status: "completed", durationMs: 1 });
  channel.complete("two", "completed");
  await channel.close();
  const frames = packets(bytes);
  assert.deepEqual(frames.filter((p) => p.event.kind === "completed").map((p) => p.event), [{ kind: "completed", turn_id: "one", status: "cancelled" }, { kind: "completed", turn_id: "two", status: "succeeded" }]);
  assert.deepEqual(frames.map((p) => p.sequence), frames.map((_, i) => i + 1));
});

test("callback failures become observable fatal state without throwing into an EventBus", async () => {
  const { channel } = outputChannel();
  await channel.ready();
  assert.doesNotThrow(() => channel.observe({ type: "turn:start", turnId: "foreign" }));
  const failure = await channel.failure;
  assert.equal(failure.message, "host_callback_failed");
  assert.equal(channel.signal.aborted, true);
  channel.fail("host_output_failed");
  assert.equal(channel.signal.reason, failure);
  await assert.rejects(channel.close(), /host_callback_failed/);
});

test("actual closed output propagates outside synchronous callbacks", async () => {
  const { channel, output } = outputChannel();
  await channel.ready();
  output.destroy();
  assert.equal((await channel.failure).message, "host_output_failed");
  await assert.rejects(channel.close(), /host_output_failed/);
});

test("stalled real output bounds callback work including in-flight frames", async () => {
  const output = new PassThrough({ highWaterMark: 128 });
  const channel = new HostChannel(output, "session", "instance");
  const ready = channel.ready();
  await once(output, "readable");
  assert.equal(packets([output.read()])[0].event.kind, "ready");
  await ready;
  channel.receive(control("prompt", { kind: "prompt", turn_id: "turn", text: "data" }));
  channel.start("turn");
  channel.observe({ type: "turn:start", turnId: "sdk-turn" });
  for (let i = 0; i < 1000; i++) channel.observe({ type: "assistant:delta", turnId: "sdk-turn", messageId: "message", text: "x".repeat(16384) });
  assert.equal(channel.signal.aborted, true);
  assert(channel.pending.size <= 64);
  await assert.rejects(channel.close(), /host_output_failed/);
});

test("wire bytes trigger failure below the callback count bound", async () => {
  const output = new PassThrough({ highWaterMark: 128 });
  const channel = new HostChannel(output, "session", "instance");
  const ready = channel.ready();
  await once(output, "readable");
  output.read(); await ready;
  channel.receive(control("prompt", { kind: "prompt", turn_id: "turn", text: "data" })); channel.start("turn");
  channel.observe({ type: "turn:start", turnId: "sdk-turn" });
  for (let i = 0; i < 40; i++) channel.observe({ type: "assistant:delta", turnId: "sdk-turn", messageId: "message", text: '"'.repeat(16384) });
  assert.equal((await channel.failure).message, "host_output_failed");
  assert(channel.pending.size < 64);
  await assert.rejects(channel.close(), /host_output_failed/);
});

test("channel invariants are runnable assertions", async () => {
  const { channel } = outputChannel();
  await channel.ready();
  for (let i = 0; i < 65; i++) channel.pending.add(Promise.resolve());
  assert.throws(() => channel.receive(control("stop", { kind: "shutdown" })), /host_channel_invariant/);
  channel.pending.clear();
  await channel.close();
});

test("bounded waits use real completion and real deadlines", async () => {
  assert.equal(await within(Promise.resolve("actual"), 100), "actual");
  await assert.rejects(within(new Promise(() => {}), 5), /host_deadline/);
});
