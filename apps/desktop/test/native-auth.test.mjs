import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const { reduceNativeAuth } = await import("../src/native-auth.ts");
const { SignInPanel } = await import("../src/screens/settings/sign-in.tsx");

function syntheticDetail() {
  return {
    ticket: "17",
    collection: "connections",
    revision: "23",
    config_id: "synthetic-config",
    source_hash: "a".repeat(64),
    writable: false,
    fresh: true,
    record: {
      kind: "connection",
      connection: {
        id: "connection-a",
        provider: "provider-a",
        harness: "codex",
        protocol: "native",
        account_ref: "account-a",
        endpoint: { kind: "native" },
        model: { id: "model-a", author: "author-a" },
        auth: { method: "native_managed", profile_ref: "credential-profile-a" },
      },
    },
  };
}

function syntheticReply(patch = {}) {
  return {
    ticket: "17",
    request_id: "synthetic-owner-start",
    operation_id: "synthetic-owner-start",
    database_instance: "b".repeat(64),
    engine_epoch: "c".repeat(64),
    connection: "connection-a",
    config_id: "synthetic-config",
    source_hash: "a".repeat(64),
    generation: "11",
    action: { kind: "sign_in", method: "codex_browser" },
    phase: "awaiting_owner",
    code: null,
    challenge: {
      kind: "codex_browser",
      auth_url: "https://auth.example.invalid/login?state=SYNTHETIC_STATE&client=codex",
    },
    account: null,
    ...patch,
  };
}

function opened(detail = syntheticDetail(), navigation = 7) {
  return reduceNativeAuth(undefined, { kind: "open", detail, navigation });
}

function presented(reply = syntheticReply(), detail = syntheticDetail(), action = "codex_browser") {
  const queued = reduceNativeAuth(opened(detail), { kind: "begin", action, request: 1 });
  return reduceNativeAuth(queued, { kind: "reply", navigation: 7, request: 1, reply });
}

function forbiddenEffect() {
  throw new Error("static rendering must not invoke an owner operation");
}

function markup(state, props = {}) {
  const harness = state?.context.harness;
  return renderToStaticMarkup(createElement(SignInPanel, {
    provider: harness === "claude" ? "claude" : "chatgpt", method: harness === "claude" ? "claude_official" : "codex_device_code",
    state, onStart: forbiddenEffect, onCancel: forbiddenEffect, onCheck: forbiddenEffect, onClaudeCode: forbiddenEffect, ...props,
  }));
}

test("synthetic typed selection opens inspectable auth without starting or editing metadata", () => {
  const detail = syntheticDetail();
  const original = structuredClone(detail);
  const state = opened(detail);
  assert.equal(state.open, true);
  assert.equal(state.context.ticket, detail.ticket);
  assert.equal(state.context.connection, detail.record.connection.id);
  assert.equal(state.context.config_id, detail.config_id);
  assert.equal(state.context.source_hash, detail.source_hash);
  assert.equal(state.context.navigation, 7);
  assert.equal(state.in_flight, undefined);
  assert.equal(state.operation, undefined);
  assert.equal(state.presentation, undefined);
  assert.deepEqual(detail, original);
  const browser = reduceNativeAuth(state, { kind: "begin", action: "codex_browser", request: 1 });
  assert.deepEqual(browser.in_flight, { action: "codex_browser", request: 1 });
  assert.equal(browser.operation, undefined);
  assert.equal(state.in_flight, undefined);
  const existing = reduceNativeAuth(state, { kind: "begin", action: "codex_existing", request: 1 });
  assert.deepEqual(existing.in_flight, { action: "codex_existing", request: 1 });
  assert.match(markup(state, { method: "codex_existing" }), /Codex itself is not changed\.[\s\S]*>Use this sign-in<\/button>/);
  assert.match(markup(state), />Get sign-in code<\/button>/);
  const wrong = reduceNativeAuth(state, { kind: "begin", action: "claude_official", request: 1 });
  assert.equal(wrong.in_flight, undefined);
  const claude = structuredClone(detail);
  claude.record.connection.harness = "claude";
  assert.deepEqual(
    reduceNativeAuth(opened(claude), { kind: "begin", action: "claude_official", request: 1 }).in_flight,
    { action: "claude_official", request: 1 },
  );
  for (const invalid of [
    { ...detail, fresh: false },
    { ...detail, collection: "accounts", record: { kind: "account", account_ref: "account-a" } },
    { ...detail, collection: "pools", record: { kind: "pool", pool: { id: "pool" } } },
    { ...detail, record: { kind: "connection", connection: { ...detail.record.connection, auth: { method: "none" } } } },
    { ...detail, record: { kind: "connection", connection: { ...detail.record.connection, endpoint: { kind: "http", url: "https://example.invalid" } } } },
  ]) {
    const blocked = opened(invalid);
    assert.equal(blocked.open, true);
    assert.ok(blocked.error);
    assert.equal(reduceNativeAuth(blocked, { kind: "begin", action: "codex_browser", request: 1 }).in_flight, undefined);
    assert.match(markup(blocked), /role="alert"/);
    assert.doesNotMatch(markup(blocked), /Get sign-in code|Use this sign-in/);
  }
});

