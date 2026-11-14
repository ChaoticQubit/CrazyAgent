import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const client = await import("../src/api-usage.ts");
const reset = await import("../src/screens/settings/spend-reset.tsx");
const section = await import("../src/screens/settings/spend-settings.tsx");
const poolsScreen = await import("../src/screens/settings/pools.tsx");

const NOW = Date.UTC(2026, 10, 12, 13, 0);
const settings = (confirm, all_limited = "notice", resets_source = "off") => ({ confirm, all_limited, resets_source });
const attempt = (patch = {}) => ({ account: "codex-a", origin: "owner", state: "settled", outcome: "reset", requested_ms: NOW - 1000, settled_ms: NOW, ...patch });
const view = (patch = {}) => ({ settings: settings("confirm_each"), attempts: [], limited_pools: [], ...patch });
const noop = () => {};

test("Spend confirmation: confirm each asks first, one click spends at once, off shows no Spend at all", () => {
  const base = { inApp: true, resetsLeft: 2, attempt: undefined };
  assert.deepEqual(reset.spendFlow({ ...base, settings: settings("confirm_each") }), { kind: "ready", confirm: true });
  assert.deepEqual(reset.spendFlow({ ...base, settings: settings("one_click") }), { kind: "ready", confirm: false });
  assert.deepEqual(reset.spendFlow({ ...base, settings: settings("off") }), { kind: "hidden" });
});

test("no Spend for an account whose provider has no in-app spend, or that holds no free reset", () => {
  const on = settings("one_click");
  assert.deepEqual(reset.spendFlow({ settings: on, inApp: false, resetsLeft: 3 }), { kind: "hidden" });
  assert.deepEqual(reset.spendFlow({ settings: on, inApp: true, resetsLeft: 0 }), { kind: "hidden" });
  assert.deepEqual(reset.spendFlow({ settings: on, inApp: true, resetsLeft: null }), { kind: "ready", confirm: false });
});

test("a spend that is queued or sent keeps the button busy with its line, a settled one shows its outcome", () => {
  const on = settings("confirm_each");
  assert.deepEqual(reset.spendFlow({ settings: on, inApp: true, resetsLeft: 1, attempt: attempt({ state: "queued", outcome: null, settled_ms: null }) }),
    { kind: "busy", line: "Using a free reset now." });
  assert.deepEqual(reset.spendFlow({ settings: on, inApp: true, resetsLeft: 1, attempt: attempt({ state: "sent", outcome: null, settled_ms: null }) }),
    { kind: "busy", line: "Sent. Waiting for the provider's answer." });
  assert.deepEqual(reset.spendFlow({ settings: on, inApp: true, resetsLeft: 1, attempt: attempt({ outcome: "no_credit" }) }),
    { kind: "ready", confirm: true, line: "This account has no free reset left." });
});

