import assert from "node:assert/strict";
import test from "node:test";
import { REFUSED_EXIT, countFindings, margin, noise, quiet, summary, verdict } from "./speed-budget.mjs";

const QUIET = { load1: 0.8, compilers: [], memoryLevel: 80, onAC: true, screenLocked: false, busy: false };

test("summary takes the median and the nearest-rank p90 of the kept samples", () => {
  assert.deepEqual(summary([5, 1, 4, 2, 3]), { n: 5, median: 3, p90: 5 });
  assert.deepEqual(summary([10, 20, 30, 40, 50, 60, 70, 80, 90, 100]), { n: 10, median: 55, p90: 90 });
  assert.deepEqual(summary([7]), { n: 1, median: 7, p90: 7 });
});

test("summary refuses an empty list, a negative or a non-finite sample", () => {
  assert.throws(() => summary([]), /at least one sample/);
  assert.throws(() => summary([1, -2]), /finite and not negative/);
  assert.throws(() => summary([1, Number.NaN]), /finite and not negative/);
  assert.throws(() => summary([1, Infinity]), /finite and not negative/);
});

test("noise is the relative gap between two medians of the same build", () => {
  assert.equal(noise({ median: 100 }, { median: 104 }), 0.04);
  assert.equal(noise({ median: 104 }, { median: 100 }), 0.04);
  assert.equal(noise({ median: 50 }, { median: 50 }), 0);
});

test("the margin is 10% or twice the measured noise, whichever is larger", () => {
  assert.equal(margin(0), 0.1);
  assert.equal(margin(0.03), 0.1);
  assert.equal(margin(0.08), 0.16);
  assert.throws(() => margin(-0.1), /noise/);
});

test("a candidate inside the margin passes; one past both the margin and the floor fails", () => {
  const base = { n: 10, median: 200, p90: 240 };
  assert.deepEqual(verdict({ base, candidate: { n: 10, median: 215, p90: 250 }, margin: 0.1, floor: 20 }),
    { pass: true, reasons: [] });
  const slow = verdict({ base, candidate: { n: 10, median: 250, p90: 260 }, margin: 0.1, floor: 20 });
  assert.equal(slow.pass, false);
  assert.match(slow.reasons.join("\n"), /median 250 > 220 \(base 200 \+ 10%\) by 50 > floor 20/);
});

test("a gap past the margin but under the floor passes, so tiny absolute gaps never fail", () => {
  const base = { n: 10, median: 10, p90: 12 };
  const result = verdict({ base, candidate: { n: 10, median: 25, p90: 30 }, margin: 0.1, floor: 20 });
  assert.deepEqual(result, { pass: true, reasons: [] });
});

test("the p90 fails past twice the margin and twice the floor even when the median holds", () => {
  const base = { n: 10, median: 200, p90: 240 };
  const result = verdict({ base, candidate: { n: 10, median: 205, p90: 330 }, margin: 0.1, floor: 20 });
  assert.equal(result.pass, false);
  assert.match(result.reasons.join("\n"), /p90 330 > 288 \(base 240 \+ 20%\) by 90 > floor 40/);
});

test("verdict refuses mismatched sample counts or a zero floor", () => {
  const base = { n: 10, median: 200, p90: 240 };
  assert.throws(() => verdict({ base, candidate: { n: 9, median: 200, p90: 240 }, margin: 0.1, floor: 20 }), /same number of samples/);
  assert.throws(() => verdict({ base, candidate: base, margin: 0.1, floor: 0 }), /floor/);
});

test("call counts: a new command or one called more than once beyond the base is a finding", () => {
  const base = { start: { desktop_connect: 1, desktop_list_projects: 2 }, idle: { desktop_overview: 12 } };
  const candidate = { start: { desktop_connect: 1, desktop_list_projects: 3, desktop_load_extras: 1 }, idle: { desktop_overview: 14 } };
  assert.deepEqual(countFindings(base, candidate), [
    { scenario: "idle", cmd: "desktop_overview", base: 12, candidate: 14 },
    { scenario: "start", cmd: "desktop_load_extras", base: 0, candidate: 1 },
  ]);
});

test("call counts: fewer calls, or one more from poll timing, is no finding", () => {
  const base = { idle: { desktop_overview: 12, desktop_status: 4 } };
  const candidate = { idle: { desktop_overview: 13, desktop_status: 1 } };
  assert.deepEqual(countFindings(base, candidate), []);
});

test("call counts refuse a scenario the base never ran", () => {
  assert.throws(() => countFindings({ start: {} }, { start: {}, idle: {} }), /scenario idle/);
});

test("a quiet Mac passes the gate", () => {
  assert.deepEqual(quiet(QUIET), { quiet: true, reasons: [] });
});

test("load, compilers, memory pressure, battery, a locked screen or busy slots each refuse", () => {
  const cases = [
    [{ load1: 3.5 }, /load 3.5 > 2/],
    [{ compilers: ["rustc", "cargo"] }, /compilers running: rustc, cargo/],
    [{ memoryLevel: 30 }, /memory level 30 < 50/],
    [{ onAC: false }, /on battery/],
    [{ screenLocked: true }, /screen locked/],
    [{ busy: true }, /build slots busy/],
  ];
  for (const [change, reason] of cases) {
    const result = quiet({ ...QUIET, ...change });
    assert.equal(result.quiet, false, JSON.stringify(change));
    assert.match(result.reasons.join("\n"), reason);
  }
});

test("REFUSED exits 75 (EX_TEMPFAIL), never 0 or 1", () => {
  assert.equal(REFUSED_EXIT, 75);
});