test("synthetic typed replies attach only to the exact active selection and dispatch", () => {
  const initial = reduceNativeAuth(opened(), { kind: "begin", action: "codex_browser", request: 1 });
  const reply = syntheticReply();
  for (const patch of [
    { ticket: "18" }, { connection: "connection-z" }, { config_id: "other-config" },
    { source_hash: "d".repeat(64) }, { action: { kind: "logout" } },
  ]) {
    const rejected = reduceNativeAuth(initial, {
      kind: "reply", navigation: 7, request: 1, reply: { ...reply, ...patch },
    });
    assert.equal(rejected.presentation, undefined);
    assert.equal(rejected.operation, undefined);
  }
  for (const [navigation, request] of [[8, 1], [7, 2]]) {
    const rejected = reduceNativeAuth(initial, { kind: "reply", navigation, request, reply });
    assert.equal(rejected.presentation, undefined);
    assert.equal(rejected.operation, undefined);
  }
  const accepted = presented(reply);
  assert.deepEqual(accepted.presentation, reply);
  assert.equal(accepted.operation.operation_id, reply.operation_id);
  assert.equal(accepted.operation.database_instance, reply.database_instance);
  assert.equal(accepted.operation.engine_epoch, reply.engine_epoch);
  assert.doesNotMatch(JSON.stringify(accepted.operation), /SYNTHETIC_STATE|auth_url|challenge|account/);
  const read = reduceNativeAuth(accepted, { kind: "begin", action: "read", request: 2 });
  assert.equal(read.presentation, undefined);
  for (const patch of [
    { operation_id: "other-operation" }, { database_instance: "d".repeat(64) },
    { engine_epoch: "e".repeat(64) }, { generation: "12" },
  ]) {
    const rejected = reduceNativeAuth(read, {
      kind: "reply", navigation: 7, request: 2,
      reply: { ...reply, request_id: "synthetic-owner-read", ...patch },
    });
    assert.equal(rejected.presentation, undefined);
    assert.equal(rejected.operation.operation_id, reply.operation_id);
  }
  const replacement = { ...reply, request_id: "synthetic-owner-read", challenge: null };
  const settled = reduceNativeAuth(read, { kind: "reply", navigation: 7, request: 2, reply: replacement });
  assert.deepEqual(settled.presentation, replacement);
  assert.doesNotMatch(markup(settled), /SYNTHETIC_STATE/);
});

test("synthetic close and owner invalidation drop presentation without inventing cancellation", () => {
  const original = presented();
  const closed = reduceNativeAuth(original, { kind: "close" });
  assert.equal(closed.open, false);
  assert.equal(closed.presentation, undefined);
  assert.deepEqual(closed.operation, original.operation);
  assert.equal(markup(closed), "");
  assert.doesNotMatch(JSON.stringify(closed), /SYNTHETIC_STATE|auth_url|user_code/);
  const pending = reduceNativeAuth(original, { kind: "begin", action: "read", request: 2 });
  const hidden = reduceNativeAuth(pending, { kind: "close" });
  const late = reduceNativeAuth(hidden, {
    kind: "reply", navigation: 7, request: 2,
    reply: syntheticReply({ request_id: "synthetic-owner-read" }),
  });
  assert.equal(late.open, false);
  assert.equal(late.presentation, undefined);
  assert.equal(late.operation.operation_id, original.operation.operation_id);
  assert.doesNotMatch(JSON.stringify(late), /SYNTHETIC_STATE|auth_url|user_code/);
  const invalid = reduceNativeAuth(original, { kind: "invalidate" });
  assert.equal(invalid.presentation, undefined);
  assert.deepEqual(invalid.operation, original.operation);
  assert.equal(reduceNativeAuth(invalid, { kind: "begin", action: "read", request: 3 }).in_flight, undefined);
  assert.doesNotMatch(JSON.stringify(invalid), /SYNTHETIC_STATE|auth_url|user_code/);
});

