import assert from "node:assert/strict";
import { test } from "node:test";
import { LIMIT_MS, clockSkew, nativeCounts, nativeLook, nativeSample, nativeShown } from "./switch-time-kit.mjs";
import { NATIVE_FIRST_READ_MS, VIEW_IDS_MAX, inPageNativeRead } from "./switch-time.mjs";

const TEXT = "stand-in ready";
const CLICK = 1_800_000_000_000;
const ROWS = ["", "Codex stand-in ready", ""];
const seen = (fields) => ({ hidden: false, rows: ROWS, uncovered_ms: CLICK + 9, presented_ms: CLICK + 21, now_ms: CLICK + 80, direct: true,
  asked_ms: CLICK + 4, skipped: 0, skip_first: null, busy_answers: 3, cover_refused: 0, ...fields });

test("a native view is on screen once it is not hidden, presented a frame since its uncover and its rows hold the marker", () => {
  assert.equal(nativeShown(seen({}), TEXT), true);
  assert.equal(nativeShown(seen({ hidden: true, uncovered_ms: null, presented_ms: null }), TEXT), false, "a covered view");
  assert.equal(nativeShown(seen({ presented_ms: null }), TEXT), false, "uncovered, no frame drawn yet");
  assert.equal(nativeShown(seen({ rows: ["", "$ ", ""] }), TEXT), false, "another screen");
  assert.throws(() => nativeShown({ rows: ROWS }, TEXT), /hidden/);
  assert.throws(() => nativeShown(seen({ hidden: true }), TEXT), /hidden view keeps no stamps/);
});

test("a native switch runs from the click to the first frame the view presented after it, and keeps the uncover's share, the cover's entry and the draws skipped since the uncover", () => {
  assert.deepEqual(nativeSample(CLICK, seen({}), TEXT), { ms: 21, uncovered_ms: 9, asked_ms: 4, skipped: 0, skip_first: null });
  assert.deepEqual(nativeSample(CLICK, seen({ skipped: 2, skip_first: "frame_timeout" }), TEXT), { ms: 21, uncovered_ms: 9, asked_ms: 4, skipped: 2, skip_first: "frame_timeout" });
  assert.equal(nativeSample(CLICK, seen({ asked_ms: CLICK - 300 }), TEXT).asked_ms, null, "a cover entry before the click is another switch's");
  assert.equal(nativeSample(CLICK, seen({ asked_ms: null }), TEXT).asked_ms, null);
  assert.equal(nativeSample(CLICK, seen({ hidden: true, uncovered_ms: null, presented_ms: null }), TEXT), null, "still covered: not yet");
  assert.equal(nativeSample(CLICK, seen({ presented_ms: null }), TEXT), null, "uncovered, not yet presented");
  assert.equal(nativeSample(CLICK, seen({ rows: [""] }), TEXT), null, "presented without the marker");
  assert.throws(() => nativeSample(CLICK, seen({ uncovered_ms: CLICK - 5, presented_ms: CLICK - 1 }), TEXT), /shown before the click/,
    "a view already shown before the click is never this switch's sample");
  assert.throws(() => nativeSample(0, seen({}), TEXT), /click stamp/);
  assert.throws(() => nativeSample(CLICK, seen({ presented_ms: CLICK + 1 }), TEXT), /presents only after/);
});

