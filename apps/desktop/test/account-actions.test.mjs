import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const { AccountsView, accountActions, deleteDialog, deleteAccounts, deleteSummary, linkedSignIn, signInPolling } = await import("../src/screens/settings/accounts.tsx");
const { poolEffect } = await import("../src/screens/settings/account-drawer.tsx");
const { cancelSignIn, cancelError, discardAccount } = await import("../src/screens/settings/sign-in.tsx");

const account = (id, patch = {}) => ({ id, name: `Claude ${id}`, provider: "claude", provider_name: "Claude", method: "subscription",
  method_label: "Sign in with your Claude subscription", url: null, model: null, status: "not_signed_in", status_label: "Not signed in", ...patch });
const signing = (id) => account(id, { status: "signing_in", status_label: "Signing in..." });
const keyed = (id) => account(id, { provider: "openrouter", provider_name: "OpenRouter", method: "api_key", method_label: "Use an API key", status: "key_saved", status_label: "Key saved" });
const member = (id) => ({ id, kind: "claude", provider: "claude", status: "not_signed_in", status_label: "Not signed in" });
const pool = (name, ids) => ({ id: `pool-${name}`, name, summary: "", members: ids.map(member), project_count: 0 });
const noted = () => {
  const codes = [];
  return { codes, note: (result) => { codes.push(result.kind === "error" ? result.code : result.kind); return result; } };
};
const forbidden = () => { throw new Error("static rendering must not act"); };

test("each account offers the actions its state allows: sign in, cancel a running sign-in, details and delete", () => {
  assert.deepEqual(accountActions(account("a")), ["sign_in", "details", "delete"]);
  assert.deepEqual(accountActions(signing("b")), ["cancel_sign_in", "details", "delete"]);
  assert.deepEqual(accountActions(account("c", { status: "signed_in", status_label: "Signed in" })), ["details", "delete"]);
  assert.deepEqual(accountActions(keyed("d")), ["details", "delete"]);
  assert.equal(signInPolling([account("a"), keyed("d")]), false);
  assert.equal(signInPolling([account("a"), signing("b")]), true);
});

test("a Sign in again link opens that account with its sign-in, or its details once it needs none", () => {
  const ok = (value) => ({ kind: "ok", value });
  const list = [account("a"), account("b", { status: "signed_in", status_label: "Signed in" }), signing("c")];
  assert.deepEqual(linkedSignIn(ok(list), "a"), { account: list[0], signIn: true });
  assert.deepEqual(linkedSignIn(ok(list), "b"), { account: list[1], signIn: false });
  assert.deepEqual(linkedSignIn(ok(list), "c"), { account: list[2], signIn: false });
  assert.equal(linkedSignIn(ok(list), "gone"), undefined);
  assert.equal(linkedSignIn({ kind: "loading" }, "a"), undefined);
});

test("deleting accounts in pools names the pools they leave and the pools deleted with them, then removes them all", () => {
  const accounts = [account("a"), account("b"), keyed("c")];
  const pools = [pool("Main", ["b"]), pool("Spare", ["b", "x"])];
  const many = deleteDialog(accounts, pools);
  assert.equal(many.title, "Delete 3 accounts?");
  assert.equal(many.confirmLabel, "Remove from pools and delete");
  assert.deepEqual(many.lines, [
    "Claude a, Claude b and Claude c and their saved sign-ins or keys are removed from CrazyAgent. A running sign-in is cancelled first.",
    "They leave pools Main and Spare. Main has no other account, so it is deleted too and its projects have no pool.",
  ]);
  const one = deleteDialog([accounts[1]], [pool("Spare", ["b", "x"])]);
  assert.equal(one.title, "Delete Claude b?");
  assert.equal(one.confirmLabel, "Remove from pools and delete");
  assert.deepEqual(one.lines.slice(1), ["It leaves pool Spare."]);
  assert.equal(deleteDialog([accounts[0]], pools).confirmLabel, "Delete account");
  assert.equal(deleteDialog(accounts, undefined).confirmLabel, "Delete 3 accounts");
  assert.equal(deleteDialog(accounts, undefined).lines.length, 1);
  assert.equal(poolEffect(["a"], pools), undefined);
  assert.equal(poolEffect(["b", "x"], pools), "They leave pools Main and Spare. Main and Spare have no other account, so they are deleted too and their projects have no pool.");
  assert.equal(api.knownError("account_in_pool"), undefined, "account_in_pool is no longer a refusal, so it has no sentence of its own");
});

