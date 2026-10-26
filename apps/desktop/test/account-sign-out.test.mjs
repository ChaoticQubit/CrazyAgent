import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api.ts");
const { InUseNotice, canSignOut, inUseLine, releaseLabels } = await import("../src/screens/settings/account-drawer.tsx");
const { signOut } = await import("../src/screens/settings/sign-in.tsx");

const account = (patch = {}) => ({ id: "acct-1", name: "Claude C", provider: "claude", provider_name: "Claude", method: "subscription",
  method_label: "Sign in with your Claude subscription", url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });
const agent = (id, project) => ({ id, project, role: "sub_agent", label: `Team sub-agent ${id}`, harness: "claude-code", state: "running",
  status: "Working", reattached: false, busy: true, attempt: 1, can_stop: true, can_interrupt: true, can_restart: true, exit_code: null,
  reason: null, last_text: null, started_ms: 1, ended_ms: null });
const projects = [{ id: "p-alpha", name: "Alpha", repository_count: 1 }, { id: "p-beta", name: "Beta", repository_count: 1 }];
const used = (agents, holders = []) => ({ agents, holders });
const master = (project) => ({ kind: "master", project, session: "master-1" });
const research = (project, title) => ({ kind: "research", project, job: 7, title });
const forbidden = () => { throw new Error("static rendering must not act"); };

test("only a signed-in subscription account offers Sign out", () => {
  assert.equal(canSignOut(account()), true);
  assert.equal(canSignOut(account({ provider: "chatgpt", provider_name: "ChatGPT" })), true);
  assert.equal(canSignOut(account({ status: "not_signed_in", status_label: "Not signed in" })), false);
  assert.equal(canSignOut(account({ status: "signing_in", status_label: "Signing in..." })), false);
  assert.equal(canSignOut(account({ provider: "openrouter", method: "api_key", status: "key_saved" })), false);
});

test("a refused sign-out names the agents using the account and their projects", () => {
  assert.equal(inUseLine(used([agent("3", "p-alpha")]), projects), "Team sub-agent 3 in Alpha is using this account.");
  assert.equal(inUseLine(used([agent("3", "p-alpha"), agent("4", "p-alpha")]), projects),
    "Team sub-agent 3 and Team sub-agent 4 in Alpha are using this account.");
  assert.equal(inUseLine(used([agent("3", "p-alpha"), agent("5", "p-beta")]), projects),
    "Team sub-agent 3 in Alpha and Team sub-agent 5 in Beta are using this account.");
  assert.equal(inUseLine(used([agent("6", "p-gone")]), projects), "Team sub-agent 6 in another project is using this account.");
  assert.equal(inUseLine(used([agent("6", "p-gone")]), undefined), "Team sub-agent 6 in another project is using this account.");
});

test("a refused sign-out names a master conversation and a research job with their projects", () => {
  assert.equal(inUseLine(used([], [master("p-alpha")]), projects), "The master conversation in Alpha is using this account.");
  assert.equal(inUseLine(used([agent("3", "p-alpha")], [master("p-alpha"), research("p-beta", "Market research")]), projects),
    "Team sub-agent 3 and the master conversation in Alpha and Market research in Beta are using this account.");
  assert.deepEqual(releaseLabels(used([agent("3", "p-alpha")])), { label: "Stop agents and sign out", pending: "Stopping agents..." });
  assert.deepEqual(releaseLabels(used([], [research("p-beta", "Market research")])), { label: "Stop agents and sign out", pending: "Stopping agents..." });
  assert.deepEqual(releaseLabels(used([], [master("p-alpha")])), { label: "End conversation and sign out", pending: "Ending conversation..." });
  assert.deepEqual(releaseLabels(used([agent("3", "p-alpha")], [master("p-alpha")])),
    { label: "Stop agents, end conversation and sign out", pending: "Ending..." });
});

test("the in-use notice offers one action that stops those agents and signs out", () => {
  const markup = renderToStaticMarkup(createElement(InUseNotice, { used: used([agent("3", "p-alpha")]), projects, pending: false, onStop: forbidden }));
  assert.match(markup, /Team sub-agent 3 in Alpha is using this account\./);
  assert.match(markup, />Stop agents and sign out<\/button>/);
  const conversation = renderToStaticMarkup(createElement(InUseNotice, { used: used([], [master("p-alpha")]), projects, pending: false, onStop: forbidden }));
  assert.match(conversation, /The master conversation in Alpha is using this account\./);
  assert.match(conversation, />End conversation and sign out<\/button>/);
  const unnamed = renderToStaticMarkup(createElement(InUseNotice, { used: used([]), projects, pending: false, onStop: forbidden }));
  assert.match(unnamed, new RegExp(api.plainError("native_profile_in_use").replace(/[.]/g, "\\.")));
  assert.doesNotMatch(unnamed, /Stop agents and sign out/);
});

test("signing out, and reading or releasing what uses an account, reach the engine", async () => {
  for (const result of [await signOut("acct-1"), await api.accountUse("acct-1"), await api.releaseAccount("acct-1")]) {
    assert.equal(result.kind, "error");
    assert.equal(result.code, "engine_unreachable");
  }
});
