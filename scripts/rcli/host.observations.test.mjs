import assert from "node:assert/strict";
import test from "node:test";
import { SdkTurnProjection } from "./host.turns.ts";
import { encodeEvent } from "./host.codec.ts";
import { HostChannel } from "./host.runtime.ts";
import { PassThrough } from "node:stream";
import { once } from "node:events";
import { copyObservationValue } from "./host.observations.ts";

const tool = (event) => ({
  kind: "tool_observation", scope: event.type === "tool:requested"
    ? "explicit_sdk_turn" : "turn_unattributed", event,
});
const usage = (value) => ({
  kind: "usage_observation", scope: "turn_unattributed",
  provenance: "sdk_reported", accounting: "UNKNOWN", usage: value,
});
const packet = (body) => {
  const bytes = Buffer.from(encodeEvent("session", "instance", 2n, body));
  assert.equal(bytes.readUInt32BE(), bytes.length - 4);
  assert(bytes.length <= 65540);
  return JSON.parse(bytes.subarray(4).toString()).event;
};

test("pending tool observations survive without an invented turn", () => {
  const projection = new SdkTurnProjection();
  const event = { type: "tool:pending", toolCallId: "call/reused", name: "Read", inputSummary: "" };
  assert.deepEqual(projection.event(event), [{
    kind: "tool_observation", scope: "turn_unattributed", event,
  }]);
  assert.equal(projection.hasOutstanding, false);
});

test("all traced tool producers retain exact bounded fields through projection and codec", () => {
  const projection = new SdkTurnProjection();
  const events = [
    { type: "tool:requested", turnId: "native/turn", calls: [
      { id: "call/reused", name: "read", input: { path: "src/a", offset: 0 } },
    ] },
    { type: "tool:pending", toolCallId: "call/reused", name: "Read", inputSummary: "" },
    { type: "tool:start", toolCallId: "call/reused", name: "Read", inputSummary: "a" },
    { type: "tool:start", toolCallId: "agent", name: "Agent", inputSummary: "a", agentMode: "worker" },
    { type: "tool:retracted", toolCallId: "call/reused" },
    { type: "tool:result", toolCallId: "call/reused", name: "Read", title: "", detail: "", detailLines: [
      { key: "file", displayValue: "src/a", highlighted: false, kind: "neutral" },
    ], agentOutput: { mode: "worker", report: "done", status: "completed", rounds: 0 } },
    { type: "tool:error", toolCallId: "call/reused", name: "Read", error: "Permission denied" },
    { type: "agent:child_tool_start", agentToolCallId: "parent", call: {
      id: "child", name: "Read", inputSummary: "", status: "running",
    } },
    { type: "agent:child_tool_result", agentToolCallId: "parent", childToolCallId: "child", status: "done" },
    { type: "agent:child_tool_result", agentToolCallId: "parent", childToolCallId: "child", status: "error" },
  ];
  for (const event of events) {
    const expected = tool(event);
    assert.deepEqual(projection.event(event), [expected]);
    assert.deepEqual(packet(expected), expected);
  }
});

test("out of order, duplicate, retracted and reused IDs never acquire inferred correlation", () => {
  const projection = new SdkTurnProjection();
  const events = [
    { type: "tool:result", toolCallId: "same", name: "Read", title: "early result" },
    { type: "tool:pending", toolCallId: "same", name: "Read", inputSummary: "" },
    { type: "tool:pending", toolCallId: "same", name: "Read", inputSummary: "updated" },
    { type: "tool:start", toolCallId: "same", name: "Read", inputSummary: "old" },
    { type: "tool:retracted", toolCallId: "same" },
    { type: "tool:start", toolCallId: "same", name: "Read", inputSummary: "rerun" },
    { type: "tool:requested", turnId: "sdk-one", calls: [{ id: "same", name: "read", input: {} }] },
    { type: "tool:requested", turnId: "sdk-two", calls: [{ id: "same", name: "read", input: {} }] },
    { type: "tool:error", toolCallId: "same", name: "Read", error: "denied" },
  ];
  assert.deepEqual(events.flatMap((event) => projection.event(event)), events.map(tool));
  projection.stop();
  assert.deepEqual(projection.event(events[4]), [tool(events[4])]);
  assert.equal(projection.hasOutstanding, false);
});

