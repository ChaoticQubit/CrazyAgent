import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { onScreen, planEnd } from "./ide-window-kit.mjs";

/** A clock the rows move by hand: `wait` adds its pause to the time and notes it. Input: the start
 * time in milliseconds. Output: the clock and the pauses it was asked for. */
function handClock(start) {
  let time = start;
  const waits = [];
  return { waits, clock: { now: () => time, wait: async (ms) => { waits.push(ms); time += ms; } } };
}

/** The System Events key lines in an ide-window source that do not go through realKey. Input:
 * the source. Output: the offending matches, empty when every key goes through realKey. */
function keysPastRealKey(source) {
  return source.match(/onApp\(`(?:keystroke|key code)/g) ?? [];
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

test("every ide-window row that sends a real System Events key is a keysIt row, skipped on macOS without E2E_REAL_KEYS=1, and every keystroke goes through realKey", () => {
  const source = readFileSync(new URL("./ide-window.e2e.mjs", import.meta.url), "utf8");
  const rows = source.split(/\n {2}(?=(?:it|keysIt)\()/).slice(1);
  const realKeyRows = [
    "a saved level ends the IDE's own window, the tab opens it again, and the Save's words match what happens to an unsaved edit",
    "moves to its own window with no reload, comes back, and closes with its project and the main window",
  ];
  for (const title of realKeyRows) {
    const row = rows.find((candidate) => candidate.includes(`("${title}"`));
    assert.ok(row, `the row "${title}" was found`);
    assert.ok(row.startsWith("keysIt("), `"${title}" is a keysIt row`);
  }
  for (const row of rows) assert.equal(/\b(?:realKey|commandKey)\(/.test(row), row.startsWith("keysIt("), row.slice(0, 120));
  assert.deepEqual(keysPastRealKey(source), [], "a key sent past realKey reaches whichever app is in front");
  assert.match(source, /const REAL_KEYS = process\.platform !== "darwin" \|\| process\.env\.E2E_REAL_KEYS === "1";/);
});

test("the realKey guard catches a key line in any quote and a direct osascript keystroke, and passes realKey's own lines (IDE-WINDOW-GUARD-REGEX)", () => {
  const past = [
    "onApp(`keystroke \"n\" using command down`);",
    "onApp(\"keystroke return\");",
    "onApp('key code 36');",
    "onApp(\"Keystroke \\\"w\\\" using command down\");",
    "spawnSync(\"/usr/bin/osascript\", [\"-e\", 'tell application \"System Events\" to keystroke \"q\" using command down']);",
    "const line = `key  code 53`;\n  onApp(line);",
  ];
  for (const sample of past) assert.notDeepEqual(keysPastRealKey(sample), [], `caught: ${sample}`);
  const through = [
    "await realKey(`keystroke \"${key}\" using command down`);",
    "await realKey(\"key code 36\");",
    "await realKey('keystroke \"x\"');",
    "/** Sends one real System Events keystroke line through realKey. */\nfunction realKey(line) { onApp(line); }",
    "onApp(\"set frontmost to true\");",
  ];
  for (const sample of through) assert.deepEqual(keysPastRealKey(sample), [], `passed: ${sample}`);
});
