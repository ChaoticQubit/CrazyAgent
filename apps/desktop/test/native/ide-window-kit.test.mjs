import assert from "node:assert/strict";
import { test } from "node:test";
import { onScreen, planEnd } from "./ide-window-kit.mjs";

/** A clock the rows move by hand: `wait` adds its pause to the time and notes it. Input: the start
 * time in milliseconds. Output: the clock and the pauses it was asked for. */
function handClock(start) {
  let time = start;
  const waits = [];
  return { waits, clock: { now: () => time, wait: async (ms) => { waits.push(ms); time += ms; } } };
}

test("a page plan ends at its latest step plus the settle time, whatever order its rows come in, so the next row waits out a plan its failed row left running", () => {
  const plan = [[0, "go", "#/projects/a/ide"], [232, "note", "closed"], [90, "note", "open"]];
  assert.equal(planEnd(plan, 1_000, 5), 1_000 + 237_000);
  assert.equal(planEnd([[90, "press", "Open in its own window"]], 0, 0), 90_000);
});

test("a page plan with no steps, or a step with no time, is refused", () => {
  assert.throws(() => planEnd([], 0, 5), /a plan has steps/);
  assert.throws(() => planEnd([["soon", "go", "#/"]], 0, 5), /every plan step has a time/);
});

test("a window the window server drops from its on-screen list for a moment is found once it is back, before the step's deadline", async () => {
  const { waits, clock } = handClock(10_000);
  const answers = ["", "", "4242"];
  const found = await onScreen(() => answers.shift(), { by: 13_000, every: 200, label: "the open-92s picture: the app's window", clock });
  assert.equal(found, "4242");
  assert.deepEqual(waits, [200, 200]);
});

test("a window off the on-screen list up to the deadline fails the picture with the span it was missing, and stops looking there", async () => {
  const { waits, clock } = handClock(Date.UTC(2026, 10, 16, 6, 35, 0));
  let looks = 0;
  const missing = onScreen(() => { looks += 1; return ""; }, { by: Date.UTC(2026, 10, 16, 6, 35, 1), every: 300, label: "the moved picture: the app's window", clock });
  await assert.rejects(missing, /^AssertionError.*the moved picture: the app's window is off the window server's on-screen list from 2026-11-16T06:35:00\.000Z to 2026-11-16T06:35:01\.000Z/s);
  assert.equal(looks, 5);
  assert.deepEqual(waits, [300, 300, 300, 100]);
});

test("a window already on the list is found with no pause", async () => {
  const { waits, clock } = handClock(0);
  assert.equal(await onScreen(() => "7", { by: 1, every: 200, label: "the closed picture: the app's window", clock }), "7");
  assert.deepEqual(waits, []);
});