test("usage keeps absence, zero, decimals and independent reports with UNKNOWN accounting", () => {
  const projection = new SdkTurnProjection();
  for (const value of [{}, { inputTokens: 0 }, {
    inputTokens: 0, outputTokens: 2, totalTokens: 0, cachedTokens: 0,
    cacheWriteTokens: 0, costUsd: 0.000125, contextTokens: 0, contextLimit: 0,
    model: "model/version", provider: "sdk-provider",
  }, { costUsd: 0 }, { totalTokens: 3 }, { totalTokens: 1 }]) {
    assert.deepEqual(projection.event({ type: "usage", usage: value }), [usage(value)]);
    assert.deepEqual(packet(usage(value)), usage(value));
  }
  assert.deepEqual(projection.event({ type: "usage", usage: {
    inputTokens: undefined, provider: undefined, costUsd: 0,
  } }), [usage({ costUsd: 0 })]);
});

test("unknown events and unqualified progress produce explicit bounded capture gaps", () => {
  const projection = new SdkTurnProjection();
  for (const event of [{ type: "sdk:new_event", value: "not retained" },
    { type: "system:warning", message: "not retained" }]) {
    const gap = { kind: "observation_gap", scope: "turn_unattributed",
      source_type: event.type, reason: "unsupported_event" };
    assert.deepEqual(projection.event(event), [gap]);
    assert.deepEqual(packet(gap), gap);
  }
  const gap = { kind: "observation_gap", scope: "turn_unattributed",
    source_type: "tool:progress", reason: "unqualified_progress", tool_call_id: "call" };
  assert.deepEqual(projection.event({ type: "tool:progress", toolCallId: "call", data: { arbitrary: 10 } }), [gap]);
  assert.deepEqual(packet(gap), gap);
});

test("malformed selected observations fail without partial output or consumed identity", () => {
  const projection = new SdkTurnProjection();
  const pending = { type: "tool:pending", toolCallId: "same", name: "Read", inputSummary: "" };
  for (const event of [null, [], {}, { type: 5 },
    { ...pending, turnId: "invented" }, { ...pending, toolCallId: "" },
    { ...pending, inputSummary: 1 }, { ...pending, name: "bad\ud800" },
    { type: "tool:requested", turnId: "native", calls: [{ id: "a", name: "read" }] },
    { type: "tool:requested", turnId: "native", calls: [] },
    { type: "tool:start", toolCallId: "a", name: "Read", inputSummary: "", agentMode: "approved" },
    { type: "tool:result", toolCallId: "a", name: "Read", title: "ok", detailLines: [{}] },
    { type: "tool:result", toolCallId: "a", name: "Read", title: "ok", agentOutput: {} },
    { type: "agent:child_tool_result", agentToolCallId: "parent", childToolCallId: "child", status: "running" },
    { type: "tool:progress", toolCallId: "", data: 1 },
  ]) assert.throws(() => projection.event(event), /host_observation_rejected/);
  assert.deepEqual(projection.event(pending), [tool(pending)]);
});

test("usage rejects invalid numeric values, extra identities and accounting guesses", () => {
  const projection = new SdkTurnProjection();
  for (const value of [null, [], { inputTokens: -1 }, { totalTokens: 1.5 },
    { cachedTokens: Number.MAX_SAFE_INTEGER + 1 }, { costUsd: NaN },
    { costUsd: Infinity }, { costUsd: -1 }, { inputTokens: "0" },
    { model: null }, { remainingQuota: 100 }, { accounting: "delta" }]) {
    assert.throws(() => projection.event({ type: "usage", usage: value }), /host_observation_rejected/);
  }
  assert.throws(() => projection.event({ type: "usage", usage: {}, turnId: "invented" }), /host_observation_rejected/);
});

