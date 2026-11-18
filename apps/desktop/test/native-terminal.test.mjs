import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const native = await import("../src/screens/team/native-terminal.tsx");

test("V243-NATIVE-A11Y-IME: the harness's text operations become hook events, anything else none", () => {
  assert.deepEqual(native.textOp({ kind: "marked", text: "\u306b", at: 1 }), { kind: "marked", text: "\u306b", at: 1 });
  assert.deepEqual(native.textOp({ kind: "insert", text: "\u65e5\u672c" }), { kind: "insert", text: "\u65e5\u672c" });
  assert.deepEqual(native.textOp({ kind: "unmark", text: "dropped" }), { kind: "unmark" });
  assert.deepEqual(native.textOp({ kind: "access" }), { kind: "access" });
  assert.equal(native.textOp({ kind: "marked", text: "a", at: -1 }), null);
  assert.equal(native.textOp({ kind: "marked", text: "a", at: 1.5 }), null);
  assert.equal(native.textOp({ kind: "marked", text: "a", at: 2 }), null, "the caret stays inside the text");
  assert.equal(native.textOp({ kind: "marked", text: "\u{1f600}", at: 2 }).at, 2, "UTF-16 units");
  assert.equal(native.textOp({ kind: "insert", text: 5 }), null);
  assert.equal(native.textOp({ kind: "text", text: "a" }), null, "timed inputs keep their own events");
  assert.equal(native.textOp(null), null);
  assert.equal(native.textOp("access"), null);
});
