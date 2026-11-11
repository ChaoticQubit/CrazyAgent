import assert from "node:assert/strict";
import { test } from "node:test";
import { checks, clearStep } from "./trial4/n-d1-sign-in-again.mjs";

const UP = { up: true, busy: false };
const CLEARING = { up: true, busy: true };
const NONE = { up: false, busy: false };

test("N-D1 is one trial 4 check for OP-7 that runs at the end", () => {
  assert.deepEqual(checks.map((check) => [check.id, check.op, check.after]), [["N-D1", ["OP-7"], "end"]]);
});

test("N-D1 cleanup clears its account's stuck sign-in with the app's command when the window shows no banner", () => {
  assert.equal(clearStep({ stuck: "acct-1", id: "acct-1", banner: NONE }), "ask");
});

test("N-D1 cleanup uses the banner's Clear stuck sign-in when the window shows it, and waits while it reads Clearing...", () => {
  assert.equal(clearStep({ stuck: "acct-1", id: "acct-1", banner: UP }), "click");
  assert.equal(clearStep({ stuck: "acct-1", id: "acct-1", banner: CLEARING }), "wait");
});

test("N-D1 cleanup is done only once the app reads no stuck sign-in for its account, whatever the banner shows", () => {
  assert.equal(clearStep({ stuck: null, id: "acct-1", banner: NONE }), "done");
  assert.equal(clearStep({ stuck: null, id: "acct-1", banner: UP }), "done");
});

test("N-D1 cleanup leaves another account's stuck sign-in alone", () => {
  assert.equal(clearStep({ stuck: "owner-account", id: "acct-1", banner: UP }), "done");
  assert.equal(clearStep({ stuck: "owner-account", id: "acct-1", banner: NONE }), "done");
});
