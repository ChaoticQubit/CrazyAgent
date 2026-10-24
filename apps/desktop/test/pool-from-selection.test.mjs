import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { AccountsView, toggleSelected, poolFromSelection } = await import("../src/screens/settings/accounts.tsx");
const { PoolEditorView, editorAccounts } = await import("../src/screens/settings/pools.tsx");
const { startPoolWith, takePoolDraft } = await import("../src/screens/settings/pool-draft.ts");
const { POOL_MEMBER_MAX } = await import("../src/api.ts");

const account = (index, patch = {}) => ({ id: `acct-${index}`, name: `Account ${index}`, provider: "chatgpt", provider_name: "ChatGPT / Codex",
  method: "subscription", method_label: "Sign in with ChatGPT", url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });
const forbidden = () => { throw new Error("static rendering must not save or navigate"); };

test("selecting accounts toggles each one and keeps the order picked", () => {
  let selected = [];
  selected = toggleSelected(selected, "acct-2");
  selected = toggleSelected(selected, "acct-0");
  selected = toggleSelected(selected, "acct-5");
  assert.deepEqual(selected, ["acct-2", "acct-0", "acct-5"]);
  selected = toggleSelected(selected, "acct-0");
  assert.deepEqual(selected, ["acct-2", "acct-5"]);
});

test("a new pool from the selection needs 1 to 15 accounts", () => {
  assert.deepEqual(poolFromSelection([]), { kind: "blocked", reason: "Select accounts first." });
  assert.deepEqual(poolFromSelection(["acct-0"]), { kind: "ready" });
  const full = Array.from({ length: POOL_MEMBER_MAX }, (_, index) => `acct-${index}`);
  assert.deepEqual(poolFromSelection(full), { kind: "ready" });
  assert.deepEqual(poolFromSelection([...full, "acct-15"]), { kind: "blocked", reason: "A pool holds up to 15 accounts. 16 are selected." });
});

test("the picked accounts reach the next new pool once", () => {
  assert.deepEqual(takePoolDraft(), []);
  startPoolWith(["acct-3", "acct-1"]);
  assert.deepEqual(takePoolDraft(), ["acct-3", "acct-1"]);
  assert.deepEqual(takePoolDraft(), []);
  assert.throws(() => startPoolWith([]), /1 to 15 accounts/);
  assert.throws(() => startPoolWith(Array.from({ length: 16 }, (_, index) => `acct-${index}`)), /1 to 15 accounts/);
  assert.throws(() => startPoolWith(["acct-1", "acct-1"]), /picked once/);
  assert.deepEqual(takePoolDraft(), []);
});

test("the account list shows a select box per row only when it can start a pool", () => {
  const many = Array.from({ length: 1000 }, (_, index) => account(index));
  const selectable = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "ok", value: many }, pools: { kind: "ok", value: [] },
    onAdd: forbidden, onOpen: forbidden, onRetry: forbidden, onNewPool: forbidden }));
  const boxes = selectable.match(/class="account-pick"/g) ?? [];
  const rows = selectable.match(/class="account-row"/g) ?? [];
  assert.ok(boxes.length > 0 && boxes.length === rows.length, `rendered ${boxes.length} boxes for ${rows.length} rows`);
  assert.ok(rows.length < 60, `rendered ${rows.length} rows`);
  assert.match(selectable, /aria-label="Select Account 0"/);
  assert.match(selectable, /class="accounts-selectable"/);
  assert.doesNotMatch(selectable, /New pool from selected/);
  const plain = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "ok", value: many }, pools: { kind: "ok", value: [] },
    onAdd: forbidden, onOpen: forbidden, onRetry: forbidden }));
  assert.doesNotMatch(plain, /account-pick|accounts-selectable/);
});

test("a new pool opens with the picked accounts as members", () => {
  const accounts = Array.from({ length: 20 }, (_, index) => account(index));
  const markup = renderToStaticMarkup(createElement(PoolEditorView, { pool: undefined, accounts: { kind: "ok", value: accounts },
    initialMembers: ["acct-4", "acct-7"], onSave: forbidden, onCancel: forbidden }));
  assert.match(markup, /<h2>New pool<\/h2>/);
  assert.match(markup, /2 of 15/);
  assert.match(markup, /aria-label="Remove Account 4"/);
  assert.match(markup, /aria-label="Remove Account 7"/);
  assert.match(markup, />Selected only</);
  const over = renderToStaticMarkup(createElement(PoolEditorView, { pool: undefined, accounts: { kind: "ok", value: accounts },
    initialMembers: accounts.map((item) => item.id), onSave: forbidden, onCancel: forbidden }));
  assert.match(over, /15 of 15/);
});

test("Selected only lists just the members, after search and provider", () => {
  const accounts = [account(0), account(1, { provider: "claude", provider_name: "Claude" }), account(2), account(3, { name: "Work" })];
  assert.equal(editorAccounts(accounts, "", "", [], false).length, 4);
  assert.deepEqual(editorAccounts(accounts, "", "", ["acct-1", "acct-2"], true).map((item) => item.id), ["acct-1", "acct-2"]);
  assert.deepEqual(editorAccounts(accounts, "", "chatgpt", ["acct-1", "acct-2"], true).map((item) => item.id), ["acct-2"]);
  assert.deepEqual(editorAccounts(accounts, "work", "", ["acct-1", "acct-3"], true).map((item) => item.id), ["acct-3"]);
  assert.deepEqual(editorAccounts(accounts, "", "", [], true), []);
});
