import assert from "node:assert/strict";
import test from "node:test";
import { FRAME_TEXT_MAX, encodeEvent } from "./host.codec.ts";
import { SdkTurnProjection } from "./host.turns.ts";
const control = (request, body) => ({ protocol_version: 1, session_id: "session", request_id: request, control: body });
const prompt = (id, text = "do the work") => control(`request-${id}`, { kind: "prompt", turn_id: id, text });
const steer = (id) => control(`request-${id}`, { kind: "steer", turn_id: id, text: "change course" });
const start = (state, id, sdk = `sdk-${id}`) => {
state.start(id);
assert.deepEqual(state.event({ type: "turn:start", turnId: sdk }), []);
};
const end = (state, sdk, status = "completed") => state.event({ type: "turn:end", turnId: sdk, status, durationMs: 1 });
const answer = (request, id, promptId, answers) => control(request, { kind: "answer", turn_id: id, prompt_id: promptId, answers });
const ask = (state, questions, id = "native-ask") => state.event({ type: "input:request", request: { id, questions } });
test("admission binds the actual SDK turn and only the completion promise emits completion", () => {
const state = new SdkTurnProjection();
assert.deepEqual(state.admit(prompt("one")), { kind: "submit", turnId: "one", text: "do the work", steer: false });
assert.equal(state.hasOutstanding, true);
start(state, "one");
assert.deepEqual(state.event({ type: "assistant:delta", turnId: "sdk-one", messageId: "message", text: "draft" }), [
{ kind: "message_started", turn_id: "one", message_id: "msg-1" },
{ kind: "message_delta", turn_id: "one", message_id: "msg-1", text: "draft" },
]);
assert.deepEqual(end(state, "sdk-one"), []);
assert.equal(state.hasOutstanding, true);
assert.deepEqual(state.complete("one", "completed"), [{ kind: "completed", turn_id: "one", status: "succeeded" }]);
assert.equal(state.hasOutstanding, false);
});
test("a steering successor may start before the previous promise callback runs", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
assert.deepEqual(state.admit(steer("two")), { kind: "submit", turnId: "two", text: "change course", steer: true });
assert.throws(() => state.admit(steer("three")), /host_turn_rejected/);
assert.throws(() => state.admit(prompt("three")), /host_turn_rejected/);
end(state, "sdk-one", "cancelled");
start(state, "two");
assert.deepEqual(state.complete("one", "cancelled"), [{ kind: "completed", turn_id: "one", status: "cancelled" }]);
assert.deepEqual(state.event({ type: "assistant:message", turnId: "sdk-two", messageId: "message", text: "result" }).at(-1), { kind: "message_finished", turn_id: "two", message_id: "msg-1" });
end(state, "sdk-two");
assert.deepEqual(state.complete("two", "completed"), [{ kind: "completed", turn_id: "two", status: "succeeded" }]);
});
test("unknown, duplicate and stale controls reject without consuming a valid admission", () => {
const state = new SdkTurnProjection();
assert.throws(() => state.admit(steer("one")), /host_turn_rejected/);
assert.throws(() => state.admit(control("request-one", { kind: "cancel", turn_id: "unknown" })), /host_turn_rejected/);
state.admit(prompt("one"));
assert.throws(() => state.admit(prompt("one")), /host_turn_rejected/);
assert.deepEqual(state.complete("one", "cancelled"), [{ kind: "completed", turn_id: "one", status: "cancelled" }]);
assert.throws(() => state.admit(control("new-request", { kind: "prompt", turn_id: "one", text: "repeat" })), /host_turn_rejected/);
assert.throws(() => state.admit(control("request-one", { kind: "prompt", turn_id: "two", text: "repeat" })), /host_turn_rejected/);
assert.equal(state.hasOutstanding, false);
state.admit(prompt("two"));
});
test("cancelling either known turn clears both inputs without inventing completion", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
const first = ask(state, [{ question: "Proceed?", options: ["yes", "no"] }])[0];
state.admit(steer("two"));
assert.deepEqual(state.admit(control("cancel", { kind: "cancel", turn_id: "two" })), { kind: "cancel" });
assert.throws(() => state.admit(answer("late", "one", first.prompt_id, ["yes"])), /host_turn_rejected/);
assert.equal(state.hasOutstanding, true);
assert.deepEqual(state.complete("two", "cancelled"), [{ kind: "completed", turn_id: "two", status: "cancelled" }]);
assert.deepEqual(state.complete("one", "cancelled"), [{ kind: "completed", turn_id: "one", status: "cancelled" }]);
});
test("questions are presented sequentially and retained until the real input response", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
const first = ask(state, [{ question: "Target?", options: ["macOS", "Linux"] }, { question: "Name?", options: [] }])[0];
assert.deepEqual(first, { kind: "input", turn_id: "one", prompt_id: "input-1", question: "Target?", choices: ["macOS", "Linux"] });
const partial = state.admit(answer("a", "one", first.prompt_id, ["Linux"]));
assert.equal(partial.kind, "answer"); assert.equal(partial.delivery, null);
assert.deepEqual(partial.events, [{ kind: "input", turn_id: "one", prompt_id: "input-2", question: "Name?", choices: [] }]);
const last = state.admit(answer("b", "one", "input-2", ["project"]));
assert.deepEqual(last, { kind: "answer", events: [], delivery: { id: "native-ask", answers: ["Linux", "project"] } });
assert.throws(() => ask(state, [{ question: "Third?", options: [] }], "other"), /host_turn_rejected/);
assert.deepEqual(state.event({ type: "input:response", response: { id: "native-ask", answers: ["Linux", "project"] } }), []);
assert.equal(ask(state, [{ question: "Third?", options: [] }], "other")[0].prompt_id, "input-3");
});
test("stale answers, multiple answers and mismatched response acknowledgements preserve pending input", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
ask(state, [{ question: "Value?", options: [] }]);
for (const value of [answer("a", "one", "unknown", ["x"]), answer("a", "one", "input-1", ["x", "y"]), answer("a", "other", "input-1", ["x"])]) {
assert.throws(() => state.admit(value), /host_turn_rejected/);
}
assert.throws(() => state.event({ type: "input:response", response: { id: "native-ask", answers: ["x"] } }), /host_turn_rejected/);
state.admit(answer("a", "one", "input-1", ["x"]));
for (const response of [{ id: "wrong", answers: ["x"] }, { id: "native-ask", answers: ["wrong"] }, { id: "native-ask", answers: [] }]) {
assert.throws(() => state.event({ type: "input:response", response }), /host_turn_rejected/);
}
assert.deepEqual(state.event({ type: "input:response", response: { id: "native-ask", answers: ["x"] } }), []);
});
test("all input questions are validated before any pending state is published", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
const valid = { question: "  ", options: ["😀"] };
const cases = [[], Array(9).fill(valid), [valid, { question: "", options: [] }], [{ question: "😀".repeat(1025), options: [] }], [{ question: "bad\ud800", options: [] }], [{ question: "Q", options: Array(9).fill("x") }], [{ question: "Q", options: ["x".repeat(1025)] }], [{ question: "Q", options: [""] }]];
for (const questions of cases) assert.throws(() => ask(state, questions), /host_turn_rejected/);
assert.equal(ask(state, [valid])[0].prompt_id, "input-1");
});
test("foreign SDK events, duplicate starts and out of order lifecycle reject", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one"));
assert.throws(() => state.event({ type: "turn:start", turnId: "sdk-one" }), /host_turn_rejected/);
start(state, "one");
assert.throws(() => state.start("one"), /host_turn_rejected/);
assert.throws(() => state.event({ type: "assistant:delta", turnId: "foreign", messageId: "m", text: "x" }), /host_turn_rejected/);
assert.throws(() => end(state, "foreign"), /host_turn_rejected/);
end(state, "sdk-one");
assert.throws(() => end(state, "sdk-one"), /host_turn_rejected/);
assert.throws(() => state.event({ type: "assistant:delta", turnId: "sdk-one", messageId: "m", text: "late" }), /host_turn_rejected/);
state.complete("one", "completed");
state.admit(prompt("two")); state.start("two");
assert.throws(() => state.event({ type: "turn:start", turnId: "sdk-one" }), /host_turn_rejected/);
assert.deepEqual(state.event({ type: "turn:start", turnId: "sdk-two" }), []);
});
test("nonterminal promise results and duplicate completion never report success", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one"));
for (const status of ["in_progress", "requires_action", "unknown"]) assert.throws(() => state.complete("one", status), /host_turn_rejected/);
assert.throws(() => state.complete("foreign", "completed"), /host_turn_rejected/);
assert.deepEqual(state.complete("one", "failed"), [{ kind: "completed", turn_id: "one", status: "failed" }]);
assert.throws(() => state.complete("one", "completed"), /host_turn_rejected/);
});
test("shutdown and EOF stop admission while actual turn outcomes can still drain", () => {
for (const shutdown of [true, false]) {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
if (shutdown) assert.deepEqual(state.admit(control("shutdown", { kind: "shutdown" })), { kind: "shutdown" });
else state.stop();
assert.throws(() => state.admit(prompt("two")), /host_turn_rejected/);
assert.equal(state.hasOutstanding, true);
state.complete("one", "cancelled");
state.close(); state.close();
assert.equal(state.hasOutstanding, false);
assert.throws(() => state.event({ type: "turn:start", turnId: "late" }), /host_turn_rejected/);
}
});
test("process lifetime identity capacity fails closed without evicting old requests", () => {
const state = new SdkTurnProjection();
for (let index = 0; index < 4096; index += 1) {
const id = `turn-${index}`;
state.admit(prompt(id)); state.complete(id, "cancelled");
}
assert.throws(() => state.admit(prompt("overflow")), /host_turn_rejected/);
assert.throws(() => state.admit(control("extra", { kind: "shutdown" })), /host_turn_rejected/);
assert.equal(state.hasOutstanding, false);
state.stop(); state.close();
});
test("internal turn ownership corruption trips a runnable invariant", () => {
const state = new SdkTurnProjection();
state.admit(prompt("one"));
state.turns.set("other", state.turns.get("one"));
assert.throws(() => state.admit(prompt("two")), /host_turn_state_invariant/);
});
test("input questions are bounded by their combined JSON size so an admitted question always encodes", () => {
const options = Array(8).fill("\u0001".repeat(1024));
const question = "\u0001".repeat(2555) + "xx";
assert.equal(Buffer.byteLength(JSON.stringify([question, ...options])), FRAME_TEXT_MAX);
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
assert.throws(() => ask(state, [{ question: `${question}x`, options }], "too-large"), /host_turn_rejected/);
const [frame] = ask(state, [{ question, options }]);
assert.deepEqual(frame, { kind: "input", turn_id: "one", prompt_id: "input-1", question, choices: options });
const id = "a".repeat(128);
assert(encodeEvent(id, id, 18446744073709551615n, { ...frame, turn_id: id, prompt_id: id }).byteLength <= 65540);
});
test("a turn whose message overflowed completes failed unless it was cancelled", () => {
for (const [status, expected] of [["completed", "failed"], ["cancelled", "cancelled"], ["failed", "failed"]]) {
const state = new SdkTurnProjection();
state.admit(prompt("one")); start(state, "one");
assert.deepEqual(state.event({ type: "assistant:message", turnId: "sdk-one", messageId: "large", text: "x".repeat(1048576) }), []);
end(state, "sdk-one", status);
assert.deepEqual(state.complete("one", status), [{ kind: "completed", turn_id: "one", status: expected }]);
state.admit(prompt("two")); start(state, "two");
state.event({ type: "assistant:message", turnId: "sdk-two", messageId: "small", text: "ok" });
end(state, "sdk-two");
assert.deepEqual(state.complete("two", "completed"), [{ kind: "completed", turn_id: "two", status: "succeeded" }]);
}
});
