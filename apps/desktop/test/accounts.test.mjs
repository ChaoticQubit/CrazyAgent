import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const auth = await import("../src/native-auth.ts");
const { SignInPanel, signInStage, signInFailure, signedInLine, signInRunning } = await import("../src/screens/settings/sign-in.tsx");
const { methodOptions, providerTiles, defaultAccountName, urlError, nameError, addedLine, TILE_LINES, ProviderTiles, MethodMenu, KeyOrUrlForm } =
  await import("../src/screens/settings/add-account.tsx");
const { SessionContext, useSession } = await import("../src/state.ts");
const { poolEffect, supportDetails } = await import("../src/screens/settings/account-drawer.tsx");
const { AccountsView, filterAccounts, accountCount } = await import("../src/screens/settings/accounts.tsx");

const field = (url, key, model) => ({ url, key, model });
const catalog = [
  { provider: "chatgpt", name: "ChatGPT / Codex", methods: [
    { method: "subscription", label: "Sign in with ChatGPT", url_preset: null, ...field("hidden", "hidden", "hidden") },
    { method: "api_key", label: "Use an API key", url_preset: null, ...field("hidden", "required", "optional") }] },
  { provider: "claude", name: "Claude", methods: [
    { method: "subscription", label: "Sign in with your Claude subscription", url_preset: null, ...field("hidden", "hidden", "hidden") },
    { method: "api_key", label: "Use an API key", url_preset: null, ...field("hidden", "required", "optional") }] },
  { provider: "openrouter", name: "OpenRouter", methods: [{ method: "api_key", label: "Use an API key", url_preset: null, ...field("hidden", "required", "required") }] },
  { provider: "local", name: "Local models (Ollama)", methods: [{ method: "url", label: "Connect a URL", url_preset: "http://127.0.0.1:11434/v1", ...field("optional", "hidden", "required") }] },
];
const account = (patch) => ({ id: "chatgpt-account-1", name: "ChatGPT 1", provider: "chatgpt", provider_name: "ChatGPT / Codex", method: "subscription",
  method_label: "Sign in with ChatGPT", url: null, model: null, status: "signed_in", status_label: "Signed in", ...patch });

function detail(harness = "codex") {
  return { ticket: "17", collection: "connections", revision: "23", config_id: "unit-config", source_hash: "a".repeat(64), writable: true, fresh: true,
    record: { kind: "connection", connection: { id: "chatgpt-account-1", provider: "openai", harness, protocol: "native", account_ref: "unit-account",
      endpoint: { kind: "native" }, model: { id: "unit-model", author: "openai" }, auth: { method: "native_managed", profile_ref: "unit-profile" } } } };
}

function reply(patch = {}) {
  return { ticket: "17", request_id: "unit-op", operation_id: "unit-op", database_instance: "b".repeat(64), engine_epoch: "c".repeat(64),
    connection: "chatgpt-account-1", config_id: "unit-config", source_hash: "a".repeat(64), generation: "11",
    action: { kind: "sign_in", method: "codex_device_code" }, phase: "awaiting_owner", code: null,
    challenge: { kind: "codex_device_code", user_code: "QX4F-9KTB", verification_url: "https://auth.example.invalid/codex/device" }, account: null, ...patch };
}

const opened = (harness) => auth.reduceNativeAuth(undefined, { kind: "open", detail: detail(harness), navigation: 7 });
const begun = (action = "codex_device_code", harness = "codex") => auth.reduceNativeAuth(opened(harness), { kind: "begin", action, request: 1 });
const answered = (patch, action, harness) => auth.reduceNativeAuth(begun(action, harness), { kind: "reply", navigation: 7, request: 1, reply: reply(patch) });

const forbidden = () => { throw new Error("static rendering must not start, cancel or send anything"); };
const panel = (state, props = {}) => renderToStaticMarkup(createElement(SignInPanel, {
  provider: "chatgpt", method: "codex_device_code", state, onStart: forbidden, onCancel: forbidden, onCheck: forbidden, onClaudeCode: forbidden, ...props }));