test("wire validation refuses forged scope, authority fields and unsupported event payloads", () => {
  const valid = tool({ type: "tool:retracted", toolCallId: "same" });
  for (const body of [
    { ...valid, scope: "explicit_sdk_turn" }, { ...valid, turn_id: "invented" },
    { ...valid, approved: true }, { ...valid, event: { type: "sdk:new" } },
    { ...usage({}), accounting: "cumulative" }, { ...usage({}), provenance: "provider_verified" },
    { kind: "observation_gap", scope: "turn_unattributed", source_type: "tool:progress", reason: "unsupported_event" },
    { kind: "observation_gap", scope: "turn_unattributed", source_type: "usage", reason: "unqualified_progress", tool_call_id: "a" },
  ]) assert.throws(() => encodeEvent("session", "instance", 2n, body), /host_event_rejected/);
});

test("bounded payloads reject oversized, deep, cyclic and executable objects before serialization", () => {
  const projection = new SdkTurnProjection();
  const request = (input) => ({ type: "tool:requested", turnId: "native", calls: [{ id: "a", name: "read", input }] });
  const cycle = {}; cycle.self = cycle;
  let deep = {}; for (let i = 0; i < 10; i++) deep = { deep };
  for (const input of ["x".repeat(16385), cycle, deep, Array(65).fill(0),
    { toJSON() { throw new Error("must not execute"); } },
    { a: "x".repeat(16384), b: "x".repeat(16384) },
    { value: 1n }, new Date(), { value: undefined }]) {
    assert.throws(() => projection.event(request(input)), /host_observation_rejected/);
  }
  const accessor = {};
  Object.defineProperty(accessor, "type", { enumerable: true, get() { throw new Error("must not execute"); } });
  assert.throws(() => projection.event(accessor), /host_observation_rejected/);
  assert.throws(() => projection.event({ type: "tool:requested", turnId: "native", calls:
    Array.from({ length: 33 }, (_, i) => ({ id: String(i), name: "read", input: {} })),
  }), /host_observation_rejected/);
  assert.throws(() => projection.event({ type: "tool:pending", toolCallId: "x".repeat(257), name: "read", inputSummary: "" }), /host_observation_rejected/);
});

test("normalization copies caller data and preserves opaque JSON keys without prototype mutation", () => {
  const projection = new SdkTurnProjection();
  const event = { type: "tool:requested", turnId: "native", calls: [{
    id: "__proto__", name: "read", input: JSON.parse('{"__proto__":{"safe":true},"zero":0}'),
  }] };
  const frames = projection.event(event);
  event.calls[0].input.zero = 99;
  assert.equal(frames[0].event.calls[0].input.zero, 0);
  assert.deepEqual(packet(frames[0]), frames[0]);
  assert.equal(Object.prototype.safe, undefined);
});

test("the real channel writes observations with bound session, instance and contiguous sequence", async () => {
  const output = new PassThrough();
  const chunks = [];
  output.on("data", (chunk) => chunks.push(chunk));
  const channel = new HostChannel(output, "session", "instance");
  await channel.ready();
  const event = { type: "tool:retracted", toolCallId: "same" };
  channel.observe(event);
  channel.observe({ type: "usage", usage: { costUsd: 0, inputTokens: 0 } });
  await channel.close();
  const bytes = Buffer.concat(chunks);
  const packets = [];
  for (let offset = 0; offset < bytes.length;) {
    const length = bytes.readUInt32BE(offset);
    packets.push(JSON.parse(bytes.subarray(offset + 4, offset + 4 + length).toString()));
    offset += length + 4;
  }
  assert.deepEqual(packets.map((value) => value.event), [{ kind: "ready" }, tool(event), usage({ costUsd: 0, inputTokens: 0 })]);
  assert.deepEqual(packets.map((value) => [value.session_id, value.instance_id, value.sequence]), [
    ["session", "instance", 1], ["session", "instance", 2], ["session", "instance", 3],
  ]);
  assert.equal(channel.pending.size, 0);
});