test("the confirm names the account, says it cannot be undone and labels a private call that may break", () => {
  const plain = reset.confirmCopy("Bravo", false);
  assert.equal(plain.title, "Use a free reset on Bravo?");
  assert.equal(plain.confirmLabel, "Use free reset");
  assert.match(plain.body, /can't be undone\.$/);
  assert.doesNotMatch(plain.body, /may break/);
  assert.match(reset.confirmCopy("Bravo", true).body, /private call that may break\./);
});

test("the Spend button renders for confirm each and one click, nothing while spending is off", () => {
  const draw = (confirm) => renderToStaticMarkup(createElement(reset.SpendReset, {
    account: "codex-a", name: "Bravo", inApp: true, mayBreak: true, resetsLeft: 2, spend: view({ settings: settings(confirm) }), onDone: noop,
  }));
  assert.match(draw("confirm_each"), />Use a free reset</);
  assert.match(draw("one_click"), />Use a free reset</);
  assert.equal(draw("off"), "");
});

test("both spend settings show every option, the defaults first and unchanged", () => {
  assert.deepEqual(section.CONFIRM_OPTIONS.map((option) => option.value), ["confirm_each", "one_click", "off"]);
  assert.deepEqual(section.ALL_LIMITED_OPTIONS.map((option) => option.value), ["notice", "no_hint", "auto_spend"]);
  assert.deepEqual(client.SPEND_DEFAULTS, settings("confirm_each", "notice"));
  const markup = renderToStaticMarkup(createElement(section.SpendSettingsView, { settings: client.SPEND_DEFAULTS, refused: "", onChange: noop }));
  assert.match(markup, /aria-label="Spend confirmation"/);
  assert.match(markup, /aria-label="When every pool account is limited"/);
  for (const option of [...section.CONFIRM_OPTIONS, ...section.ALL_LIMITED_OPTIONS]) assert.match(markup, new RegExp(`>${option.label}<`));
  assert.doesNotMatch(markup, /changed/);
});

test("the Claude free resets setting shows its three options, Don't read first as the default, each with its own help line", () => {
  assert.deepEqual(section.RESETS_SOURCE_OPTIONS.map((option) => [option.value, option.label]), [
    ["off", "Don't read"],
    ["through_claude_code", "Through Claude Code"],
    ["as_claude_code", "As Claude Code"],
  ]);
  assert.equal(client.SPEND_DEFAULTS.resets_source, "off");
  assert.equal(section.resetsSourceHelp("through_claude_code"), "Until Claude Code can report free resets itself, CrazyAgent asks Anthropic as Claude Code, the same as As Claude Code. Anthropic may treat this as against its terms, and the risk falls on your Claude accounts.");
  assert.equal(section.resetsSourceHelp("as_claude_code"), "CrazyAgent asks Anthropic for free resets as Claude Code, the way Claude Code itself asks. Anthropic may treat this as against its terms, and the risk falls on your Claude accounts.");
  assert.equal(section.resetsSourceHelp("off"), "CrazyAgent never asks for Claude free resets, and Claude accounts say off in Settings. Don't read is the default.");
  const escaped = (text) => text.replaceAll("'", "&#x27;");
  const markup = renderToStaticMarkup(createElement(section.SpendSettingsView, { settings: client.SPEND_DEFAULTS, refused: "", onChange: noop }));
  assert.match(markup, /aria-label="Claude free resets"/);
  for (const option of section.RESETS_SOURCE_OPTIONS) assert.ok(markup.includes(`>${escaped(option.label)}<`), option.label);
  assert.ok(markup.includes(escaped(section.resetsSourceHelp("off"))), "the default shows the Don't read help line");
  assert.doesNotMatch(markup, /changed/);
  for (const source of ["through_claude_code", "as_claude_code"]) {
    const picked = renderToStaticMarkup(createElement(section.SpendSettingsView, { settings: settings("confirm_each", "notice", source), refused: "", onChange: noop }));
    assert.match(picked, /changed/, source);
    assert.ok(picked.includes(escaped(section.resetsSourceHelp(source))), source);
  }
});

test("settings from an engine that sends no Claude free resets source read as Don't read, never as a Claude Code option", () => {
  const older = renderToStaticMarkup(createElement(section.SpendSettingsView, { settings: { confirm: "confirm_each", all_limited: "notice" }, refused: "", onChange: noop }));
  assert.ok(older.includes(section.resetsSourceHelp("off").replaceAll("'", "&#x27;")), "the help line is Don't read's");
  assert.doesNotMatch(older, /changed/);
  const source = readFileSync(new URL("../src/screens/settings/spend-settings.tsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /\?\? "through_claude_code"|\?\? "as_claude_code"/);
});

test("each option has its own help line, and auto-spend says it does nothing while spending is off", () => {
  const helps = section.CONFIRM_OPTIONS.map((option) => section.confirmHelp(option.value));
  assert.equal(new Set(helps).size, 3);
  const limited = section.ALL_LIMITED_OPTIONS.map((option) => section.allLimitedHelp(settings("confirm_each", option.value)));
  assert.equal(new Set(limited).size, 3);
  assert.match(section.allLimitedHelp(settings("off", "auto_spend")), /^Spending is off, so CrazyAgent never uses a free reset on its own\./);
  for (const line of [...helps, ...limited]) assert.match(line, /^[A-Z].*\.$/);
  const markup = renderToStaticMarkup(createElement(section.SpendSettingsView, { settings: settings("off", "auto_spend"), refused: "", onChange: noop }));
  assert.match(markup, /changed/);
  assert.match(markup, /Spending is off, so CrazyAgent never uses a free reset on its own\./);
});

test("the limited pool notice names the accounts with free resets, or says none has one", () => {
  const names = new Map([["codex-a", "Alpha"], ["codex-b", "Bravo"], ["codex-c", "Charlie"]]);
  const pool = { pool: "pool-a", name: "Daily work", since_ms: NOW, with_resets: ["codex-b", "codex-c"] };
  assert.equal(section.limitedPoolLine(pool, names), "Every account in Daily work is at its limit. Free resets: Bravo and Charlie.");
  assert.equal(section.limitedPoolLine({ ...pool, with_resets: [] }, names), "Every account in Daily work is at its limit. None of them has a free reset.");
  assert.equal(section.limitedPoolLine({ ...pool, name: null, with_resets: ["codex-z"] }, names), "Every account in a pool is at its limit. Free resets: an account.");
  const markup = renderToStaticMarkup(createElement(section.LimitedPoolsNotice, { spend: view({ limited_pools: [pool] }), names, inApp: () => true, onDone: noop }));
  assert.match(markup, /Free resets: Bravo and Charlie\./);
  assert.equal((markup.match(/>Use a free reset</g) ?? []).length, 2);
  assert.equal(renderToStaticMarkup(createElement(section.LimitedPoolsNotice, { spend: view(), names, inApp: () => true, onDone: noop })), "");
});

test("every spend outcome the engine stores reads as its own sentence", () => {
  const engine = ["spend.rs", "spend/wire.rs"].map((file) => readFileSync(new URL(`../../../crates/crazyagent/src/usage/${file}`, import.meta.url), "utf8")).join("\n");
  const codes = new Set([...engine.matchAll(/"(usage_spend_[a-z_]+|reset|nothing_to_reset|no_credit|already_redeemed|outcome_unknown)"/g)].map((match) => match[1]));
  for (const code of ["reset", "nothing_to_reset", "no_credit", "already_redeemed", "outcome_unknown", "usage_spend_off", "usage_spend_not_signed_in", "usage_spend_failed", "usage_spend_refused", "usage_spend_unavailable"]) assert.ok(codes.has(code), code);
  for (const code of codes) assert.ok(client.SPEND_OUTCOMES_KNOWN.includes(code), `${code} has its own sentence`);
  for (const code of client.SPEND_OUTCOMES_KNOWN) {
    const line = client.spendOutcome(attempt({ outcome: code }));
    assert.match(line, /^[A-Z].*\.$/, code);
    assert.doesNotMatch(line, /_/, code);
  }
  assert.equal(client.spendOutcome(attempt({ outcome: "usage_new_thing" })), "Something went wrong. Try again. (usage_new_thing)");
  assert.equal(client.usageRefusal({ code: "usage_spend_off" }), "Spending free resets is off. Turn it on in Settings, Pools.");
});

test("the window may call both spend commands", () => {
  const allowed = JSON.parse(readFileSync(new URL("../src-tauri/capabilities/main-window.json", import.meta.url), "utf8")).permissions;
  for (const permission of ["allow-desktop-usage-spend", "allow-desktop-usage-set-spend"]) assert.ok(allowed.includes(permission), permission);
});

test("a spend says whether the owner confirmed it, and only the confirm dialog says yes", () => {
  const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
  assert.match(read("../src/api-usage.ts"), /call<void>\("desktop_usage_spend", \{ account, confirmed \}\)/);
  assert.match(read("../src-tauri/src/lib.rs"), /fn desktop_usage_spend\([^)]*account: String,\s*confirmed: bool/);
  const control = read("../src/screens/settings/spend-reset.tsx");
  assert.match(control, /onConfirm=\{\(\) => go\(true\)\}/);
  assert.equal((control.match(/go\(true\)/g) ?? []).length, 1, "only the dialog confirms");
  assert.match(control, /setAsking\(true\) : go\(false\)/);
  assert.equal(client.usageRefusal({ code: "usage_spend_unconfirmed" }), "Spend confirmation now asks first. Use a free reset again and confirm it.");
});

test("each Pools row holds its own Spend for an account with a free reset, whatever the all-limited setting", () => {
  const ok = (value) => ({ kind: "ok", value });
  const loading = { kind: "loading" };
  const forbidden = () => { throw new Error("static rendering must not save or navigate"); };
  const member = { id: "codex-a", kind: "chatgpt", provider: "chatgpt", status: "not_signed_in", status_label: "Not signed in" };
  const pool = { id: "pool-main", name: "Main", summary: "1 ChatGPT", members: [member], project_count: 0 };
  const account = { id: "codex-a", name: "Bravo", provider: "chatgpt", provider_name: "ChatGPT / Codex", method: "subscription", method_label: "Sign in with ChatGPT",
    url: null, model: null, status: "not_signed_in", status_label: "Not signed in" };
  const ranking = [{ pool: "pool-main", strategy: "spread", own_order_projects: 0, accounts: [{ connection: "codex-a", standing: { kind: "not_signed_in" } }] }];
  const read = (resets) => new Map([["codex-a", { harness: "Codex", adapter: "codex", may_break: true, resets, credits: null, spend: { kind: "in_app" },
    not_reported: "not reported by Codex", resets_checked_ms: NOW, credits_checked_ms: null }]]);
  const two = read({ count: 2, kinds: [], next_expiry_ms: null });
  const draw = (spend, extras) => renderToStaticMarkup(createElement(poolsScreen.PoolsScreenView, {
    pools: ok([pool]), accounts: ok([account]), schedule: loading, ranking: ok(ranking), limits: loading, extras, spend, onSpent: noop, nowMs: NOW, selected: undefined,
    saved: undefined, apply: forbidden, onSelect: forbidden, onNew: forbidden, onDelete: forbidden, onRetry: forbidden, onRetryOrder: forbidden, onRetrySchedule: forbidden,
    onToggleAdd: forbidden, onDoneAdd: forbidden, onAdd: forbidden, onRemove: forbidden, onRename: forbidden, onDraftName: forbidden,
  }));
  const row = (markup) => markup.slice(markup.indexOf('class="pool-row'));
  for (const [confirm, limited] of [["confirm_each", "no_hint"], ["one_click", "auto_spend"], ["one_click", "notice"]]) {
    assert.match(row(draw(view({ settings: settings(confirm, limited) }), two)), /class="spend-reset"[\s\S]*>Use a free reset</, `${confirm} ${limited}`);
  }
  assert.doesNotMatch(draw(view({ settings: settings("off", "no_hint") }), two), /spend-reset/, "spending off");
  assert.doesNotMatch(draw(view({ settings: settings("one_click", "no_hint") }), read({ count: 0, kinds: [], next_expiry_ms: null })), /spend-reset/, "no free reset");
  assert.doesNotMatch(draw(view({ settings: settings("one_click", "no_hint") }), read(null)), /spend-reset/, "count not reported");
  assert.doesNotMatch(draw(view({ settings: settings("one_click", "no_hint") }), read({ count: 2, kinds: [], next_expiry_ms: NOW })), /spend-reset/, "resets expired");
  assert.doesNotMatch(draw(view({ settings: settings("one_click", "no_hint") }), new Map()), /spend-reset/, "extras not read");
  assert.doesNotMatch(draw(undefined, two), /spend-reset/, "spend view not read");
});

test("Free resets shows no choice while the usage read loads, failed or has no spend part, never the defaults as stored", () => {
  const draw = (usage) => renderToStaticMarkup(createElement(section.SpendSettingsSection, { usage, onDone: noop, onRetry: noop }));
  const loading = draw({ kind: "loading" });
  assert.doesNotMatch(loading, /role="radiogroup"/);
  assert.match(loading, /role="status"[^>]*>Loading your free resets settings\.\.\.</);
  const failed = draw({ kind: "error", code: "usage_unavailable", message: "Usage couldn't be read. Try again." });
  assert.doesNotMatch(failed, /role="radiogroup"/);
  assert.match(failed, /Usage couldn&#x27;t be read\. Try again\./);
  assert.match(failed, />Try again</);
  assert.doesNotMatch(draw({ kind: "not_ready" }), /role="radiogroup"/);
  assert.doesNotMatch(draw({ kind: "ok", value: {} }), /role="radiogroup"/);
  const stored = draw({ kind: "ok", value: { spend: view({ settings: settings("off", "no_hint", "as_claude_code") }) } });
  for (const label of ["Off", "No hint", "As Claude Code"]) assert.match(stored, new RegExp(`aria-checked="true"[^>]*>${label}<`), label);
  for (const label of ["Confirm each", "Notice", "Don&#x27;t read"]) assert.match(stored, new RegExp(`aria-checked="false"[^>]*>${label}<`), label);
});

test("free resets saves go one at a time in the order picked, the ones picked meanwhile collapse to the latest, and only that one answers", async () => {
  const tick = () => new Promise((resolve) => setImmediate(resolve));
  const sent = [];
  const answers = [];
  const push = section.latestSaves((value) => { sent.push(value); return new Promise((resolve) => answers.push(resolve)); });
  const done = { kind: "ok", value: undefined };
  const asClaudeCode = push(settings("confirm_each", "notice", "as_claude_code"));
  const throughClaudeCode = push(settings("confirm_each", "notice", "through_claude_code"));
  const dontRead = push(settings("confirm_each", "notice", "off"));
  await tick();
  assert.deepEqual(sent.map((value) => value.resets_source), ["as_claude_code"]);
  answers[0]({ kind: "error", code: "usage_unavailable", message: "Usage couldn't be read. Try again." });
  assert.equal(await asClaudeCode, null);
  assert.equal(await throughClaudeCode, null);
  await tick();
  assert.deepEqual(sent.map((value) => value.resets_source), ["as_claude_code", "off"]);
  answers[1](done);
  assert.deepEqual(await dontRead, done);
  const later = push(settings("one_click", "notice", "off"));
  await tick();
  assert.equal(sent.length, 3);
  answers[2](done);
  assert.deepEqual(await later, done);
});
