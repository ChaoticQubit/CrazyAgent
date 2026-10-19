import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { test } from "node:test";
import { BoundedEventWriter, decodeControl, encodeEvent, readControls } from "./host.codec.ts";
import { SdkMessageProjection } from "./host.messages.ts";

function framed(value) {
  const payload = Buffer.from(JSON.stringify(value));
  const header = Buffer.alloc(4);
  header.writeUInt32BE(payload.byteLength);
  return Buffer.concat([header, payload]);
}

function unpack(bytes) {
  const buffer = Buffer.from(bytes);
  assert.equal(buffer.readUInt32BE(), buffer.byteLength - 4);
  return JSON.parse(buffer.subarray(4).toString("utf8"));
}

test("extraction preserves the existing closed event codec", () => {
  for (const body of [
    { kind: "ready" },
    { kind: "output", turn_id: "turn-a", text: "answer 🙂" },
    { kind: "input", turn_id: "turn-a", prompt_id: "prompt-a", question: "Choose", choices: ["A", "B"] },
    { kind: "completed", turn_id: "turn-a", status: "succeeded" },
  ]) {
    assert.deepEqual(unpack(encodeEvent("session-a", "instance-a", 1n, body)), {
      protocol_version: 1, session_id: "session-a", instance_id: "instance-a", sequence: 1, event: body,
    });
  }
});

test("extraction preserves actual fragmented control decoding", async () => {
  const control = {
    protocol_version: 1, session_id: "session-a", request_id: "request-a",
    control: { kind: "prompt", turn_id: "turn-a", text: "Inspect" },
  };
  const bytes = framed(control);
  assert.deepEqual(decodeControl(bytes.subarray(4), "session-a"), control);
  async function* source() {
    yield bytes.subarray(0, 1);
    yield bytes.subarray(1, 7);
    yield bytes.subarray(7);
  }
  const decoded = [];
  for await (const value of readControls(source(), "session-a")) decoded.push(value);
  assert.deepEqual(decoded, [control]);
});

test("the extracted writer preserves ordered frames on a real Node stream", async () => {
  const destination = new PassThrough();
  const bytes = [];
  destination.on("data", (chunk) => bytes.push(chunk));
  const writer = new BoundedEventWriter(destination, "session-a", "instance-a");
  await writer.enqueue({ kind: "ready" });
  await writer.enqueue({ kind: "output", turn_id: "turn-a", text: "answer" });
  await writer.close();
  const joined = Buffer.concat(bytes);
  const firstLength = joined.readUInt32BE() + 4;
  const first = unpack(joined.subarray(0, firstLength));
  const second = unpack(joined.subarray(firstLength));
  assert.equal(first.sequence, 1);
  assert.deepEqual(first.event, { kind: "ready" });
  assert.equal(second.sequence, 2);
  assert.deepEqual(second.event, { kind: "output", turn_id: "turn-a", text: "answer" });
});

test("all five message operations have one exact bounded wire representation", () => {
  for (const [index, kind] of ["message_started", "message_delta", "message_reset", "message_discarded", "message_finished"].entries()) {
    const body = { kind, turn_id: "turn-a", message_id: "msg-1", ...(kind === "message_delta" ? { text: "text" } : {}) };
    const packet = unpack(encodeEvent("session-a", "instance-a", BigInt(index + 1), body));
    assert.deepEqual(packet.event, body);
    assert.equal(packet.sequence, index + 1);
  }
});

test("message deltas use UTF-8 byte bounds and preserve whitespace", () => {
  for (const text of ["🙂".repeat(4096), " ".repeat(16384), "\n\t"]) {
    const body = { kind: "message_delta", turn_id: "turn-a", message_id: "msg-1", text };
    assert.deepEqual(unpack(encodeEvent("session-a", "instance-a", 1n, body)).event, body);
  }
  for (const text of ["", "x".repeat(16385), "🙂".repeat(4097), "\ud800"]) {
    assert.throws(() => encodeEvent("session-a", "instance-a", 1n,
      { kind: "message_delta", turn_id: "turn-a", message_id: "msg-1", text }));
  }
});

test("message frames reject invalid identities and unexpected fields", () => {
  for (const body of [
    { kind: "message_started", turn_id: "turn-a", message_id: "" },
    { kind: "message_reset", turn_id: "turn-a", message_id: "bad/id" },
    { kind: "message_discarded", turn_id: "turn-a", message_id: "a..b" },
    { kind: "message_finished", turn_id: "", message_id: "msg-1" },
    { kind: "message_started", turn_id: "turn-a", message_id: "msg-1", text: "unexpected" },
    { kind: "message_delta", turn_id: "turn-a", message_id: "msg-1" },
    { kind: "message_delta", turn_id: "turn-a", message_id: "msg-1", text: "x", extra: true },
    { kind: "message_unknown", turn_id: "turn-a", message_id: "msg-1" },
  ]) assert.throws(() => encodeEvent("session-a", "instance-a", 1n, body));
});

test("actual projection output passes through the actual encoder without text duplication", () => {
  const projection = new SdkMessageProjection("turn-a", "native-turn-a");
  const output = [
    ...projection.accept({ type: "assistant:delta", turnId: "native-turn-a", messageId: "native-a", text: "draft" }),
    ...projection.accept({ type: "assistant:message", turnId: "native-turn-a", messageId: "native-a", text: "final" }),
  ];
  const packets = output.map((body, index) => unpack(encodeEvent("session-a", "instance-a", BigInt(index + 1), body)));
  const messages = new Map();
  for (const { event } of packets) {
    if (event.kind === "message_started" || event.kind === "message_reset") messages.set(event.message_id, "");
    if (event.kind === "message_delta") messages.set(event.message_id, messages.get(event.message_id) + event.text);
  }
  assert.deepEqual([...messages], [["msg-1", "final"]]);
  assert.deepEqual(packets.map((packet) => packet.sequence), [1, 2, 3, 4, 5]);
});
