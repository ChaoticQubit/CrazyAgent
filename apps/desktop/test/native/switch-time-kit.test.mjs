import assert from "node:assert/strict";
import { test } from "node:test";
import { GATE_SHARE, LIMIT_MS, SAMPLES_MAX, SWITCHES, TARGET_MS, line, rank, report, stampSample } from "./switch-time-kit.mjs";
import { inPageFramePeriod, inPageSwitch, switchExpression } from "./switch-time.mjs";

const row = (times) => times.map((ms) => ({ ms }));
const twenty = (fill, ...worst) => [...Array(SWITCHES - worst.length).fill(fill), ...worst];

test("the limits are the owner's: 30 ms asked for, 50 ms accepted, 20 switches a row (DEC-187)", () => {
  assert.deepEqual([TARGET_MS, LIMIT_MS, SWITCHES, GATE_SHARE], [30, 50, 20, 0.95]);
});

test("rank is the nearest rank: the second worst of 20 at 0.95, the 10th at 0.5", () => {
  const sorted = Array.from({ length: SWITCHES }, (_, index) => index + 1);
  assert.equal(rank(sorted, 0.95), 19);
  assert.equal(rank(sorted, 0.5), 10);
  assert.equal(rank(sorted, 1), 20);
  assert.equal(rank([7], 0.95), 7);
  assert.throws(() => rank([], 0.5), /at least one/);
  assert.throws(() => rank([2, 1], 0.5), /rise/);
  assert.throws(() => rank([1], 0), /share/);
});

test("one slow switch in 20 passes, two fail", () => {
  const one = report(row(twenty(12, 400)));
  assert.equal(one.pass, true);
  assert.deepEqual([one.p95_ms, one.max_ms, one.over_limit, one.over_target], [12, 400, 1, 1]);
  const two = report(row(twenty(12, 51, 400)));
  assert.equal(two.pass, false);
  assert.deepEqual([two.p95_ms, two.over_limit], [51, 2]);
});

test("the limit itself passes and the target is only reported", () => {
  const atLimit = report(row(twenty(LIMIT_MS)));
  assert.equal(atLimit.pass, true);
  assert.deepEqual([atLimit.p50_ms, atLimit.over_target, atLimit.over_limit], [50, 20, 0]);
  assert.match(line("terminal tab", atLimit), /PASS .*p95 misses the 30 ms target/);
  assert.match(line("terminal tab", report(row(twenty(TARGET_MS)))), /PASS .*p95 meets the 30 ms target/);
  assert.equal(report(row(twenty(LIMIT_MS + 0.1))).pass, false);
});

test("a switch that never showed its target counts past every limit", () => {
  const one = report(row(twenty(10, null)));
  assert.equal(one.pass, true);
  assert.deepEqual([one.never_shown, one.over_limit, one.max_ms, one.p95_ms], [1, 1, null, 10]);
  const two = report(row(twenty(10, null, null)));
  assert.equal(two.pass, false);
  assert.deepEqual([two.never_shown, two.p95_ms], [2, null]);
  assert.match(line("terminal page", two), /p95 never shown.*FAIL/);
  const all = report(row(twenty(null)));
  assert.deepEqual([all.pass, all.p50_ms, all.never_shown], [false, null, 20]);
});

test("a report refuses samples that are not times", () => {
  assert.throws(() => report([]), /1 to/);
  assert.throws(() => report(row([-1])), /time or null/);
  assert.throws(() => report(row([NaN])), /time or null/);
  assert.throws(() => report([{}]), /time or null/);
  assert.throws(() => report(row(Array(SAMPLES_MAX + 1).fill(1))), /1 to/);
});

test("a stamp sample is the first show at or after the click", () => {
  assert.deepEqual(stampSample(1000, [400, 990, 1031, 1500]), { ms: 31 });
  assert.deepEqual(stampSample(1000, [1000]), { ms: 0 });
  assert.deepEqual(stampSample(1000, [400, 999]), { ms: null });
  assert.deepEqual(stampSample(1000, []), { ms: null });
  assert.throws(() => stampSample(0, [1]), /click stamp/);
  assert.throws(() => stampSample(1000, [NaN]), /show stamp/);
});

const TARGET = { scope: ".tg-terminal", rows: ".tg-terminal-host", text: "ready" };
const REQUEST = { click: "a#tab-conversation", target: TARGET, limit_ms: 10000 };
const FRAME_MS = 16;

/** A stand-in page for `inPageSwitch`, built from the function's own text as a page gets it:
 * one terminal whose marker row is in the page from the start, a link, a clock and frames
 * the test steps by hand. `state` says whether the row is laid out, passes the visibility
 * check and is what a press at its middle hits, from which frame after the click, and
 * whether its view has set up its buffer reader yet.
 * Inputs: the state and the in-page function to build, the switch by default. Output: the
 * function to run and the handles to step and read it. */
