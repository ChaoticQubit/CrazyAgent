import assert from "node:assert/strict";
import { test } from "node:test";
import { SdkMessageProjection } from "./host.messages.ts";

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
    event("assistant:message", "bad", "x".repeat(1048577)),
    event("assistant:message", "bad", "🙂".repeat(262145)),
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

test("a text exactly at the SDK byte limit remains lossless", () => {
  const projection = new SdkMessageProjection(turn, sdkTurn);
  const text = "🙂".repeat(262144);
  const output = projection.accept(event("assistant:message", "max", text));
  const deltas = output.filter((item) => item.kind === "message_delta");
  assert.equal(deltas.length, 64);
  assert.equal(deltas.map((item) => item.text).join(""), text);
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
