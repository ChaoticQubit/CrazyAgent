import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const io = await import("../src/screens/team/terminal-io.ts");

const SLOW_CODES = ["terminal_outcome_unknown", "terminal_attach_timeout", "terminal_not_answering", "terminal_attach_failed"];

test("a live master view whose terminal opens and closes at once on every try stops after its tries instead of connecting forever", () => {
  let tries = 0;
  let now = 1_000_000;
  let count = 0;
  for (; count < io.FOLLOW_TRIES * 10; count += 1) {
    const opened = now;
    now += 200;
    const next = io.followTry(tries, opened, now);
    if (next === null) break;
    tries = next;
    now += 1000;
  }
  assert.equal(count, io.FOLLOW_TRIES, "a view that never stays open spends its tries and stops with Open again");
});

test("a view that never opens stops after its tries, and one that stayed open a minute starts its tries afresh", () => {
  let tries = 0;
  let count = 0;
  while (io.followTry(tries, null, 5_000) !== null) {
    tries = io.followTry(tries, null, 5_000);
    count += 1;
    assert.ok(count <= io.FOLLOW_TRIES);
  }
  assert.equal(count, io.FOLLOW_TRIES);
  assert.equal(io.followTry(io.FOLLOW_TRIES, 0, 60_000), 1, "an agent that ran a while and restarted in a new pane is followed again");
});

test("a clock that steps back between a view opening and its end counts a try instead of freezing the view (master r1 F1)", () => {
  assert.equal(io.followTry(3, 10_000, 5_000), 4, "an end that reads earlier than the open is not a steady view, so it spends one try");
  assert.equal(io.followTry(io.FOLLOW_TRIES, 10_000, 5_000), null, "a view out of tries still stops with Open again");
});

test("a view trying again says which try it is and why the last one ended", () => {
  assert.equal(io.connectingLine(null, 0), "Connecting to the terminal...");
  const reason = io.endCopy("terminal_outcome_unknown");
  const line = io.connectingLine(reason, 3);
  assert.match(line, new RegExp(`try 3 of ${io.FOLLOW_TRIES}`));
  assert.ok(line.includes(reason), `the line names the last reason: ${line}`);
});

test("a terminal that did not open in time says so in plain words, not only that it closed", () => {
  const generic = io.endCopy("no_such_code");
  for (const code of SLOW_CODES) {
    const copy = io.endCopy(code);
    assert.notEqual(copy, generic, `${code} has its own words`);
    assert.doesNotMatch(copy, /terminal_|_/, `${code} reads as plain words`);
  }
});
