import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const client = await import("../src/api-usage.ts");
const reset = await import("../src/screens/settings/spend-reset.tsx");
const section = await import("../src/screens/settings/spend-settings.tsx");

const NOW = Date.UTC(2026, 10, 12, 13, 0);
const settings = (confirm, all_limited = "notice") => ({ confirm, all_limited });
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