test("deleting a selection tries every account once, cancels a running sign-in first, and reports what was left", async () => {
  const { codes, note } = noted();
  const result = await deleteAccounts([account("a"), signing("b")], note);
  assert.deepEqual(result.removed, []);
  assert.deepEqual(result.failures.map((failure) => failure.name), ["Claude a", "Claude b"]);
  assert.deepEqual(codes, ["engine_unreachable", "engine_unreachable", "engine_unreachable"]);
  assert.equal(deleteSummary(result.removed, result.failures), "Couldn't delete Claude a and Claude b. CrazyAgent's engine isn't running.");
  assert.equal(deleteSummary(["Claude a", "Claude b"], []), "Deleted Claude a and Claude b.");
  assert.equal(deleteSummary(["Claude a"], [{ name: "Claude b", message: "This account no longer exists." }]),
    "Deleted Claude a. Couldn't delete Claude b. This account no longer exists.");
});

test("cancelling a sign-in reaches the engine and says plainly when the engine can no longer stop it", async () => {
  const result = await cancelSignIn("acct-1");
  assert.equal(result.kind, "error");
  assert.equal(result.code, "engine_unreachable");
  for (const code of ["native_auth_operation_missing", "native_profile_source_conflict", "native_auth_operation_scope_conflict", "native_auth_recovery_required"]) {
    assert.equal(cancelError(code), "CrazyAgent can't stop this sign-in from here. Delete the account to clear it, then add it again.", code);
  }
  assert.equal(cancelError("native_auth_not_active"), undefined);
});

test("a change refused while a sign-in runs, or a cancel still stopping, says so plainly", () => {
  assert.equal(api.plainError("native_auth_recovery_required"), "Another sign-in is still running or closing. Finish or cancel it, or wait a minute, then try again. If this keeps happening, restart CrazyAgent.");
  assert.equal(api.plainError("native_auth_cancel_pending"), "The sign-in is still stopping. Try again in a moment.");
});

test("an abandoned Add account removes its half-made account, or says where it stays", async () => {
  assert.equal(await discardAccount(account("a"), (result) => result),
    "Couldn't remove the unfinished account Claude a. It stays in your list as Not signed in. Delete it there.");
});

test("rows carry an actions menu and a selection offers Delete selected and Cancel sign-in", () => {
  const accounts = [account("a"), signing("b")];
  const markup = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "ok", value: accounts }, pools: { kind: "ok", value: [] },
    onAdd: forbidden, onOpen: forbidden, onRetry: forbidden, onNewPool: forbidden, onAction: forbidden, onDeleteSelected: forbidden,
    onCancelSelected: forbidden, initialSelection: ["a", "b"] }));
  assert.match(markup, /aria-label="More for Claude a"/);
  assert.match(markup, /aria-label="More for Claude b"/);
  assert.match(markup, /2 selected/);
  assert.match(markup, />Delete selected<\/button>/);
  assert.match(markup, />Cancel sign-in<\/button>/);
  assert.match(markup, />New pool from selected<\/button>/);
  const idle = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "ok", value: [account("a")] }, pools: { kind: "ok", value: [] },
    onAdd: forbidden, onOpen: forbidden, onRetry: forbidden, onNewPool: forbidden, onAction: forbidden, onDeleteSelected: forbidden,
    onCancelSelected: forbidden, initialSelection: ["a"] }));
  assert.doesNotMatch(idle, />Cancel sign-in<\/button>/);
  assert.match(idle, />Delete selected<\/button>/);
});