test("step 2 lists catalog methods in order with Recommended first, and ChatGPT offers the Codex sign-in on this Mac", () => {
  const chatgpt = methodOptions(catalog[0]);
  assert.deepEqual(chatgpt.map((option) => [option.title, option.recommended, option.native ?? null]), [
    ["Sign in with ChatGPT", true, "codex_device_code"], ["Use the Codex sign-in on this Mac", false, "codex_existing"], ["Use an API key", false, null]]);
  assert.equal(chatgpt[1].detail, "Reuses the account Codex is already signed in to.");
  assert.deepEqual(methodOptions(catalog[1]).map((option) => option.native ?? null), ["claude_official", null]);
  const single = methodOptions(catalog[2]);
  assert.equal(single.length, 1);
  assert.equal(single[0].recommended, false);
  for (const tile of ["openai", "anthropic", "openrouter", "openai_compatible", "local"]) assert.ok(TILE_LINES[tile]);
  assert.match(TILE_LINES.openai_compatible, /OpenAI API/);
  assert.match(TILE_LINES.local, /Ollama/);
  const claudeKey = methodOptions(catalog[1]).find((option) => option.entry.method === "api_key");
  assert.deepEqual([claudeKey.recommended, claudeKey.native], [false, undefined]);
});

const keyed = (url) => ({ method: "api_key", label: "Use an API key", url_preset: url, ...field("hidden", "required", "required") });
const engineCatalog = [
  { provider: "chatgpt", name: "ChatGPT / Codex", methods: [
    { method: "subscription", label: "Sign in with ChatGPT", url_preset: null, ...field("hidden", "hidden", "hidden") }, keyed("https://api.openai.com/v1")] },
  { provider: "claude", name: "Claude", methods: [
    { method: "subscription", label: "Sign in with your Claude subscription", url_preset: null, ...field("hidden", "hidden", "hidden") }, keyed("https://api.anthropic.com/v1")] },
  { provider: "openrouter", name: "OpenRouter", methods: [keyed("https://openrouter.ai/api/v1")] },
  { provider: "openai_api", name: "OpenAI API", methods: [keyed("https://api.openai.com/v1")] },
  { provider: "anthropic_api", name: "Anthropic API", methods: [keyed("https://api.anthropic.com/v1")] },
  { provider: "openai_compatible", name: "Any OpenAI-compatible URL", methods: [
    { method: "url", label: "Connect a URL", url_preset: null, ...field("required", "optional", "required") }] },
  { provider: "local", name: "Local models (Ollama)", methods: [
    { method: "url", label: "Connect a URL", url_preset: "http://127.0.0.1:11434/v1", ...field("required", "hidden", "required") }] },
];

test("Add account offers one OpenAI and one Anthropic entry, and each key method adds the company's API account", () => {
  const markup = renderToStaticMarkup(createElement(ProviderTiles, { catalog: engineCatalog, onPick: forbidden }));
  for (const gone of ["ChatGPT / Codex", "Claude", "OpenAI API", "Anthropic API"]) assert.ok(!markup.includes(`<span class="title">${gone}</span>`), gone);
  assert.equal(markup.match(/<span class="title">/g).length, 5);
  const tiles = providerTiles(engineCatalog);
  assert.deepEqual(tiles.map((tile) => tile.name), ["OpenAI", "Anthropic", "OpenRouter", "Any OpenAI-compatible URL", "Local models (Ollama)"]);
  for (const tile of tiles) assert.ok(markup.includes(`<span class="title">${tile.name}</span><span class="detail">${tile.line}</span>`), tile.name);
  const ways = (name) => tiles.find((tile) => tile.name === name).options
    .map((option) => [option.title, option.recommended, option.provider.provider, option.entry.method, option.native ?? null]);
  assert.deepEqual(ways("OpenAI"), [
    ["Sign in with ChatGPT", true, "chatgpt", "subscription", "codex_device_code"],
    ["Use the Codex sign-in on this Mac", false, "chatgpt", "subscription", "codex_existing"],
    ["Use an API key", false, "openai_api", "api_key", null]]);
  assert.deepEqual(ways("Anthropic"), [
    ["Sign in with your Claude subscription", true, "claude", "subscription", "claude_official"],
    ["Use an API key", false, "anthropic_api", "api_key", null]]);
  assert.deepEqual(ways("OpenRouter"), [["Use an API key", false, "openrouter", "api_key", null]]);
  assert.equal(tiles[0].line, "Sign in with ChatGPT or use an OpenAI API key.");
  assert.equal(tiles[1].line, "Sign in with Claude or use an Anthropic API key.");
  const anthropic = renderToStaticMarkup(createElement(MethodMenu, { tile: tiles[1], onPick: forbidden }));
  assert.match(anthropic, /Sign in with your Claude subscription<span class="badge tone-info">Recommended<\/span>/);
  assert.match(anthropic, /Pay per use with an Anthropic key\./);
  assert.equal(defaultAccountName(tiles[1].options[1].provider, []), "Anthropic API 1");
  assert.equal(defaultAccountName(tiles[0].options[0].provider, []), "ChatGPT 1");
  const older = providerTiles(catalog).find((tile) => tile.name === "Anthropic").options.map((option) => option.key);
  assert.deepEqual(older, ["claude:subscription", "claude:api_key"], "without a separate API provider the subscription provider's key stays");
});

