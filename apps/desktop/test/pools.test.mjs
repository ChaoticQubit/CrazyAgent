import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { toggleMember, notSignedInNote, deleteCopy, poolReadiness, poolUse, PoolsScreenView } = await import("../src/screens/settings/pools.tsx");
const { poolInput, POOL_MEMBER_MAX } = await import("../src/api.ts");

const account = (index, patch = {}) => ({ id: `acct-${index}`, name: `Account ${index}`, provider: "chatgpt", provider_name: "ChatGPT / Codex",
  method: "subscription", method_label: "Sign in with ChatGPT", url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });
const member = (index, patch = {}) => ({ id: `acct-${index}`, kind: "chatgpt", provider: "chatgpt", status: "signed_in", status_label: "Signed in", ...patch });
const pool = (count, patch = {}) => ({ id: "pool-main", name: "Main", summary: `${count} ChatGPT`, members: Array.from({ length: count }, (_, index) => member(index)), project_count: 0, ...patch });
const forbidden = () => { throw new Error("static rendering must not save or navigate"); };

test("the 16th account is refused and removing one frees a place", () => {
  assert.equal(POOL_MEMBER_MAX, 15);
  let members = [];
  for (let index = 0; index < 15; index += 1) {
    const next = toggleMember(members, `acct-${index}`);
    assert.equal(next.kind, "ok");
    members = next.members;
  }
  assert.equal(members.length, 15);
  assert.deepEqual(toggleMember(members, "acct-15"), { kind: "full" });
  const removed = toggleMember(members, "acct-3");
  assert.equal(removed.kind, "ok");
  assert.equal(removed.members.length, 14);
  assert.ok(!removed.members.includes("acct-3"));
  assert.deepEqual(toggleMember(removed.members, "acct-15").members.at(-1), "acct-15");
  assert.equal(poolInput("Main", members).kind, "ok");
  assert.equal(poolInput("Main", [...members, "acct-15"]).code, "pool_member_limit");
  assert.equal(poolInput("  ", []).message, "Enter a pool name.");
  assert.equal(poolInput("Main", ["a", "a"]).code, "pool_duplicate_member");
});

test("pool lines: readiness, use, the not-signed-in note and the delete refusal", () => {
  assert.equal(poolReadiness([]), "No accounts");
  assert.equal(poolReadiness([member(0), member(1, { status: "not_signed_in" })]), "1 ready, 1 not signed in");
  assert.equal(poolUse(0), "Not used");
  assert.equal(poolUse(2), "Used by 2 projects");
  const status = new Map([["a", "not_signed_in"], ["b", "signed_in"], ["c", "not_signed_in"]]);
  assert.equal(notSignedInNote(["b"], status), undefined);
  assert.equal(notSignedInNote(["a", "b"], status), "1 account isn't signed in. Agents skip it until you sign in again.");
  assert.equal(notSignedInNote(["a", "c"], status), "2 accounts aren't signed in. Agents skip them until you sign in again.");
  assert.deepEqual(deleteCopy(pool(1), undefined), { body: "Its accounts stay in your list. Only the pool goes away.", confirmLabel: "Delete pool", done: "Main deleted." });
  assert.deepEqual(deleteCopy(pool(1, { project_count: 2 }), ["Harbor", "Atlas"]), {
    body: "Used by Harbor and Atlas. They will have no pool until you pick one. Its accounts stay in your list.",
    confirmLabel: "Unassign and delete", done: "Main deleted. Its projects have no pool now. Pick a pool to start in each project's Settings." });
  assert.equal(deleteCopy(pool(1, { project_count: 1 }), undefined).body, "Used by 1 project. It will have no pool until you pick one. Its accounts stay in your list.");
  assert.equal(deleteCopy(pool(1, { project_count: 1 }), undefined).confirmLabel, "Unassign and delete");
});

const NOT_READ = { kind: "loading" };
const screen = (pools) => renderToStaticMarkup(createElement(PoolsScreenView, { pools: { kind: "ok", value: pools }, accounts: NOT_READ, schedule: NOT_READ, ranking: NOT_READ, limits: NOT_READ, nowMs: 0,
  selected: undefined, onSelect: forbidden, onNew: forbidden, onDelete: forbidden, onRetry: forbidden, onRetryOrder: forbidden }));

test("the pool list shows count, readiness and use, and the selected pool has its name field and Delete", () => {
  const markup = screen([pool(4, { project_count: 1 })]);
  assert.match(markup, />4 of 15</);
  assert.match(markup, />4 ready\. Used by 1 project\.</);
  assert.match(markup, /aria-label="Pool name"[^>]*value="Main"/);
  assert.match(markup, /aria-label="Delete pool Main"[^>]*>Delete pool</);
  const empty = screen([]);
  assert.match(empty, /<h2>No pools yet<\/h2>/);
  assert.match(empty, /A pool groups up to 15 accounts for your projects to use\./);
});
