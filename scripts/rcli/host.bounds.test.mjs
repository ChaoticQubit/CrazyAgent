import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";
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

async function turn(messages) {
  const output = new PassThrough();
  const bytes = [];
  output.on("data", (chunk) => bytes.push(chunk));
  const channel = new HostChannel(output, "session", "instance");
  await channel.ready();
  channel.receive(control("prompt", { kind: "prompt", turn_id: "turn", text: "data" }));
  channel.start("turn");
  channel.observe({ type: "turn:start", turnId: "sdk-turn" });
  for (const message of messages) channel.observe({ turnId: "sdk-turn", messageId: "message", ...message });
  channel.observe({ type: "turn:end", turnId: "sdk-turn", status: "completed", durationMs: 1 });
  channel.complete("turn", "completed");
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
  assert.deepEqual(events.at(-1), { kind: "completed", turn_id: "turn", status: "succeeded" });
});