test("a catalog provider with no way to add an account trips the tile check", () => {
  assert.throws(() => providerTiles([{ provider: "openrouter", name: "OpenRouter", methods: [] }]), /every Add account tile offers a way to add an account/);
});

test("default names take the lowest free number; form checks match the engine's rules", () => {
  assert.equal(defaultAccountName(catalog[0], []), "ChatGPT 1");
  assert.equal(defaultAccountName(catalog[0], [account({}), account({ id: "x", name: "ChatGPT 3" })]), "ChatGPT 2");
  assert.equal(defaultAccountName(catalog[0], [account({ name: "ChatGPT 1" }), account({ id: "y", name: "ChatGPT 2" })]), "ChatGPT 3");
  assert.equal(defaultAccountName(catalog[3], []), "Local models 1");
  assert.equal(urlError("https://api.together.xyz/v1"), undefined);
  assert.equal(urlError("http://127.0.0.1:11434/v1"), undefined);
  assert.equal(urlError("http://localhost:8080"), undefined);
  for (const bad of ["http://example.com/v1", "api.together.xyz", "", "https://user:pass@example.com", "ftp://127.0.0.1"]) {
    assert.equal(urlError(bad), "Enter a full URL that starts with https://, or http://127.0.0.1 for this Mac.", bad);
  }
  assert.equal(nameError("Work"), undefined);
  for (const bad of ["", "   ", "a".repeat(61), "two\nlines"]) assert.ok(nameError(bad), JSON.stringify(bad));
});

test("Account added lines follow the method and the real check result", () => {
  const key = catalog[2].methods[0];
  const url = catalog[3].methods[0];
  assert.equal(addedLine(key, account({})), "Key saved. CrazyAgent will check it the first time an agent uses it.");
  assert.equal(addedLine(catalog[0].methods[0], account({})), "Signed in.");
  assert.equal(addedLine(url, account({}), { kind: "ok", value: { models: ["a", "b", "c"] } }), "Connected. 3 models found.");
  assert.equal(addedLine(url, account({}), { kind: "ok", value: { models: ["a"] } }), "Connected. 1 model found.");
  assert.equal(addedLine(url, account({}), { kind: "error", code: "account_url_unreachable", message: "Can't reach this URL." }), "Saved. Can't reach this URL.");
  assert.equal(addedLine(url, account({})), "Saved. Checking the connection...");
});

