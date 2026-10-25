import assert from "node:assert/strict";
import { test } from "node:test";
import { FRAME_TEXT_MAX, encodeEvent } from "./host.codec.ts";
import { MESSAGE_JSON_MAX, SdkMessageProjection } from "./host.messages.ts";

const turn = "external-turn-1";
const sdkTurn = "sdk-turn-1";
const event = (type, messageId, text) => ({
  type,
  turnId: sdkTurn,
  messageId,
  ...(text === undefined ? {} : { text }),
});
const frame = (kind, messageId, text) => ({
  kind,
  turn_id: turn,
  message_id: messageId,
  ...(text === undefined ? {} : { text }),
});

test("streamed final content replaces the same message without duplicate append", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  assert.deepEqual(projection.accept(event("assistant:delta", "native-a", "draft")), [
    frame("message_started", "msg-1"), frame("message_delta", "msg-1", "draft"),
  ]);
  assert.deepEqual(projection.accept(event("assistant:delta", "native-a", " text")), [
    frame("message_delta", "msg-1", " text"),
  ]);
  assert.deepEqual(projection.accept(event("assistant:message", "native-a", "final text")), [
    frame("message_reset", "msg-1"), frame("message_delta", "msg-1", "final text"),
    frame("message_finished", "msg-1"),
  ]);
  assert.throws(() => projection.accept(event("assistant:message", "native-a", "again")), /host_sdk_event_rejected/);
  assert.throws(() => projection.accept(event("assistant:delta", "native-a", "late")), /host_sdk_event_rejected/);
});

test("non-streamed output and subsequent discard preserve message identity", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  assert.deepEqual(projection.accept(event("assistant:message", "native-a", "answer")), [
    frame("message_started", "msg-1"), frame("message_delta", "msg-1", "answer"),
    frame("message_finished", "msg-1"),
  ]);
  assert.deepEqual(projection.accept(event("assistant:message:discard", "native-a")), [
    frame("message_discarded", "msg-1"),
  ]);
  assert.throws(() => projection.accept(event("assistant:delta", "native-a", "late")), /host_sdk_event_rejected/);
  assert.throws(() => projection.accept(event("assistant:message:discard", "native-a")), /host_sdk_event_rejected/);
});

test("unseen buffered SDK discard creates no visible message and cannot be reused", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  assert.deepEqual(projection.accept(event("assistant:message:discard", "hidden")), []);
  assert.throws(() => projection.accept(event("assistant:message", "hidden", "unexpected")), /host_sdk_event_rejected/);
  assert.deepEqual(projection.accept(event("assistant:message", "visible", "ok")), [
    frame("message_started", "msg-1"), frame("message_delta", "msg-1", "ok"),
    frame("message_finished", "msg-1"),
  ]);
});

test("Unicode chunks preserve every scalar and remain within the UTF-8 frame bound", () => {
  const text = "a".repeat(16383) + "🙂" + "é".repeat(9000);
  const projection = new SdkMessageProjection(turn, sdkTurn);
  const result = projection.accept(event("assistant:message", "native-a", text));
  const deltas = result.filter((item) => item.kind === "message_delta");
  assert(deltas.length >= 3);
  assert.equal(deltas.map((item) => item.text).join(""), text);
  for (const item of deltas) {
    assert(Buffer.byteLength(item.text) > 0 && Buffer.byteLength(item.text) <= 16384);
    assert.equal(item.text.isWellFormed(), true);
  }
  assert.deepEqual(result[0], frame("message_started", "msg-1"));
  assert.deepEqual(result.at(-1), frame("message_finished", "msg-1"));
});

test("empty final messages clear provisional content without inventing text", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  assert.deepEqual(projection.accept(event("assistant:delta", "empty-first", "")), []);
  assert.deepEqual(projection.accept(event("assistant:message", "empty-first", "")), [
    frame("message_started", "msg-1"), frame("message_finished", "msg-1"),
  ]);
  projection.accept(event("assistant:delta", "native-b", "provisional"));
  assert.deepEqual(projection.accept(event("assistant:message", "native-b", "")), [
    frame("message_reset", "msg-2"), frame("message_finished", "msg-2"),
  ]);
});

test("opaque SDK identifiers never become host IDs or object property lookups", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  for (const [index, id] of ["__proto__", "constructor", "toString"].entries()) {
    const result = projection.accept(event("assistant:message", id, "text"));
    assert.deepEqual(result[0], frame("message_started", `msg-${index + 1}`));
  }
  assert.equal(Object.prototype.hostMessage, undefined);
});

test("rejected SDK input has no partial publication or identity allocation", () => {
  for (const bad of [
    null,
    [],
    { ...event("assistant:delta", "bad", "text"), turnId: "other-turn" },
    event("assistant:delta", "bad", "\ud800"),
    event("assistant:delta", "", "text"),
    event("assistant:delta", "x".repeat(257), "text"),
    { ...event("assistant:message", "bad", "text"), extra: "unsupported" },
    { type: "assistant:delta", turnId: sdkTurn, messageId: "bad" },
  ]) {
    const projection = new SdkMessageProjection(turn, sdkTurn);
    assert.throws(() => projection.accept(bad), /host_sdk_event_rejected/);
    assert.deepEqual(projection.accept(event("assistant:message", "valid", "ok"))[0],
      frame("message_started", "msg-1"));
  }
});

test("accessor records are rejected without invoking untrusted getters", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  let invoked = false;
  const input = { ...event("assistant:message", "bad", "text") };
  Object.defineProperty(input, "text", { enumerable: true, get() { invoked = true; return "text"; } });
  assert.throws(() => projection.accept(input), /host_sdk_event_rejected/);
  assert.equal(invoked, false);
});

