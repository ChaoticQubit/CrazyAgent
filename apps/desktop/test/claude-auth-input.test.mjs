import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { transformSync } from "rolldown/utils";

const sourceRoot = new URL("../src/", import.meta.url).href;
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(".") && context.parentURL?.startsWith(sourceRoot)) {
      for (const extension of [".ts", ".tsx"]) {
        const candidate = new URL(specifier + extension, context.parentURL);
        if (existsSync(candidate)) return nextResolve(candidate.href, context);
      }
    }
    return nextResolve(specifier, context);
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(sourceRoot) || !/\.tsx?$/.test(url)) return nextLoad(url, context);
    const transformed = transformSync(fileURLToPath(url), readFileSync(new URL(url), "utf8"), {
      target: "es2022", sourceType: "module", jsx: { runtime: "automatic" },
    });
    assert.deepEqual(transformed.errors, [], "production modules must load before behavior assertions");
    return { format: "module", source: transformed.code, shortCircuit: true };
  },
});

const auth = await import("../src/native-auth.ts");
const { NativeAuthPanel } = await import("../src/screens/native-auth.tsx");
const unitUrl = "https://claude.com/cai/oauth/authorize?client_id=SYNTHETIC_UNIT_CLIENT&state=SYNTHETIC_UNIT_OAUTH_STATE";

function parserState() {
  const operation = {
    operation_id: "unit-operation", database_instance: "unit-database", engine_epoch: "unit-epoch",
    connection: "unit-claude-connection", config_id: "unit-config", source_hash: "unit-source",
    generation: "7", action: { kind: "sign_in", method: "claude_official" }, phase: "awaiting_owner", code: null,
  };
  return {
    open: true, valid: true, confirming_logout: false,
    context: { ticket: "17", connection: operation.connection, config_id: operation.config_id,
      source_hash: operation.source_hash, navigation: 3, harness: "claude" },
    operation,
    presentation: { ...operation, ticket: "17", request_id: "unit-parser-observation",
      challenge: { kind: "claude_official", auth_url: unitUrl, accepts_code: true, prompt_id: "1" }, account: null },
  };
}

function panel(state, blocked = false) {
  return renderToStaticMarkup(createElement(NativeAuthPanel, { state, blocked }));
}

test("Claude parser rejects nonofficial destinations rather than treating an unknown challenge as safe", () => {
  const challenge = parserState().presentation.challenge;
  assert.equal(auth.safeChallenge({ ...challenge, auth_url: "https://example.invalid/oauth/authorize?state=SYNTHETIC_UNIT_STATE" }), false,
    "the real production challenge validator must fail closed on a nonofficial Claude URL");
  for (const auth_url of [
    "http://claude.com/cai/oauth/authorize?state=x", "https://claude.com.evil.invalid/cai/oauth/authorize?state=x",
    "https://claude.com@evil.invalid/cai/oauth/authorize?state=x", "https://claude.com/cai/oauth/authorize?",
    "https://console.anthropic.com/oauth/authorize?state=x", unitUrl + "#fragment", unitUrl + "\\suffix",
    unitUrl + "\n", unitUrl + "\u202e", "https://claude.com/cai/oauth/authorize?state=" + "x".repeat(8192),
  ]) assert.equal(auth.safeChallenge({ ...challenge, auth_url }), false);
  assert.equal(auth.safeChallenge(challenge), true);
  assert.equal(auth.safeChallenge({ ...challenge, accepts_code: false, prompt_id: null }), true);
  assert.equal(auth.safeChallenge({ ...challenge, accepts_code: false, prompt_id: "1" }), false);
  assert.equal(auth.safeChallenge({ ...challenge, accepts_code: true, prompt_id: null }), false);
});

test("fallback validation accepts only a bounded complete synthetic code and state without echoing it", () => {
  assert.equal(typeof auth.validateClaudeAuthCode, "function", "production must validate the complete fallback input separately from persisted state");
  assert.equal(auth.validateClaudeAuthCode("SYNTHETIC_COMPLETION#SYNTHETIC_FALLBACK_STATE"), undefined);
  assert.equal(auth.validateClaudeAuthCode("x".repeat(8190) + "#s"), undefined);
  for (const value of [null, "", "code", "#state", "code#", "code#state#extra", "code#state\n", "code#state\r",
    "code#sta te", "code#\u0000state", "code#\u202estate", "code#界", "x".repeat(8191) + "#s"]) {
    assert.throws(() => auth.validateClaudeAuthCode(value), (error) => {
      assert.equal(error.message, "native_auth_claude_input_invalid");
      assert.doesNotMatch(error.message, /SYNTHETIC_COMPLETION|SYNTHETIC_FALLBACK_STATE/);
      return true;
    });
  }
});