test("sign-in stages come from real reducer states, never from a reply the reducer refused", () => {
  assert.deepEqual(signInStage(undefined, "chatgpt"), { kind: "loading" });
  assert.deepEqual(signInStage(opened("codex"), "chatgpt"), { kind: "start" });
  assert.deepEqual(signInStage(begun(), "chatgpt"), { kind: "working", text: "Getting a sign-in code..." });
  assert.equal(signInRunning(begun()), true);
  const waiting = answered({});
  const stage = signInStage(waiting, "chatgpt");
  assert.equal(stage.kind, "waiting");
  assert.equal(stage.challenge.user_code, "QX4F-9KTB");
  assert.equal(stage.claudeInput, false);
  assert.equal(signInRunning(waiting), true);
  const signedIn = answered({ phase: "authenticated_observed", challenge: null, account: { kind: "codex_chatgpt", email: "m.okafor@fernlabs.dev", plan_type: "plus" } });
  assert.deepEqual(signInStage(signedIn, "chatgpt"), { kind: "signed_in", line: "Signed in as m.okafor@fernlabs.dev (ChatGPT Plus)." });
  assert.equal(signInRunning(signedIn), false);
  const mismatched = answered({ connection: "another-account" });
  assert.equal(mismatched.presentation, undefined);
  assert.deepEqual(signInStage(mismatched, "chatgpt"), { kind: "failed", text: "We couldn't confirm the sign-in. Check again.", hint: "check" });
  const missing = auth.reduceNativeAuth(begun(), { kind: "failure", navigation: 7, request: 1, code: "native_auth_host_unavailable" });
  assert.equal(signInStage(missing, "chatgpt").hint, "missing");
  assert.match(signInStage(missing, "chatgpt").text, /^Codex isn't installed on this Mac, so ChatGPT sign-in isn't available\./);
  assert.match(signInStage(missing, "claude").text, /^Claude Code isn't installed on this Mac/);
  const expired = answered({ phase: "unauthenticated_observed", challenge: null, code: "native_auth_sign_in_expired" });
  assert.deepEqual(signInStage(expired, "chatgpt"), { kind: "failed", text: "This code expired.", hint: "expired" });
  assert.deepEqual(signInStage(answered({ phase: "unavailable", challenge: null, code: null }), "claude").hint, "missing");
  assert.deepEqual(signInFailure("native_auth_some_new_code", "chatgpt"), { text: "Sign-in didn't finish. Try again. (native_auth_some_new_code)", hint: "retry" });
  const closing = "Another sign-in is still running or closing. Finish or cancel it, or wait a minute, then try again. If this keeps happening, restart CrazyAgent.";
  assert.deepEqual(signInStage(answered({ phase: "uncertain", challenge: null, code: "native_auth_recovery_required" }), "claude"),
    { kind: "failed", text: closing, hint: "check" });
  assert.deepEqual(signInFailure("native_auth_recovery_required", "claude"), { text: closing, hint: "retry" });
  assert.deepEqual(signInStage(answered({ phase: "cancelled", challenge: null, code: null }), "claude"),
    { kind: "failed", text: "Sign-in cancelled.", hint: "retry" });
  assert.deepEqual(signInStage(answered({ phase: "failed", challenge: null, code: "native_auth_interrupted" }), "claude"),
    { kind: "failed", text: "Sign-in didn't finish. Try again.", hint: "retry" });
});

test("signed-in lines show only observed fields, escaped, and never invent an email or plan", () => {
  assert.equal(signedInLine(null), "Signed in.");
  assert.equal(signedInLine({ kind: "codex_chatgpt", email: null, plan_type: "pro" }), "Signed in (ChatGPT Pro).");
  assert.equal(signedInLine({ kind: "codex_chatgpt", email: "a\u0007b@x.dev", plan_type: "" }), "Signed in as a\\u0007b@x.dev.");
  const claude = (email, plan) => ({ kind: "claude", logged_in: true, auth_method: "claude.ai", api_provider: "firstParty", config_directory: "/unit",
    email, org_id: { state: "null" }, org_name: { state: "missing" }, subscription_type: plan });
  assert.equal(signedInLine(claude({ state: "value", text: "mira@fernlabs.dev" }, { state: "value", text: "max" })), "Signed in as mira@fernlabs.dev (Claude Max).");
  assert.equal(signedInLine(claude({ state: "missing" }, { state: "null" })), "Signed in.");
  assert.doesNotMatch(signedInLine(claude({ state: "value", text: "x@y.dev" }, { state: "missing" })), /\/unit|firstParty|claude\.ai/);
});

test("the device-code panel shows the code large with Copy code, the page as selectable text, and Cancel sign-in", () => {
  const markup = panel(answered({}));
  assert.match(markup, /<div class="device-code" aria-label="Sign-in code">QX4F-9KTB<\/div>/);
  assert.match(markup, />Copy code<\/button>/);
  assert.match(markup, /<p class="mono small sign-in-url">https:\/\/auth\.example\.invalid\/codex\/device<\/p>/);
  assert.match(markup, /Waiting for you to finish signing in\.\.\./);
  assert.match(markup, />Cancel sign-in<\/button>/);
  assert.doesNotMatch(markup, /type="password"/);
  assert.ok(Buffer.byteLength(markup) < 65536);
});

test("the start panel offers each managed method with the design copy", () => {
  const idle = panel(undefined, { idle: true });
  assert.match(idle, />Get sign-in code<\/button>/);
  assert.match(idle, />Sign in in this browser instead<\/button>/);
  const existing = panel(undefined, { idle: true, method: "codex_existing" });
  assert.match(existing, /CrazyAgent copies this sign-in into its own private folder for this account\. Codex itself is not changed\./);
  assert.match(existing, />Use this sign-in<\/button>/);
  assert.match(panel(undefined, { idle: true, provider: "claude", method: "claude_official" }), />Open Claude sign-in<\/button>/);
  assert.match(panel(undefined), /Getting ready to sign in\.\.\./);
  assert.match(panel(begun()), /Getting a sign-in code\.\.\./);
});

const claudeUrl = "https://claude.com/cai/oauth/authorize?client_id=SYNTHETIC_UNIT_CLIENT&state=SYNTHETIC_UNIT_OAUTH_STATE";
const claudeWaiting = (accepts = true) => answered({ action: { kind: "sign_in", method: "claude_official" }, challenge: { kind: "claude_official", auth_url: claudeUrl,
  accepts_code: accepts, prompt_id: accepts ? "1" : null } }, "claude_official", "claude");

test("the Claude panel shows the complete official URL and a masked, uncontrolled code field only while Claude asks for it", () => {
  const props = { provider: "claude", method: "claude_official" };
  const markup = panel(claudeWaiting(), props);
  assert.ok(markup.includes(claudeUrl.replaceAll("&", "&amp;")), "the whole official URL survives rendering");
  const input = markup.match(/<input\b[^>]*id="claude-sign-in-code"[^>]*>/)?.[0];
  assert.ok(input, "the observed prompt exposes the code field");
  assert.match(input, /type="password"/);
  assert.match(input, /maxLength="8192"/i);
  assert.match(input, /autoComplete="off"/i);
  assert.doesNotMatch(input, /\b(?:name|value)=/);
  assert.match(markup, /Paste the code from the Claude page/);
  assert.match(markup, />Send code<\/button>/);
  const noPrompt = panel(claudeWaiting(false), props);
  assert.ok(noPrompt.includes(claudeUrl.replaceAll("&", "&amp;")));
  assert.doesNotMatch(noPrompt, /claude-sign-in-code/);
  for (const event of [{ kind: "close" }, { kind: "invalidate" }]) {
    assert.doesNotMatch(panel(auth.reduceNativeAuth(claudeWaiting(), event), props), /SYNTHETIC_UNIT_OAUTH_STATE|claude-sign-in-code/);
  }
  const sending = auth.reduceNativeAuth(claudeWaiting(), { kind: "claude_input_begin", request: 21 });
  assert.match(panel(sending, props), /Sending the code\.\.\./);
  const accepted = auth.reduceNativeAuth(sending, { kind: "claude_input_accepted", navigation: 7, request: 21 });
  const after = panel(accepted, props);
  assert.match(after, /Code sent\. Waiting for Claude to finish signing in\.\.\./);
  assert.doesNotMatch(after, /claude-sign-in-code/);
  assert.equal(signInStage(accepted, "claude").kind, "waiting", "an accepted code is not a sign-in");
  const unknown = auth.reduceNativeAuth(sending, { kind: "claude_input_failure", navigation: 7, request: 21 });
  assert.match(panel(unknown, props), /We couldn&#x27;t confirm the code was sent\. Check again before sending it a second time\./);
});

test("Codex browser sign-in shows its URL without the Claude code field, and an unsafe challenge shows nothing of it", () => {
  const browser = panel(answered({ action: { kind: "sign_in", method: "codex_browser" },
    challenge: { kind: "codex_browser", auth_url: "https://auth.example.invalid/login?state=SYNTHETIC_CODEX_STATE" } }, "codex_browser"));
  assert.match(browser, /SYNTHETIC_CODEX_STATE/);
  assert.match(browser, /Open this page in your browser to sign in/);
  assert.doesNotMatch(browser, /claude-sign-in-code|type="password"/);
  const unsafe = answered({ challenge: { kind: "codex_device_code", user_code: "QX4F‮9KTB", verification_url: "https://auth.example.invalid/device" } });
  const markup = panel(unsafe);
  assert.doesNotMatch(markup, /QX4F|auth\.example/);
  assert.match(markup, /The sign-in page sent something CrazyAgent can&#x27;t show safely\. Try again\./);
  assert.match(markup, />Try again<\/button>/);
});

test("failure panels name the fix: a new code, a key instead of a missing harness, or Check again", () => {
  const expired = panel(answered({ phase: "unauthenticated_observed", challenge: null, code: "native_auth_sign_in_expired" }));
  assert.match(expired, /This code expired\./);
  assert.match(expired, />Get a new code<\/button>/);
  const missingState = auth.reduceNativeAuth(begun(), { kind: "failure", navigation: 7, request: 1, code: "native_auth_host_unavailable" });
  assert.match(panel(missingState, { onUseKey: forbidden }), />Use an API key instead<\/button>/);
  assert.doesNotMatch(panel(missingState), /<button/);
  assert.match(panel(answered({ connection: "another-account" })), />Check again<\/button>/);
  assert.match(panel(answered({ phase: "authenticated_observed", challenge: null, account: null })), /Signed in\./);
});

test("account drawer copy: delete names the pools it leaves, support details carry no ids or keys", () => {
  const holding = (name, ids) => ({ id: `pool-${name}`, name, summary: "", members: ids.map((id) => ({ id, kind: "claude", provider: "claude", status: "signed_in", status_label: "Signed in" })), project_count: 1 });
  assert.equal(poolEffect(["a"], [holding("Main", ["b"])]), undefined);
  assert.equal(poolEffect(["a"], [holding("Main", ["a", "b"])]), "It leaves pool Main.");
  assert.equal(poolEffect(["a"], [holding("Main", ["a", "b"]), holding("Spare", ["a"]), holding("Night", ["a", "c"])]),
    "It leaves pools Main, Spare and Night. Spare has no other account, so it is deleted too and its projects have no pool.");
  const details = supportDetails(account({ method: "api_key", method_label: "Use an API key", model: "gpt-5", status: "key_saved", status_label: "Key saved" }));
  assert.equal(details, "Account: ChatGPT 1\nProvider: ChatGPT / Codex\nSign-in method: Use an API key\nStatus: Key saved\nModel: gpt-5");
  assert.doesNotMatch(details, /chatgpt-account-1/);
});

test("the account list filters, counts and windows 1000 accounts without rendering them all", () => {
  const many = Array.from({ length: 1000 }, (_, index) => account({ id: `a${index}`, name: `Account ${index}`,
    ...(index % 2 ? { provider: "claude", provider_name: "Claude", status: "not_signed_in", status_label: "Not signed in" } : {}) }));
  assert.equal(filterAccounts(many, "", "claude", "").length, 500);
  assert.equal(filterAccounts(many, "account 99", "", "").length, 11);
  assert.equal(filterAccounts(many, "", "", "not_signed_in").length, 500);
  assert.equal(accountCount(1000, 1000), "1,000 accounts");
  assert.equal(accountCount(12, 214), "12 of 214 accounts");
  assert.equal(accountCount(1, 1), "1 account");
  const markup = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "ok", value: many }, pools: { kind: "ok", value: [] },
    onAdd: forbidden, onOpen: forbidden, onRetry: forbidden }));
  assert.match(markup, /1,000 accounts/);
  assert.match(markup, /role="list" aria-label="Accounts"/);
  const rows = markup.match(/class="account-row"/g) ?? [];
  assert.ok(rows.length > 0 && rows.length < 60, `rendered ${rows.length} rows`);
  assert.match(markup, /aria-setsize="1000"/);
  const empty = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "ok", value: [] }, pools: { kind: "ok", value: [] }, onAdd: forbidden, onOpen: forbidden, onRetry: forbidden }));
  assert.match(empty, /<h2>No accounts yet<\/h2>/);
  assert.match(empty, /Add a ChatGPT, Claude, API key or local model account\. You can add as many as you like\./);
  const notReady = renderToStaticMarkup(createElement(AccountsView, { accounts: { kind: "not_ready" }, pools: { kind: "not_ready" }, onAdd: forbidden, onOpen: forbidden, onRetry: forbidden }));
  assert.match(notReady, /Not ready yet/);
});

