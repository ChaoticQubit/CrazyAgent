import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const { extrasLine, expiryText, resetsText, creditsText, newResetLine, freshResets } = await import("../src/screens/metrics/extras.ts");

const NOW = new Date(2026, 10, 12, 9, 0).getTime();
const TODAY_1430 = new Date(2026, 10, 12, 14, 30).getTime();
const NOV_20 = new Date(2026, 10, 20, 8, 0).getTime();

const extras = (over = {}) => ({
  harness: "Codex",
  adapter: "codex",
  may_break: false,
  resets: null,
  credits: null,
  spend: { kind: "not_reported" },
  not_reported: "not reported by Codex",
  resets_checked_ms: null,
  credits_checked_ms: null,
  ...over,
});

test("a known count of zero reads No free resets, never a blank or a bare 0", () => {
  assert.equal(resetsText(extras({ resets: { count: 0, kinds: [], next_expiry_ms: null } }), NOW), "No free resets");
});

test("an absent count reads not reported by its harness, never 0", () => {
  const line = resetsText(extras({ harness: "Gemini CLI", adapter: "not_reported", not_reported: "not reported by Gemini CLI" }), NOW);
  assert.equal(line, "Free resets: not reported by Gemini CLI");
  assert.doesNotMatch(line, /\b0\b/);
});

test("one and two free resets read in the singular and the plural", () => {
  assert.equal(resetsText(extras({ resets: { count: 1, kinds: [], next_expiry_ms: null } }), NOW), "1 free reset");
  assert.equal(resetsText(extras({ resets: { count: 2, kinds: [], next_expiry_ms: null } }), NOW), "2 free resets");
});

test("the kinds the provider names follow the count, and none are made up when it names none", () => {
  assert.equal(resetsText(extras({ resets: { count: 3, kinds: ["five_hour", "weekly", "model"], next_expiry_ms: null } }), NOW), "3 free resets (5-hour, weekly, model)");
  assert.equal(resetsText(extras({ resets: { count: 1, kinds: ["all_limits"], next_expiry_ms: null } }), NOW), "1 free reset (all limits)");
  assert.equal(resetsText(extras({ resets: { count: 2, kinds: [], next_expiry_ms: null } }), NOW), "2 free resets");
});

test("the next expiry reads as a time today or as a day of the month, and is left out when not known", () => {
  assert.equal(expiryText(TODAY_1430, NOW), "today 14:30");
  assert.equal(expiryText(NOV_20, NOW), "Nov 20");
  assert.equal(resetsText(extras({ resets: { count: 2, kinds: ["weekly"], next_expiry_ms: NOV_20 } }), NOW), "2 free resets (weekly), next expires Nov 20");
  assert.equal(resetsText(extras({ resets: { count: 2, kinds: [], next_expiry_ms: TODAY_1430 } }), NOW), "2 free resets, next expires today 14:30");
});

test("a count whose next expiry has passed reads not reported, as the engine reads it, until it is read again", () => {
  assert.equal(resetsText(extras({ resets: { count: 2, kinds: [], next_expiry_ms: NOW - 1 } }), NOW), "Free resets: not reported by Codex");
  assert.equal(resetsText(extras({ resets: { count: 2, kinds: [], next_expiry_ms: NOW } }), NOW), "Free resets: not reported by Codex");
});

test("a count read through a private call carries may break; a count that is not reported does not", () => {
  const claude = { harness: "Claude Code", adapter: "claude", may_break: true, not_reported: "not reported by Claude Code" };
  assert.equal(resetsText(extras({ ...claude, resets: { count: 4, kinds: [], next_expiry_ms: null } }), NOW), "4 free resets (may break)");
  assert.equal(resetsText(extras({ ...claude, resets: { count: 0, kinds: [], next_expiry_ms: null } }), NOW), "No free resets (may break)");
  assert.equal(resetsText(extras(claude), NOW), "Free resets: not reported by Claude Code");
});

test("credits read in the provider's words, or not reported by the harness", () => {
  assert.equal(creditsText(extras({ credits: "$4.20 left" })), "Credits: $4.20 left");
  assert.equal(creditsText(extras()), "Credits: not reported by Codex");
});

test("the row line joins resets, credits and where to spend, and folds two not reported fields into one", () => {
  assert.equal(extrasLine(extras(), NOW), "Free resets and credits: not reported by Codex");
  assert.equal(extrasLine(extras({ resets: { count: 2, kinds: ["five_hour"], next_expiry_ms: NOV_20 }, credits: "$4.20 left" }), NOW),
    "2 free resets (5-hour), next expires Nov 20 · Credits: $4.20 left");
  assert.equal(extrasLine(extras({ resets: { count: 0, kinds: [], next_expiry_ms: null } }), NOW), "No free resets · Credits: not reported by Codex");
  assert.equal(extrasLine(extras({ credits: "$12.00 left", spend: { kind: "elsewhere", place: "openrouter.ai" } }), NOW),
    "Free resets: not reported by Codex · Credits: $12.00 left · Spend at openrouter.ai");
});

test("a key, URL or local account still gets its line, named by its own harness", () => {
  const key = extras({ harness: "OpenRouter", adapter: "open_router", not_reported: "not reported by OpenRouter", credits: "$3.10 left", spend: { kind: "elsewhere", place: "openrouter.ai" } });
  assert.equal(extrasLine(key, NOW), "Free resets: not reported by OpenRouter · Credits: $3.10 left · Spend at openrouter.ai");
  const local = extras({ harness: "Ollama", adapter: "not_reported", not_reported: "not reported by Ollama" });
  assert.equal(extrasLine(local, NOW), "Free resets and credits: not reported by Ollama");
});

test("a kind this build has no words for reads as its own words rather than breaking the row", () => {
  assert.equal(resetsText(extras({ resets: { count: 1, kinds: ["fable_model"], next_expiry_ms: null } }), NOW), "1 free reset (fable model)");
});

test("a new free reset is told once per arrival, naming the account and the count now", () => {
  assert.equal(newResetLine("Work", 1), "Work has a new free reset. 1 free reset now.");
  assert.equal(newResetLine("Work", 3), "Work has a new free reset. 3 free resets now.");
  const row = (account, count, gained) => ({ account, extras: extras({ resets: { count, kinds: [], next_expiry_ms: null }, resets_gained_ms: gained }) });
  const first = freshResets(null, [row("a", 2, NOW - 60_000), row("b", 1, null)], NOW);
  assert.deepEqual(first.send.map((item) => [item.account, item.count]), [["a", 2]], "a gain from the last poll is told on the first look");
  const again = freshResets(first.known, [row("a", 2, NOW - 60_000), row("b", 1, null)], NOW + 60_000);
  assert.deepEqual(again.send, [], "the same gain is never told twice");
  const later = freshResets(again.known, [row("a", 3, NOW + 120_000), row("b", 1, null)], NOW + 180_000);
  assert.deepEqual(later.send.map((item) => [item.account, item.count]), [["a", 3]]);
  const old = freshResets(null, [row("a", 2, NOW - 2 * 3_600_000)], NOW);
  assert.deepEqual(old.send, [], "a gain from long before the app looked is not told as new");
  const gone = freshResets(later.known, [row("a", 3, NOW + 240_000)].map((item) => ({ ...item, extras: { ...item.extras, resets: null } })), NOW + 300_000);
  assert.deepEqual(gone.send, [], "a count that is not reported now is never told as new");
});
