import assert from "node:assert/strict";
import { test } from "node:test";

import { checks as nt1 } from "./n-t1-master-terminal-enter-sends.mjs";
import { checks as nt2 } from "./n-t2-sessions-list.mjs";
import { checks as nt3 } from "./n-t3-team-harness-switch.mjs";

const modules = [
  ["N-T1", nt1],
  ["N-T2", nt2],
  ["N-T3", nt3],
];

test("every tui4 module exports one check with the convention shape", () => {
  for (const [id, checks] of modules) {
    assert.ok(Array.isArray(checks), `${id}: checks is an array`);
    assert.equal(checks.length, 1, `${id}: one check per module`);
    const check = checks[0];
    assert.equal(check.id, id, `${id}: id matches the file`);
    assert.ok(Array.isArray(check.op) && check.op.length >= 1, `${id}: op lists at least one OP`);
    assert.ok(check.op.every((op) => /^OP-\d+$/.test(op)), `${id}: op ids are OP-<n>`);
    assert.ok(typeof check.title === "string" && check.title.length > 0, `${id}: title`);
    assert.ok(typeof check.why === "string" && check.why.startsWith("owner:"), `${id}: why quotes the owner`);
    assert.ok(typeof check.after === "string" && check.after.length > 0, `${id}: after`);
    assert.equal(typeof check.run, "function", `${id}: run is a function`);
    assert.equal(check.run.constructor.name, "AsyncFunction", `${id}: run is async`);
  }
});

test("the three ids are distinct", () => {
  const ids = modules.map(([, checks]) => checks[0].id);
  assert.equal(new Set(ids).size, ids.length);
});
