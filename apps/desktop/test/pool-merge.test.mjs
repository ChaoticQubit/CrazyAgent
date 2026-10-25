import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { mergePool } = await import("../src/api.ts");

const pool = (name, members) => ({ name, members });

test("a rename saved from a stale editor keeps an account another client removed meanwhile out", () => {
  const opened = pool("Main", ["acct-1", "acct-leaked", "acct-3"]);
  const now = pool("Main", ["acct-1", "acct-3"]);
  const saved = pool("Main team", ["acct-1", "acct-leaked", "acct-3"]);
  assert.deepEqual(mergePool(now, opened, saved), { kind: "ok", value: pool("Main team", ["acct-1", "acct-3"]) });
});

test("the owner's own adds and removals apply on top of the pool as it is now", () => {
  const opened = pool("Main", ["acct-1", "acct-2", "acct-3"]);
  const now = pool("Renamed elsewhere", ["acct-1", "acct-2", "acct-3", "acct-9"]);
  const saved = pool("Main", ["acct-1", "acct-3", "acct-4"]);
  assert.deepEqual(mergePool(now, opened, saved), { kind: "ok", value: pool("Renamed elsewhere", ["acct-1", "acct-3", "acct-9", "acct-4"]) },
    "an unchanged name keeps the other client's rename, and its added member stays");
});

test("adding one account to a pool from Add account keeps every member added meanwhile", () => {
  const listed = pool("Main", ["acct-1"]);
  const now = pool("Main", ["acct-1", "acct-2"]);
  assert.deepEqual(mergePool(now, listed, pool("Main", ["acct-1", "acct-new"])), { kind: "ok", value: pool("Main", ["acct-1", "acct-2", "acct-new"]) });
  assert.deepEqual(mergePool(pool("Main", ["acct-new"]), listed, pool("Main", ["acct-1", "acct-new"])), { kind: "ok", value: pool("Main", ["acct-new"]) },
    "an account already in it is not added twice, and a removal elsewhere stands");
});

test("a merge that would pass 15 accounts is refused with the plain limit", () => {
  const fifteen = Array.from({ length: 15 }, (_, index) => `acct-${index}`);
  const opened = pool("Main", fifteen.slice(0, 14));
  const now = pool("Main", fifteen);
  const merged = mergePool(now, opened, pool("Main", [...fifteen.slice(0, 14), "acct-new"]));
  assert.equal(merged.kind, "error");
  assert.equal(merged.code, "pool_member_limit");
});
