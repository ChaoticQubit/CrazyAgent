import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";
import { MESSAGE_JSON_MAX } from "./host.messages.ts";
import { HostChannel } from "./host.runtime.ts";

const control = (id, body) => ({ protocol_version: 1, session_id: "session", request_id: id, control: body });

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

function run(channel, id, messages) {
  channel.receive(control(id, { kind: "prompt", turn_id: id, text: "data" }));
  channel.start(id);
  channel.observe({ type: "turn:start", turnId: `sdk-${id}` });
  for (const message of messages) channel.observe({ turnId: `sdk-${id}`, messageId: "message", ...message });
  channel.observe({ type: "turn:end", turnId: `sdk-${id}`, status: "completed", durationMs: 1 });
  channel.complete(id, "completed");
}

async function turn(...turns) {
  const output = new PassThrough();
  const bytes = [];
  output.on("data", (chunk) => bytes.push(chunk));
  const channel = new HostChannel(output, "session", "instance");
  await channel.ready();
  for (const [index, messages] of turns.entries()) {
    run(channel, `turn-${index + 1}`, messages);
    await new Promise((resolve) => setImmediate(resolve));
  }
  await channel.close();
  assert.equal(channel.signal.aborted, false);
  const events = packets(bytes).map((packet) => packet.event);
  assert.deepEqual(events[0], { kind: "ready" });
  for (const event of events) if (event.kind === "message_delta") assert(Buffer.byteLength(event.text) <= 16384);
  return events.slice(1);
}

function replies(events) {
  const messages = new Map();
  for (const event of events) {
    if (event.kind === "message_started" || event.kind === "message_reset") messages.set(event.message_id, "");
    if (event.kind === "message_delta") messages.set(event.message_id, messages.get(event.message_id) + event.text);
    if (event.kind === "message_discarded") messages.delete(event.message_id);
  }
  return [...messages.values()];
}

test("escape-heavy model text reaches a real stream in frames within the wire bound", async () => {
  const text = "\u0001".repeat(16384) + "\"\\\n".repeat(8192);
  const events = await turn([{ type: "assistant:message", text }]);
  assert.deepEqual(replies(events), [text]);
  assert.deepEqual(events.at(-1), { kind: "completed", turn_id: "turn-1", status: "succeeded" });
});

function sized(unit, edge) {
  const count = Math.floor((MESSAGE_JSON_MAX - 2 - edge.length) / Buffer.byteLength(JSON.stringify(unit).slice(1, -1)));
  const text = unit.repeat(count) + "x".repeat(MESSAGE_JSON_MAX - 2 - count * Buffer.byteLength(JSON.stringify(unit).slice(1, -1)));
  assert.equal(Buffer.byteLength(JSON.stringify(text)), MESSAGE_JSON_MAX);
  return text;
}

test("two maximum-size bursts in one tick reach a real stream without aborting the host", async () => {
  for (const unit of ["x", "\u0001", "🙂", "\"\n"]) {
    const text = sized(unit, "");
    const events = await turn([{ type: "assistant:delta", text }, { type: "assistant:message", text }]);
    assert.deepEqual(replies(events), [text]);
    assert.deepEqual(events.at(-1), { kind: "completed", turn_id: "turn-1", status: "succeeded" });
  }
});

test("an over-limit message fails its turn with a plain discard and the host keeps serving", async () => {
  const over = `${sized("x", "")}x`;
  for (const large of [over, "x".repeat(1048576), "\u0001".repeat(1048576), "x".repeat(2097152)]) {
    const events = await turn(
      [{ type: "assistant:message", text: large }],
      [{ type: "assistant:delta", text: "draft" }, { type: "assistant:message", text: large }, { type: "assistant:delta", text: "late" }, { type: "assistant:message:discard" }, { type: "assistant:message", messageId: "next", text: "kept" }],
      [{ type: "assistant:message", text: "ok" }],
    );
    const first = events.findIndex((event) => event.kind === "completed");
    assert.deepEqual(events.slice(0, first + 1), [{ kind: "completed", turn_id: "turn-1", status: "failed" }]);
    const second = events.slice(first + 1);
    const end = second.findIndex((event) => event.kind === "completed");
    assert.deepEqual(second.slice(0, end + 1), [
      { kind: "message_started", turn_id: "turn-2", message_id: "msg-1" },
      { kind: "message_delta", turn_id: "turn-2", message_id: "msg-1", text: "draft" },
      { kind: "message_discarded", turn_id: "turn-2", message_id: "msg-1" },
      { kind: "message_started", turn_id: "turn-2", message_id: "msg-2" },
      { kind: "message_delta", turn_id: "turn-2", message_id: "msg-2", text: "kept" },
      { kind: "message_finished", turn_id: "turn-2", message_id: "msg-2" },
      { kind: "completed", turn_id: "turn-2", status: "failed" },
    ]);
    assert.deepEqual(replies(second.slice(end + 1)), ["ok"]);
    assert.deepEqual(events.at(-1), { kind: "completed", turn_id: "turn-3", status: "succeeded" });
  }
});