test("parser-state gating requires the observed prompt and exact original account operation generation", () => {
  assert.equal(typeof auth.nativeClaudeInputAllowed, "function", "production must gate private input independently of sign-in/read commands");
  const state = parserState();
  assert.equal(auth.nativeClaudeInputAllowed(state, false), true);
  assert.equal(auth.nativeClaudeInputAllowed(state, true), false);
  assert.equal(auth.nativeClaudeInputAllowed(undefined, false), false);
  for (const patch of [
    { open: false }, { valid: false }, { invalidated: true }, { rebound: true }, { recover: true },
    { in_flight: { action: "read", request: 4 } }, { presentation: undefined }, { operation: undefined },
  ]) assert.equal(auth.nativeClaudeInputAllowed({ ...state, ...patch }, false), false);
  for (const field of ["operation_id", "database_instance", "engine_epoch", "connection", "config_id", "source_hash", "generation", "ticket"]) {
    const changed = structuredClone(state);
    changed.presentation[field] = "different";
    assert.equal(auth.nativeClaudeInputAllowed(changed, false), false, `mismatched ${field} must disable private input`);
  }
  for (const field of ["connection", "config_id", "source_hash"]) {
    assert.equal(auth.nativeClaudeInputAllowed({ ...state, context: { ...state.context, [field]: "different" } }, false), false);
  }
  for (const challenge of [
    { ...state.presentation.challenge, accepts_code: false, prompt_id: null },
    { ...state.presentation.challenge, accepts_code: true, prompt_id: "2" },
    { kind: "codex_browser", auth_url: "https://auth.example.invalid/login" },
  ]) assert.equal(auth.nativeClaudeInputAllowed({ ...state, presentation: { ...state.presentation, challenge } }, false), false);
  assert.equal(auth.nativeClaudeInputAllowed({ ...state, context: { ...state.context, harness: "codex" } }, false), false);
  assert.equal(auth.nativeClaudeInputAllowed({ ...state, presentation: { ...state.presentation, phase: "checking_account" } }, false), false);
  assert.equal(auth.nativeClaudeInputAllowed({ ...state, operation: { ...state.operation, generation: "0" } }, false), false);
});

test("the real reducer records one metadata-only private attempt and never converts its acknowledgement into authentication", () => {
  const initial = parserState();
  const sending = auth.reduceNativeAuth(initial, { kind: "claude_input_begin", request: 21 });
  assert.ok(sending.claude_input, "production reducer must retain the one-attempt marker before private delivery");
  assert.equal(sending.claude_input.operation_id, initial.operation.operation_id);
  assert.equal(sending.claude_input.generation, "7");
  assert.equal(sending.claude_input.prompt_id, "1");
  assert.equal(sending.claude_input.request, 21);
  assert.equal(sending.claude_input.status, "sending");
  assert.equal(sending.presentation, undefined);
  assert.deepEqual(sending.operation, initial.operation);
  assert.doesNotMatch(JSON.stringify(sending), /SYNTHETIC_COMPLETION|SYNTHETIC_FALLBACK_STATE|auth_url|challenge|secret/);
  assert.deepEqual(auth.reduceNativeAuth(sending, { kind: "claude_input_begin", request: 21 }), sending);
  assert.deepEqual(auth.reduceNativeAuth(sending, { kind: "claude_input_begin", request: 22 }), sending);
  const accepted = auth.reduceNativeAuth(sending, { kind: "claude_input_accepted", navigation: 3, request: 21 });
  assert.equal(accepted.claude_input.status, "accepted");
  assert.deepEqual(accepted.operation, initial.operation);
  assert.equal(accepted.operation.phase, "awaiting_owner");
  assert.equal(accepted.presentation, undefined);
  assert.equal(auth.nativeClaudeInputAllowed({ ...accepted, presentation: initial.presentation }, false), false);
});