test("the active turn retains at most 128 SDK message identities including discards", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  for (let index = 0; index < 128; index++) {
    assert.deepEqual(projection.accept(event("assistant:message:discard", `hidden-${index}`)), []);
  }
  assert.throws(() => projection.accept(event("assistant:delta", "too-many", "x")), /host_sdk_event_rejected/);
});

test("a text exactly at the escaped message limit remains lossless", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  const text = "🙂".repeat((MESSAGE_JSON_MAX - 4) / 4) + "xx";
  assert.equal(Buffer.byteLength(JSON.stringify(text)), MESSAGE_JSON_MAX);
  const output = projection.accept(event("assistant:message", "max", text));
  const deltas = output.filter((item) => item.kind === "message_delta");
  assert.equal(deltas.length, 28);
  assert.equal(deltas.map((item) => item.text).join(""), text);
  assert.equal(output.length, 30);
  assert.equal(projection.overflowed, false);
});

test("over-limit text discards its message, marks the turn and drops later events for it", () => {
  const limit = "x".repeat(MESSAGE_JSON_MAX - 2);
  for (const large of [`${limit}x`, "\u0001".repeat(Math.ceil((MESSAGE_JSON_MAX - 2) / 6)), "x".repeat(1048577), "🙂".repeat(262145)]) {
    const projection = new SdkMessageProjection(turn, sdkTurn);
    assert.deepEqual(projection.accept(event("assistant:message", "hidden", large)), []);
    assert.equal(projection.overflowed, true);
    assert.deepEqual(projection.accept(event("assistant:delta", "hidden", "late")), []);
    assert.deepEqual(projection.accept(event("assistant:message:discard", "hidden")), []);
    assert.deepEqual(projection.accept(event("assistant:delta", "shown", "draft")), [
      frame("message_started", "msg-1"), frame("message_delta", "msg-1", "draft"),
    ]);
    assert.deepEqual(projection.accept(event("assistant:message", "shown", large)), [frame("message_discarded", "msg-1")]);
    assert.deepEqual(projection.accept(event("assistant:message", "shown", "final")), []);
  }
  for (const malformed of [`\ud800${limit}`, `${limit}\ud800`]) {
    const fresh = new SdkMessageProjection(turn, sdkTurn);
    assert.throws(() => fresh.accept(event("assistant:message", "bad", malformed)), /host_sdk_event_rejected/);
    assert.equal(fresh.overflowed, false);
  }
});

test("chunks fill the raw and JSON-escaped budgets exactly so every delta frame fits the wire", () => {
  const projection = new SdkMessageProjection("t".repeat(128), sdkTurn);
  const controls = Array.from({ length: 32 }, (_, unit) => String.fromCharCode(unit)).join("");
  let plane = "";
  for (let unit = 0x20; unit <= 0xffff; unit++) if (unit < 0xd800 || unit > 0xdfff) plane += String.fromCharCode(unit);
  const text = controls.repeat(1024) + plane + "🙂".repeat(4096) + "\"\\".repeat(8192);
  const deltas = projection.accept(event("assistant:message", "dense", text)).filter((item) => item.kind === "message_delta");
  assert.equal(deltas.map((item) => item.text).join(""), text);
  for (const [index, delta] of deltas.entries()) {
    const raw = Buffer.byteLength(delta.text);
    const json = Buffer.byteLength(JSON.stringify(delta.text));
    assert(raw <= 16384 && json <= FRAME_TEXT_MAX, `chunk ${index} has ${raw} raw and ${json} JSON bytes`);
    assert(encodeEvent("s".repeat(128), "i".repeat(128), 18446744073709551615n, delta).byteLength <= 65540);
    const next = deltas[index + 1]?.text;
    if (next === undefined) continue;
    const scalar = String.fromCodePoint(next.codePointAt(0));
    assert(raw + Buffer.byteLength(scalar) > 16384 || json + Buffer.byteLength(JSON.stringify(scalar)) - 2 > FRAME_TEXT_MAX);
  }
});

test("closed projections reject further events and invalid turn identities fail admission", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  projection.close();
  projection.close();
  assert.throws(() => projection.accept(event("assistant:message", "late", "x")), /host_sdk_event_rejected/);
  for (const invalid of ["", "a..b", "x".repeat(129), "bad/turn", "\ud800"]) {
    assert.throws(() => new SdkMessageProjection(invalid, sdkTurn), /host_sdk_event_rejected/);
  }
  assert.throws(() => new SdkMessageProjection(turn, ""), /host_sdk_event_rejected/);
});

test("non-message SDK events remain the responsibility of the full host dispatcher", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  assert.deepEqual(projection.accept({ type: "turn:start", turnId: sdkTurn }), []);
  assert.deepEqual(projection.accept({ type: "run:error", error: "controlled" }), []);
  assert.deepEqual(projection.accept(event("assistant:message", "visible", "ok"))[0],
    frame("message_started", "msg-1"));
});

test("an impossible internal message counter trips the runtime invariant", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  projection.nextMessage = 0;
  assert.throws(() => projection.accept(event("assistant:delta", "native", "x")), /host_message_counter_invariant/);
});

test("corrupt internal message state trips the identity invariant before publication", () => {
  for (const state of [
    { hostId: "msg-1", phase: "unknown" },
    { hostId: null, phase: "streaming" },
    { hostId: "foreign-id", phase: "finished" },
    { hostId: "msg-129", phase: "streaming" },
    undefined,
    null,
  ]) {
    const projection = new SdkMessageProjection(turn, sdkTurn);
    projection.messages.set("native", state);
    assert.throws(() => projection.accept(event("assistant:message:discard", "native")),
      /host_message_identity_invariant/);
  }
});