const compatible = { provider: "openai_compatible", name: "Any OpenAI-compatible URL", methods: [
  { method: "url", label: "Connect a URL", url_preset: null, ...field("required", "optional", "required") }] };
const routing = /(?:Harness reference|Protocol reference|Profile reference|Credential reference|Account reference|Provider reference|Model author reference|rcli|openai-chat|anthropic-messages)/;
const claims = /(?:Signed in as|Account authenticated|Authenticated|Model ready)/;

function WithSession({ children }) {
  return createElement(SessionContext.Provider, { value: useSession() }, children);
}

const keyForm = (provider) => renderToStaticMarkup(createElement(WithSession, null, createElement(KeyOrUrlForm, {
  provider, option: methodOptions(provider)[0], name: "Unit account", onName: forbidden, onAdded: forbidden })));

test("Add account starts provider-first: managed sign-in, key and URL providers, no routing fields, no secret entry", () => {
  const tiles = renderToStaticMarkup(createElement(ProviderTiles, { catalog: [...catalog, compatible], onPick: forbidden }));
  for (const tile of providerTiles([...catalog, compatible])) {
    assert.ok(tiles.includes(`<span class="title">${tile.name}</span>`), tile.name);
    assert.ok(tiles.includes(`<span class="detail">${TILE_LINES[tile.key]}</span>`), tile.key);
  }
  const methods = renderToStaticMarkup(createElement(MethodMenu, { tile: providerTiles(catalog)[0], onPick: forbidden }));
  assert.match(methods, /Sign in with ChatGPT<span class="badge tone-info">Recommended<\/span>/);
  assert.match(methods, /Use the Codex sign-in on this Mac/);
  assert.match(methods, /Use an API key/);
  for (const markup of [tiles, methods]) {
    assert.doesNotMatch(markup, routing);
    assert.doesNotMatch(markup, /type="password"/);
    assert.doesNotMatch(markup, claims);
    assert.ok(Buffer.byteLength(markup, "utf8") < 65536);
  }
});