test("the last read of a switch the view never showed in keeps whether it was hidden, its stamps and the cover's entry from the click, its skipped draws and whether the marker was there", () => {
  assert.deepEqual(nativeLook(CLICK, seen({ presented_ms: null, skipped: 3, skip_first: "frame_timeout" }), TEXT),
    { hidden: false, uncovered_ms: 9, presented_ms: null, asked_ms: 4, marker: true, skipped: 3, skip_first: "frame_timeout" });
  assert.deepEqual(nativeLook(CLICK, seen({ hidden: true, uncovered_ms: null, presented_ms: null, asked_ms: CLICK - 300 }), TEXT),
    { hidden: true, uncovered_ms: null, presented_ms: null, asked_ms: null, marker: true, skipped: 0, skip_first: null }, "still covered and no cover asked since the click");
  assert.equal(nativeLook(CLICK, seen({ rows: ["", "$ ", ""] }), TEXT).marker, false, "presented without the marker");
  assert.throws(() => nativeLook(CLICK, seen({ skipped: -1 }), TEXT), /skipped/);
  assert.throws(() => nativeLook(CLICK, seen({ skip_first: "frame_timeout" }), TEXT), /first skip/);
});

test("the busy answers and refused covers of one switch are the host's counts between the read before the click and the last read", () => {
  assert.deepEqual(nativeCounts(seen({ busy_answers: 3, cover_refused: 1 }), seen({ busy_answers: 5, cover_refused: 1 })), { busy: 2, refused: 0 });
  assert.deepEqual(nativeCounts(seen({}), seen({ cover_refused: 2 })), { busy: 0, refused: 2 });
  assert.throws(() => nativeCounts(seen({ busy_answers: 5 }), seen({ busy_answers: 3 })), /never fall/);
  assert.throws(() => nativeCounts(seen({ busy_answers: undefined }), seen({})), /count/);
});

test("the clock skew is how far the host's stamp falls outside the page stamps around its read", () => {
  assert.equal(clockSkew(1000, 1003, 1007), 0);
  assert.equal(clockSkew(1000, 1000, 1000), 0);
  assert.equal(clockSkew(1000, 996, 1007), 4);
  assert.equal(clockSkew(1000, 1010, 1007), 3);
  assert.throws(() => clockSkew(1007, 1003, 1000), /page stamps in order/);
  assert.throws(() => clockSkew(1000, NaN, 1007), /host stamp/);
});

test("the first native read waits the limit itself, so a passing switch has no probe inside it, and the id scan is bounded", () => {
  assert.equal(NATIVE_FIRST_READ_MS, LIMIT_MS);
  assert.ok(Number.isInteger(VIEW_IDS_MAX) && VIEW_IDS_MAX >= 16 && VIEW_IDS_MAX <= 256);
});

/** Runs `inPageNativeRead` from its own text, as a page gets it, with a stand-in bridge
 * and clock. Inputs: what the bridge answers (a value, or `{ refuse }`). Output: the
 * answers and the bridge calls. */
async function readInPage(bridge) {
  const calls = [];
  let now = 5;
  const window = {
    __TAURI_INTERNALS__: {
      invoke: (command, args) => {
        calls.push({ command, args });
        now += 3;
        return bridge.refuse === undefined ? Promise.resolve(bridge) : Promise.reject(bridge.refuse);
      },
    },
  };
  const performance = { timeOrigin: CLICK, now: () => now };
  const run = new Function("window", "performance", `return (${inPageNativeRead.toString()})`)(window, performance);
  const answers = [];
  await new Promise((settled) => run({ view: 7 }, (answer) => { answers.push(answer); settled(); }));
  return { answers, calls };
}

test("the in-page native read carries itself: one probe read between two page stamps, or the bridge's refusal as text", async () => {
  const read = await readInPage(seen({}));
  assert.deepEqual(read.calls, [{ command: "desktop_native_terminal_probe", args: { view: 7, step: { kind: "read" } } }]);
  assert.deepEqual(read.answers, [{ seen: seen({}), before: CLICK + 5, after: CLICK + 8 }]);
  const refused = await readInPage({ refuse: { code: "native_terminal_main_busy" } });
  assert.equal(refused.answers.length, 1);
  assert.match(refused.answers[0].failed, /native_terminal_main_busy/);
  const plain = await readInPage({ refuse: "native_terminal_view_unknown" });
  assert.deepEqual(plain.answers, [{ failed: "native_terminal_view_unknown" }]);
});