test("synthetic browser and device challenges render exact safe text only in their panel", () => {
  const browser = markup(presented());
  assert.match(browser, /Waiting for you to finish signing in\.\.\./);
  assert.match(browser, /<p class="mono small sign-in-url">https:\/\/auth\.example\.invalid\/login\?state=SYNTHETIC_STATE&amp;client=codex<\/p>/);
  assert.doesNotMatch(browser, /awaiting_owner|operation|generation|epoch/i);
  assert.doesNotMatch(browser, /href=|<input\b|<textarea\b|<iframe\b|<script\b|login_id/);
  const device = syntheticReply({
    action: { kind: "sign_in", method: "codex_device_code" },
    challenge: { kind: "codex_device_code", user_code: "SYNTHETIC-7Q2", verification_url: "https://auth.example.invalid/device" },
  });
  const deviceMarkup = markup(presented(device, syntheticDetail(), "codex_device_code"));
  assert.match(deviceMarkup, /SYNTHETIC-7Q2/);
  assert.match(deviceMarkup, /https:\/\/auth\.example\.invalid\/device/);
  assert.doesNotMatch(deviceMarkup, /SYNTHETIC_STATE|login_id|href=|<input\b/);
  for (const challenge of [
    { ...device.challenge, user_code: "SYNTHETIC\u001b[31m_CODE" },
    { ...device.challenge, user_code: "SYNTHETIC\u202e_CODE" },
    { ...device.challenge, verification_url: "https://auth.example.invalid/\u2066CODE" },
  ]) {
    const unsafe = markup(presented({ ...device, challenge }, syntheticDetail(), "codex_device_code"));
    assert.doesNotMatch(unsafe, /[\u001b\u202e\u2066]|SYNTHETIC.*_CODE|auth\.example\.invalid\/.*CODE/u);
    assert.match(unsafe, /can&#x27;t show safely/i);
  }
});

test("synthetic account observations preserve distinct kinds and bounded escaped fields", () => {
  for (const [account, expected] of [
    [{ kind: "codex_api_key" }, /Signed in with an OpenAI API key\./],
    [{ kind: "codex_chatgpt", email: "SYNTHETIC_EMAIL@example.invalid", plan_type: "plus" }, /Signed in as SYNTHETIC_EMAIL@example\.invalid \(ChatGPT Plus\)\./],
    [{ kind: "codex_amazon_bedrock", uses_codex_managed_credentials: false }, /Signed in with Amazon Bedrock\./],
  ]) {
    const reply = syntheticReply({ phase: "authenticated_observed", challenge: null, account });
    const rendered = markup(presented(reply));
    assert.match(rendered, expected);
    assert.doesNotMatch(rendered, /authenticated_observed|data-testid="(?:model-ready|native-ready)"|<script\b/);
  }
  const claude = syntheticDetail();
  claude.record.connection.harness = "claude";
  const account = {
    kind: "claude", logged_in: true, auth_method: "future_method", api_provider: "synthetic-provider",
    config_directory: "/SYNTHETIC_PROFILE", email: { state: "value", text: "<script>synthetic</script>\u001b\n\u202e" }, org_id: { state: "null" },
    org_name: { state: "missing" },
    subscription_type: { state: "value", text: "synthetic-plan" },
  };
  const reply = syntheticReply({
    action: { kind: "sign_in", method: "claude_official" },
    phase: "authenticated_observed", challenge: null, account,
  });
  const rendered = markup(presented(reply, claude, "claude_official"));
  assert.match(rendered, /\(Claude Synthetic-plan\)\./);
  assert.match(rendered, /Signed in as &lt;script&gt;synthetic&lt;\/script&gt;/);
  assert.doesNotMatch(rendered, /future_method|synthetic-provider|SYNTHETIC_PROFILE/);
  assert.doesNotMatch(rendered, /[\u001b\u202e]|<script\b/u);
  const oversized = { ...reply, account: { ...account, config_directory: "é".repeat(65536) } };
  const bounded = presented(oversized, claude, "claude_official");
  assert.equal(bounded.presentation, undefined);
  assert.ok(bounded.error);
  assert.ok(Buffer.byteLength(markup(bounded)) <= 65536);
});

test("the sign-in panel offers no read or cancel without an operation, and the reducer blocks unconfirmed logout", () => {
  const initial = opened();
  for (const action of ["read", "cancel", "logout"]) {
    const blocked = reduceNativeAuth(initial, { kind: "begin", action, request: 1 });
    assert.equal(blocked.in_flight, undefined);
    assert.equal(blocked.operation, undefined);
  }
  const idle = markup(initial);
  assert.doesNotMatch(idle, /Cancel sign-in|Check now|Sign out|logout/i);
  const confirmation = reduceNativeAuth(initial, { kind: "confirm_logout" });
  assert.equal(confirmation.confirming_logout, true);
  assert.equal(confirmation.in_flight, undefined);
  assert.deepEqual(
    reduceNativeAuth(confirmation, { kind: "begin", action: "logout", request: 1 }).in_flight,
    { action: "logout", request: 1 },
  );
  const displayed = markup(presented());
  assert.match(displayed, />Cancel sign-in<\/button>/);
  assert.match(displayed, />Check now<\/button>/);
  const unavailable = presented(syntheticReply({
    phase: "unavailable", code: "native_auth_host_unavailable", challenge: null,
  }));
  assert.equal(unavailable.presentation.phase, "unavailable");
  assert.match(markup(unavailable), /Codex isn&#x27;t installed on this Mac, so ChatGPT sign-in isn&#x27;t available\./);
  assert.doesNotMatch(markup(unavailable), /native_auth_host_unavailable/);
  assert.doesNotMatch(markup(unavailable), /SYNTHETIC_STATE|SYNTHETIC_EMAIL/);
});
