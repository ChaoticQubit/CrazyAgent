import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const { AccountsView, accountActions, deletePlan, deleteAccounts, deleteSummary, linkedSignIn, signInPolling } = await import("../src/screens/settings/accounts.tsx");
const { removeBody } = await import("../src/screens/settings/account-drawer.tsx");
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

test("delete leaves out accounts a pool uses and names the pools, since the engine refuses them", () => {
  const accounts = [account("a"), account("b"), keyed("c")];
  const plan = deletePlan(accounts, [pool("Main", ["b"]), pool("Spare", ["b", "x"])]);
  assert.deepEqual(plan.remove.map((item) => item.id), ["a", "c"]);
  assert.deepEqual(plan.blocked, [{ name: "Claude b", message: "Claude b is in Main and Spare. Remove it from those pools first." }]);
  assert.deepEqual(deletePlan(accounts, undefined).remove.map((item) => item.id), ["a", "b", "c"]);
  assert.equal(api.plainError("account_in_pool"), "This account is in a pool. Remove it from the pool first.");
  assert.equal(removeBody([]), "It isn't in any pool.");
  assert.equal(removeBody(["Main"]), "It is in Main. Remove it from that pool first.");
  assert.equal(removeBody(["Main", "Spare"]), "It is in Main and Spare. Remove it from those pools first.");
});

test("deleting a selection tries every account once, cancels a running sign-in first, and reports what was left", async () => {
  const { codes, note } = noted();
  const result = await deleteAccounts([account("a"), signing("b")], note);
  assert.deepEqual(result.removed, []);
  assert.deepEqual(result.failures.map((failure) => failure.name), ["Claude a", "Claude b"]);
  assert.deepEqual(codes, ["engine_unreachable", "engine_unreachable", "engine_unreachable"]);
  assert.equal(deleteSummary(result.removed, result.failures), "Couldn't delete Claude a and Claude b. CrazyAgent's engine isn't running.");
  assert.equal(deleteSummary(["Claude a", "Claude b"], []), "Deleted Claude a and Claude b.");
  assert.equal(deleteSummary(["Claude a"], [{ name: "Claude b", message: "This account is in a pool. Remove it from the pool first." }]),
    "Deleted Claude a. Couldn't delete Claude b. This account is in a pool. Remove it from the pool first.");
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