test("close disconnect and stale completion preserve spent metadata while clearing transient presentation", () => {
  const initial = parserState();
  const sending = auth.reduceNativeAuth(initial, { kind: "claude_input_begin", request: 21 });
  assert.ok(sending.claude_input, "private attempt must exist before close/disconnect can be tested");
  for (const event of [{ kind: "close" }, { kind: "invalidate" }]) {
    const hidden = auth.reduceNativeAuth(sending, event);
    assert.equal(hidden.presentation, undefined);
    assert.deepEqual(hidden.operation, initial.operation);
    assert.deepEqual(hidden.claude_input, sending.claude_input);
    assert.equal(auth.nativeClaudeInputAllowed(hidden, false), false);
    assert.deepEqual(auth.reduceNativeAuth(hidden, { kind: "claude_input_accepted", navigation: 4, request: 21 }), hidden);
    assert.deepEqual(auth.reduceNativeAuth(hidden, { kind: "claude_input_accepted", navigation: 3, request: 22 }), hidden);
    const uncertain = auth.reduceNativeAuth(hidden, { kind: "claude_input_failure", navigation: 3, request: 21 });
    assert.equal(uncertain.claude_input.status, "uncertain");
    assert.equal(uncertain.presentation, undefined);
    assert.deepEqual(uncertain.operation, initial.operation);
    assert.equal(uncertain.open, hidden.open);
    assert.equal(uncertain.valid, hidden.valid);
    const samePromptAgain = { ...uncertain, open: true, valid: true, invalidated: false, presentation: initial.presentation };
    assert.equal(auth.nativeClaudeInputAllowed(samePromptAgain, false), false);
    assert.deepEqual(auth.reduceNativeAuth(samePromptAgain, { kind: "claude_input_begin", request: 23 }), samePromptAgain);
  }
});

test("real panel rendering uses the complete official URL and a separate masked uncontrolled fallback field only for the prompt", () => {
  const state = parserState();
  let markup;
  assert.doesNotThrow(() => { markup = panel(state); }, "actual production panel must render the typed Claude challenge safely");
  assert.ok(markup.includes(unitUrl.replaceAll("&", "&amp;")), "the complete supplied official URL must survive rendering without replacement or truncation");
  const input = markup.match(/<input\b[^>]*data-testid="native-auth-claude-code"[^>]*>/)?.[0];
  assert.ok(input, "observed fallback prompt must expose its separate input");
  assert.match(input, /type="password"/);
  assert.match(input, /maxLength="8192"/i);
  assert.match(input, /autoComplete="off"/i);
  assert.doesNotMatch(input, /\b(?:name|value)=/);
  assert.match(markup, /Send code once/);
  const noPrompt = structuredClone(state);
  noPrompt.presentation.challenge.accepts_code = false;
  noPrompt.presentation.challenge.prompt_id = null;
  const waiting = panel(noPrompt);
  assert.ok(waiting.includes(unitUrl.replaceAll("&", "&amp;")));
  assert.doesNotMatch(waiting, /native-auth-claude-code/);
  assert.doesNotMatch(panel(auth.reduceNativeAuth(state, { kind: "close" })), /SYNTHETIC_UNIT_OAUTH_STATE|native-auth-claude-code/);
  assert.doesNotMatch(panel(auth.reduceNativeAuth(state, { kind: "invalidate" })), /SYNTHETIC_UNIT_OAUTH_STATE|native-auth-claude-code/);
  assert.ok(Buffer.byteLength(markup) < 65536);
});

test("Codex parser and command behavior remain separate from Claude fallback input", () => {
  const state = parserState();
  state.context.harness = "codex";
  state.operation.action = { kind: "sign_in", method: "codex_browser" };
  state.presentation.action = state.operation.action;
  state.presentation.challenge = { kind: "codex_browser", auth_url: "https://auth.example.invalid/login?state=SYNTHETIC_CODEX_STATE" };
  assert.equal(auth.safeChallenge(state.presentation.challenge), true);
  assert.equal(auth.safeChallenge({ kind: "codex_device_code", user_code: "UNIT-CODE", verification_url: "https://auth.example.invalid/device" }), true);
  assert.equal(auth.safeChallenge({ kind: "codex_browser", auth_url: "https://auth.example.invalid/\nlogin" }), false);
  assert.equal(typeof auth.nativeClaudeInputAllowed, "function");
  assert.equal(auth.nativeClaudeInputAllowed(state, false), false);
  assert.deepEqual(auth.reduceNativeAuth(state, { kind: "claude_input_begin", request: 21 }), state);
  assert.match(panel(state), /SYNTHETIC_CODEX_STATE/);
  assert.doesNotMatch(panel(state), /native-auth-claude-code/);
});

