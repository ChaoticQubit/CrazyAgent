import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { toggleMember, notSignedInNote, deleteCopy, poolReadiness, poolUse, PoolEditorView, PoolsView, POOL_FULL } = await import("../src/screens/settings/pools.tsx");
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

test("a full pool marks unchecked rows unavailable but focusable and shows the limit notice", () => {
  const accounts = Array.from({ length: 40 }, (_, index) => account(index, index === 2 ? { status: "not_signed_in", status_label: "Not signed in" } : {}));
  const markup = renderToStaticMarkup(createElement(PoolEditorView, { pool: pool(15, { members: Array.from({ length: 15 }, (_, index) => member(index, index === 2 ? { status: "not_signed_in", status_label: "Not signed in" } : {})) }),
    accounts: { kind: "ok", value: accounts }, onSave: forbidden, onCancel: forbidden }));
  assert.match(markup, /<h2>Edit Main<\/h2>/);
  assert.match(markup, /<span class="capacity-count" aria-live="polite">15 of 15<\/span>/);
  assert.equal((markup.match(/<span class="on"><\/span>/g) ?? []).length, 15);
  assert.ok(markup.includes(`id="pool-full-notice"`));
  assert.ok(markup.includes(POOL_FULL));
  assert.match(markup, /1 account isn&#x27;t signed in\. Agents skip it until you sign in again\./);
  const checked = markup.match(/<input type="checkbox" checked=""/g) ?? [];
  assert.equal(checked.length, 15);
  const unavailable = markup.match(/<input type="checkbox" aria-disabled="true"/g) ?? [];
  assert.ok(unavailable.length > 0, "unchecked rows are aria-disabled");
  assert.doesNotMatch(markup, /<input type="checkbox"[^>]*\sdisabled=""/, "unavailable rows stay focusable");
  assert.equal((markup.match(/class="member"/g) ?? []).length, 15);
  assert.match(markup, /aria-label="Remove Account 0"/);
  assert.match(markup, />Save pool<\/button>/);
  assert.match(markup, /Back to pools/);
});

test("the pool editor keeps a member the account list does not show, with explicit checkboxes for the rest", () => {
  const saved = pool(2, { members: [member(90), member(1)] });
  const markup = renderToStaticMarkup(createElement(PoolEditorView, { pool: saved, accounts: { kind: "ok", value: [account(1), account(2)] },
    onSave: forbidden, onCancel: forbidden }));
  const rows = [...markup.matchAll(/<label class="pick"[^>]*>([\s\S]*?)<\/label>/g)].map((match) => match[1]);
  const row = (name) => rows.find((text) => text.includes(`>${name}<`));
  assert.match(row("Account 1"), /<input type="checkbox" checked=""/);
  assert.match(row("Account 2"), /<input type="checkbox"/);
  assert.doesNotMatch(row("Account 2"), /checked=""/);
  for (const text of rows) assert.doesNotMatch(text, /\sdisabled=""/);
  assert.match(markup, /2 of 15/);
  assert.equal((markup.match(/class="member"/g) ?? []).length, 2, "a member missing from the account list stays in the pool");
  assert.deepEqual(saved.members.map((item) => item.id), ["acct-90", "acct-1"]);
});

test("a new pool starts empty and windows a long account list", () => {
  const accounts = Array.from({ length: 1000 }, (_, index) => account(index));
  const markup = renderToStaticMarkup(createElement(PoolEditorView, { pool: undefined, accounts: { kind: "ok", value: accounts }, onSave: forbidden, onCancel: forbidden }));
  assert.match(markup, /<h2>New pool<\/h2>/);
  assert.match(markup, /0 of 15/);
  assert.doesNotMatch(markup, /pool-full-notice|aria-disabled="true"/);
  assert.match(markup, /No accounts yet\. Check accounts on the left to add them\./);
  const rows = markup.match(/class="pick"/g) ?? [];
  assert.ok(rows.length > 0 && rows.length < 60, `rendered ${rows.length} rows`);
  const none = renderToStaticMarkup(createElement(PoolEditorView, { pool: undefined, accounts: { kind: "ok", value: [] }, onSave: forbidden, onCancel: forbidden }));
  assert.match(none, /href="#\/settings\/accounts\?action=add"/);
});

test("the pool list shows count, readiness and use with Edit and Delete in a menu", () => {
  const markup = renderToStaticMarkup(createElement(PoolsView, { pools: { kind: "ok", value: [pool(4, { project_count: 1 })] },
    onNew: forbidden, onEdit: forbidden, onDelete: forbidden, onRetry: forbidden }));
  assert.match(markup, />4 of 15</);
  assert.match(markup, />4 ready</);
  assert.match(markup, />Used by 1 project</);
  assert.match(markup, /aria-label="More for Main"/);
  const empty = renderToStaticMarkup(createElement(PoolsView, { pools: { kind: "ok", value: [] }, onNew: forbidden, onEdit: forbidden, onDelete: forbidden, onRetry: forbidden }));
  assert.match(empty, /<h2>No pools yet<\/h2>/);
  assert.match(empty, /A pool groups up to 15 accounts for your projects to use\./);
});