function page(state, inPage = inPageSwitch) {
  const clock = { now: 0, frames: [], tasks: [], timers: [], clicks: 0, frame: 0 };
  const visible = () => state.shownFromFrame !== null && clock.frame >= state.shownFromFrame;
  const row = {
    ...(state.reader === false ? {} : { readScreen: () => ["", "Codex stand-in ready", ""] }),
    checkVisibility: () => state.visibility ?? visible(),
    getBoundingClientRect: () => (state.laidOut === false ? { left: 0, top: 0, width: 0, height: 0 } : { left: 10, top: 20, width: 600, height: 17 }),
  };
  const scope = { querySelectorAll: (selector) => (selector === TARGET.rows ? [row] : []), contains: (node) => node === row };
  const other = {};
  const env = {
    document: {
      querySelectorAll: (selector) => (selector === TARGET.scope ? [scope] : []),
      querySelector: (selector) => (selector === REQUEST.click && state.link !== false ? { click: () => { clock.clicks += 1; } } : null),
      elementFromPoint: (x, y) => {
        assert.deepEqual([x, y], [310, 28.5], "the press lands on the row's middle");
        return visible() ? row : other;
      },
    },
    performance: { now: () => clock.now },
    requestAnimationFrame: (callback) => { clock.frames.push(callback); },
    MessageChannel: class {
      constructor() {
        this.port1 = {};
        this.port2 = { postMessage: () => { clock.tasks.push(() => this.port1.onmessage()); } };
      }
    },
    setTimeout: (callback, ms) => clock.timers.push({ callback, ms, cleared: false }) - 1,
    clearTimeout: (id) => { clock.timers[id].cleared = true; },
  };
  const run = new Function(...Object.keys(env), `return (${inPage.toString()})`)(...Object.values(env));
  const step = () => {
    clock.frame += 1;
    clock.now += FRAME_MS;
    for (const callback of clock.frames.splice(0)) callback();
    clock.now += 1;
    for (const task of clock.tasks.splice(0)) task();
  };
  return { run, clock, step };
}

test("the in-page switch carries itself: its text alone runs in a page", () => {
  const expression = switchExpression(REQUEST);
  assert.ok(expression.startsWith("new Promise(") && expression.includes(JSON.stringify(REQUEST)));
  assert.throws(() => switchExpression({ ...REQUEST, limit_ms: 0 }), /limit/);
  assert.throws(() => switchExpression({ ...REQUEST, target: { ...TARGET, text: "" } }), /text/);
});

test("on screen means laid out, visible and hit: text kept in the page while hidden never counts", () => {
  const asked = (state) => {
    const answers = [];
    page(state).run({ ...REQUEST, click: null }, (answer) => answers.push(answer));
    return answers;
  };
  assert.deepEqual(asked({ shownFromFrame: 0 }), [{ shown: true }]);
  assert.deepEqual(asked({ shownFromFrame: null }), [{ shown: false }], "another element takes the press, as over a parked terminal");
  assert.deepEqual(asked({ shownFromFrame: 0, visibility: false }), [{ shown: false }], "the browser says it is not visible");
  assert.deepEqual(asked({ shownFromFrame: 0, laidOut: false }), [{ shown: false }], "it has no box");
  assert.deepEqual(asked({ shownFromFrame: 0, reader: false }), [{ shown: false }], "its view has not set up its buffer reader yet");
});

test("a switch is timed from the click to the task after the first frame that shows the target", () => {
  const { run, clock, step } = page({ shownFromFrame: 3 });
  const answers = [];
  run(REQUEST, (answer) => answers.push(answer));
  assert.equal(clock.clicks, 1);
  step();
  step();
  assert.deepEqual(answers, [], "two frames without the target answer nothing");
  step();
  assert.deepEqual(answers, [{ ms: 3 * (FRAME_MS + 1), frame_ms: 3 * FRAME_MS + 2, frames: 3 }]);
  assert.equal(clock.timers[0].cleared, true, "the limit is dropped once the target showed");
  step();
  assert.equal(answers.length, 1, "it answers once");
});

test("a switch that never shows answers null at its limit, once, and one that cannot start says why", () => {
  const { run, clock, step } = page({ shownFromFrame: null });
  const answers = [];
  run(REQUEST, (answer) => answers.push(answer));
  step();
  step();
  assert.equal(clock.timers[0].ms, REQUEST.limit_ms);
  clock.timers[0].callback();
  step();
  assert.deepEqual(answers, [{ ms: null, frame_ms: null, frames: 2 }]);
  const refused = (state) => {
    const made = page(state);
    const seen = [];
    made.run(REQUEST, (answer) => seen.push(answer));
    assert.equal(made.clock.clicks, 0, "nothing is clicked");
    return seen;
  };
  assert.match(refused({ shownFromFrame: 0 })[0].error, /on screen before the click/);
  assert.match(refused({ shownFromFrame: null, link: false })[0].error, /nothing matches a#tab-conversation/);
});

test("the frame period is the median gap between frames, with the shortest beside it, or null at its limit", () => {
  const { run, clock, step } = page({ shownFromFrame: null }, inPageFramePeriod);
  const answers = [];
  run({ frames: 3, limit_ms: 5000 }, (answer) => answers.push(answer));
  step();
  step();
  clock.now += 40;
  step();
  assert.deepEqual(answers, [], "three frames are two gaps: nothing is answered yet");
  step();
  assert.deepEqual(answers, [{ period_ms: FRAME_MS + 1, min_ms: FRAME_MS + 1, frames: 3 }], "one slow frame moves neither the median nor the shortest");
  assert.equal(clock.timers[0].cleared, true);
  step();
  assert.equal(answers.length, 1, "it answers once");
  const stuck = page({ shownFromFrame: null }, inPageFramePeriod);
  const seen = [];
  stuck.run({ frames: 30, limit_ms: 5000 }, (answer) => seen.push(answer));
  stuck.step();
  stuck.step();
  stuck.clock.timers[0].callback();
  stuck.step();
  assert.deepEqual(seen, [{ period_ms: null, min_ms: null, frames: 1 }], "a window that stopped drawing has no period");
});