test("URL accounts show the base URL and model without routing identities; only the key field is masked", () => {
  const local = keyForm(catalog[3]);
  assert.match(local, /<input\b[^>]*id="account-url"[^>]*value="http:\/\/127\.0\.0\.1:11434\/v1"/);
  assert.match(local, /id="account-model"/);
  assert.doesNotMatch(local, /type="password"/);
  const endpoint = keyForm(compatible);
  assert.match(endpoint, /id="account-url"/);
  assert.match(endpoint, /id="account-model"/);
  assert.deepEqual([...endpoint.matchAll(/<input\b[^>]*type="password"[^>]*>/g)].map((match) => match[0].match(/id="([^"]+)"/)?.[1]), ["account-key"]);
  assert.match(endpoint, /API key \(optional\)/);
  assert.match(endpoint, /<label for="account-url">Base URL<\/label>/);
  assert.match(endpoint.match(/<input\b[^>]*id="account-url"[^>]*>/)?.[0] ?? "", /type="url"/);
  assert.match(endpoint, /<label for="account-model">Model<\/label>/);
  for (const markup of [local, endpoint]) {
    assert.doesNotMatch(markup, routing);
    assert.doesNotMatch(markup, claims);
  }
});

test("the API key field is masked and uncontrolled, and its copy says the key is sent once and never shown again", () => {
  const markup = keyForm(catalog[2]);
  const input = markup.match(/<input\b[^>]*id="account-key"[^>]*>/)?.[0];
  assert.ok(input, "a key account shows the key field");
  assert.match(input, /type="password"/);
  assert.match(input, /maxLength="8192"/i);
  assert.match(input, /autoComplete="off"/i);
  assert.doesNotMatch(input, /\b(?:value|name)=/);
  assert.match(markup, /Sent once to CrazyAgent&#x27;s engine, which keeps it privately\. You won&#x27;t see it again\./);
  assert.match(markup, />Save key<\/button>/);
  assert.doesNotMatch(markup, claims);
});