test("malformed SDK observations make the real channel fail explicitly", async () => {
  const output = new PassThrough(); output.resume();
  const channel = new HostChannel(output, "session", "instance");
  await channel.ready();
  channel.observe({ type: "usage", usage: { totalTokens: Infinity } });
  assert.equal(channel.signal.aborted, true);
  assert.equal((await channel.failure).message, "host_callback_failed");
  await assert.rejects(channel.close(), /host_callback_failed/);
  assert.equal(channel.pending.size, 0);
});

test("observation bursts obey the real output backpressure bound and drain handles", async () => {
  const output = new PassThrough({ highWaterMark: 128 });
  const channel = new HostChannel(output, "session", "instance");
  const ready = channel.ready();
  await once(output, "readable"); output.read(); await ready;
  for (let i = 0; i < 100; i++) channel.observe({
    type: "tool:error", toolCallId: "same", name: "Read", error: "x".repeat(16384),
  });
  assert(channel.pending.size <= 64);
  assert.equal(channel.signal.aborted, true);
  assert.equal((await channel.failure).message, "host_output_failed");
  await assert.rejects(channel.close(), /host_output_failed/);
  assert.equal(channel.pending.size, 0);
});

test("JSON budgets accept their exact byte and node limits and reject the next value", () => {
  const exactBytes = ["x".repeat(16384), "y".repeat(16377)];
  assert.equal(Buffer.byteLength(JSON.stringify(exactBytes)), 32768);
  assert.deepEqual(copyObservationValue(exactBytes), exactBytes);
  assert.throws(() => copyObservationValue([exactBytes[0], exactBytes[1] + "y"]), /host_observation_rejected/);
  const exactNodes = Array.from({ length: 64 }, (_, index) => Array(index === 63 ? 14 : 15).fill(0));
  const budget = { nodes: 0, bytes: 0 };
  assert.deepEqual(copyObservationValue(exactNodes, budget), exactNodes);
  assert.equal(budget.nodes, 1024);
  exactNodes[63].push(0);
  assert.throws(() => copyObservationValue(exactNodes), /host_observation_rejected/);
});

test("internal copy budget and depth corruption trip real production assertions", () => {
  for (const budget of [{ nodes: -1, bytes: 0 }, { nodes: 1025, bytes: 0 },
    { nodes: 0, bytes: -1 }, { nodes: 0, bytes: 32769 }, { nodes: 0.5, bytes: 0 }]) {
    assert.throws(() => copyObservationValue(null, budget), /host_observation_budget_invariant/);
  }
  for (const depth of [-1, 10, 0.5]) {
    assert.throws(() => copyObservationValue(null, { nodes: 0, bytes: 0 }, depth), /host_observation_depth_invariant/);
  }
});

test("active and steering turns do not attribute SDK events that have no turn ID", () => {
  const projection = new SdkTurnProjection();
  const control = (id, kind) => ({ protocol_version: 1, session_id: "session",
    request_id: id, control: { kind, turn_id: id, text: "data" } });
  projection.admit(control("one", "prompt")); projection.start("one");
  projection.event({ type: "turn:start", turnId: "sdk-one" });
  projection.admit(control("two", "steer"));
  const event = { type: "tool:pending", toolCallId: "same", name: "Read", inputSummary: "" };
  assert.deepEqual(projection.event(event), [tool(event)]);
  assert.deepEqual(projection.event({ type: "usage", usage: { inputTokens: 0 } }), [usage({ inputTokens: 0 })]);
  projection.close();
  assert.throws(() => projection.event(event), /host_turn_rejected/);
});

test("a rejected frame does not consume the writer sequence or claim a capture success", async () => {
  const { BoundedEventWriter } = await import("./host.codec.ts");
  const output = new PassThrough(); const chunks = [];
  output.on("data", (chunk) => chunks.push(chunk));
  const writer = new BoundedEventWriter(output, "session", "instance");
  await writer.enqueue({ kind: "ready" });
  await assert.rejects(writer.enqueue({ ...usage({}), accounting: "delta" }), /host_event_rejected/);
  await writer.enqueue(usage({ inputTokens: 0 }));
  await writer.close();
  assert.equal(chunks.length, 2);
  assert.equal(JSON.parse(chunks[1].subarray(4).toString()).sequence, 2);
});
