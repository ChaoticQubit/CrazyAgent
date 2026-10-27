import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const client = await import("../src/api-usage.ts");
const usage = await import("../src/screens/settings/usage.tsx");
const router = await import("../src/ui/router.ts");

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const NOW = Date.UTC(2026, 9, 27, 9, 0);
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const tokens = (patch = {}) => ({ input: 0, output: 0, cache_read: 0, cache_write: 0, ...patch });
const row = (key, patch = {}) => ({ key, tokens: tokens({ input: 1200, output: 300, cache_read: 4000 }), runs: 2, last_ms: NOW - 5 * MINUTE, ...patch });
const limits = (patch = {}) => ({
  account: "acct-claude", plan: "max", five_hour: { used_percent: 34.4, resets_at_ms: NOW + 2 * HOUR }, weekly: { used_percent: 81, resets_at_ms: NOW + 3 * DAY + 4 * HOUR },
  five_hour_reset_override_ms: null, weekly_reset_override_ms: null, source: "poll", observed_ms: NOW - 3 * MINUTE, checked_ms: NOW - 3 * MINUTE, failure: null, ...patch,
});
const report = (patch = {}) => ({
  since_ms: null, total: tokens({ input: 1200, output: 300, cache_read: 4000, cache_write: 800 }), runs: 3,
  projects: [row("project-harbor")], pools: [row("pool-main")], accounts: [row("acct-claude")], agents: [row("team-7"), row("research-scout")],
  days: [{ day_ms: NOW - 9 * HOUR, tokens: tokens({ input: 1200 }) }],
  lines: [{ project: "project-harbor", change_sets: 3, added: 90, removed: 57, commits: 2, committed_added: 80, committed_removed: 55 }],
  limits: [limits()], ...patch,
});
const names = {
  projects: new Map([["project-harbor", "Harbor Notes"]]),
  pools: new Map([["pool-main", "Main pool"]]),
  accounts: new Map([["acct-claude", "Work Claude"]]),
};
const render = (patch = {}) => renderToStaticMarkup(createElement(usage.UsageView, {
  report: { kind: "ok", value: report() }, range: "7d", names, nowMs: NOW, failure: "", busy: null,
  onRange: forbidden, onRefresh: forbidden, onSetReset: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");

test("ranges start at the viewer's local midnight and hold whole local days", () => {
  const now = new Date(2026, 9, 27, 15, 30);
  assert.equal(usage.rangeStart("today", now), new Date(2026, 9, 27).getTime());
  assert.equal(usage.rangeStart("7d", now), new Date(2026, 9, 21).getTime());
  assert.equal(usage.rangeStart("30d", now), new Date(2026, 8, 28).getTime());
  assert.equal(usage.rangeStart("all", now), null);
  assert.deepEqual(usage.RANGES.map((range) => range.value), ["today", "7d", "30d", "all"]);
  assert.equal(usage.offsetMinutes({ getTimezoneOffset: () => -330 }), 330);
  assert.equal(usage.offsetMinutes({ getTimezoneOffset: () => 300 }), -300);
  assert.ok(Object.is(usage.offsetMinutes({ getTimezoneOffset: () => 0 }), 0));
});

test("token counts read compactly with the cache share of prompt tokens", () => {
  assert.equal(usage.compactCount(950), "950");
  assert.equal(usage.compactCount(1234), "1.2K");
  assert.equal(usage.compactCount(3_400_000), "3.4M");
  assert.equal(usage.compactCount(999_950), "1M");
  assert.equal(usage.totalTokens(tokens({ input: 1, output: 2, cache_read: 3, cache_write: 4 })), 10);
  assert.equal(usage.cacheShare(tokens({ input: 100, cache_read: 300, cache_write: 100 })), 60);
  assert.equal(usage.cacheShare(tokens({ output: 50 })), null);
});

test("a limit window ages like the engine's: a manual reset wins and a passed reset frees the window", () => {
  const window = { used_percent: 40, resets_at_ms: NOW + HOUR };
  assert.deepEqual(usage.windowNow(window, null, NOW), window);
  assert.deepEqual(usage.windowNow(window, NOW + 3 * HOUR, NOW), { used_percent: 40, resets_at_ms: NOW + 3 * HOUR });
  assert.deepEqual(usage.windowNow(window, null, NOW + HOUR), { used_percent: 0, resets_at_ms: null });
  assert.deepEqual(usage.windowNow(window, NOW - 1, NOW), { used_percent: 0, resets_at_ms: null });
  assert.deepEqual(usage.windowNow({ used_percent: 12, resets_at_ms: null }, null, NOW), { used_percent: 12, resets_at_ms: null });
  assert.equal(usage.windowNow(null, NOW + HOUR, NOW), null);
});

test("reset countdowns, read times and agent names are plain words", () => {
  assert.equal(usage.resetText(null, NOW), "Next reset not known yet");
  assert.equal(usage.resetText(NOW + 30_000, NOW), "Resets in under a minute");
  assert.equal(usage.resetText(NOW + 14 * MINUTE + 59_000, NOW), "Resets in 14 min");
  assert.equal(usage.resetText(NOW + 2 * HOUR + 14 * MINUTE, NOW), "Resets in 2 h 14 min");
  assert.equal(usage.resetText(NOW + 3 * DAY + 4 * HOUR, NOW), "Resets in 3 d 4 h");
  assert.equal(usage.agoText(NOW - 10_000, NOW), "just now");
  assert.equal(usage.agoText(NOW + 10_000, NOW), "just now");
  assert.equal(usage.agoText(NOW - 5 * MINUTE, NOW), "5 min ago");
  assert.equal(usage.agoText(NOW - 3 * HOUR, NOW), "3 h ago");
  assert.equal(usage.agoText(NOW - 2 * DAY, NOW), "2 d ago");
  assert.equal(usage.readLine(limits(), NOW), "Read from the provider 3 min ago");
  assert.equal(usage.readLine(limits({ source: "transcript", observed_ms: NOW - 2 * HOUR }), NOW), "Read from a run's own record 2 h ago");
  assert.equal(usage.readLine(limits({ source: null, observed_ms: null }), NOW), "Not read yet");
  assert.equal(usage.agentLabel("team-7"), "Agent 7");
  assert.equal(usage.agentLabel("research-scout"), "Research scout");
  assert.equal(usage.agentLabel("owner"), "owner");
  assert.equal(usage.agentLabel("onboarding"), "Onboarding");
  assert.equal(usage.agentLabel("master"), "Master");
  assert.equal(usage.agentLabel("master-project-harbor"), "Master, project-harbor");
  assert.equal(usage.agentLabel("master-project-harbor", (key) => key === "project-harbor" ? "Harbor Notes" : key), "Master, Harbor Notes");
});

test("a manual reset must be a real future local time", () => {
  assert.deepEqual(usage.manualReset("", NOW), { kind: "invalid", reason: "Pick a date and time." });
  assert.deepEqual(usage.manualReset("not a time", NOW), { kind: "invalid", reason: "Pick a date and time." });
  const past = new Date(NOW - HOUR);
  const local = (date) => `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}T${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  assert.deepEqual(usage.manualReset(local(past), NOW), { kind: "invalid", reason: "Pick a time in the future." });
  const later = new Date(NOW + 2 * HOUR);
  assert.deepEqual(usage.manualReset(local(later), NOW), { kind: "ok", at: NOW + 2 * HOUR });
});

test("every usage refusal and failed limit check reads as its own sentence", () => {
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/usage.rs", import.meta.url), "utf8");
  const listed = engine.slice(engine.indexOf("pub const USAGE_CODES"), engine.indexOf("];", engine.indexOf("pub const USAGE_CODES")));
  const own = [...listed.matchAll(/"(usage_[a-z_]+)"/g)].map((match) => match[1]);
  assert.ok(own.length >= 6, `engine usage codes: ${own.length}`);
  for (const code of own.filter((code) => !["usage_request_bounds", "usage_reply_bounds"].includes(code))) {
    assert.ok(client.USAGE_REFUSALS.includes(code), `${code} has its own sentence`);
  }
  const poll = readFileSync(new URL("../../../crates/crazyagent/src/usage/poll.rs", import.meta.url), "utf8")
    + readFileSync(new URL("../../../crates/crazyagent/src/usage/service.rs", import.meta.url), "utf8");
  const failures = new Set([...poll.matchAll(/"(usage_[a-z_]+)"/g)].map((match) => match[1]).filter((code) => code !== "usage_unavailable"));
  assert.ok(failures.size >= 7, `poll failure codes: ${failures.size}`);
  for (const code of failures) assert.ok(client.USAGE_POLL_FAILURES.includes(code), `${code} has its own sentence`);
  for (const line of [...client.USAGE_REFUSALS.map((code) => client.usageRefusal({ code })), ...client.USAGE_POLL_FAILURES.map(client.pollFailure)]) {
    assert.match(line, /^[A-Z].*\.$/);
    assert.doesNotMatch(line, /_/, line);
  }
  assert.equal(client.pollFailure("usage_sign_in_lapsed"), "The provider no longer accepts this sign-in. Sign in again in Accounts.");
  assert.equal(client.pollFailure("native_profile_torn"), "Something went wrong. Try again. (native_profile_torn)");
  assert.equal(client.usageRefusal({ code: "usage_limits_invalid" }), "That reset time isn't valid. Pick a time in the future.");
  assert.equal(client.usageNotReady({ kind: "not_ready" }), true);
  assert.equal(client.usageNotReady({ kind: "error", code: "usage_unavailable", message: "" }), true);
  assert.equal(client.usageNotReady({ kind: "error", code: "usage_outcome_unknown", message: "" }), false);
});

test("the usage page shows totals, limits with countdowns, named groups and lines changed", () => {
  const markup = render();
  const words = text(markup);
  assert.match(words, /Tokens 6\.3K 1\.2K in, 300 out/);
  assert.match(words, /Cache 67% 4K read, 800 written/);
  assert.match(words, /Runs 3/);
  assert.match(words, /Lines changed \+90 -57/);
  assert.match(words, /Work Claude max/);
  assert.match(words, /Read from the provider 3 min ago/);
  assert.match(words, /5-hour 34% used/);
  assert.match(words, /Resets in 2 h 0 min/);
  assert.match(words, /Weekly 81% used/);
  assert.match(words, /Resets in 3 d 4 h/);
  assert.match(markup, /<meter[^>]*value="34.4"/);
  assert.match(words, /Set reset time/);
  assert.doesNotMatch(words, /Use provider's time/);
  for (const title of ["By project", "By pool", "By account", "By agent", "By day", "Lines changed by project"]) assert.match(words, new RegExp(title));
  assert.match(words, /Harbor Notes 5\.5K 1\.2K 300 4K 2 5 min ago/);
  assert.match(words, /Main pool/);
  assert.match(words, /Agent 7/);
  assert.match(words, /Research scout/);
  assert.match(words, /Harbor Notes 3 \+90 -57 2 \+80 -55/);
  assert.doesNotMatch(words, /project-harbor|pool-main|acct-claude|team-7/);
  assert.match(markup, /aria-label="Range"/);
  assert.match(words, /Check limits now/);
});

test("runs tied to no project or account show as Unattributed and the master as its project's row", () => {
  const words = text(render({ report: { kind: "ok", value: report({
    projects: [row("project-harbor"), row("")], accounts: [row("")], agents: [row("master-project-harbor"), row("onboarding")],
  }) } }));
  assert.match(words, /By project Name Tokens In Out Cache read Runs Last used Harbor Notes .* Unattributed 5\.5K/);
  assert.match(words, /By account Name Tokens In Out Cache read Runs Last used Unattributed 5\.5K/);
  assert.match(words, /Master, Harbor Notes 5\.5K/);
  assert.match(words, /Onboarding 5\.5K/);
});

test("a manual reset, a failed check and a passed reset show on the account's card", () => {
  const words = text(render({ report: { kind: "ok", value: report({ limits: [limits({
    five_hour_reset_override_ms: NOW + 45 * MINUTE, weekly: { used_percent: 90, resets_at_ms: NOW - MINUTE }, failure: "usage_rate_limited",
  })] }) } }));
  assert.match(words, /Resets in 45 min \(your time\)/);
  assert.match(words, /Use provider's time/);
  assert.match(words, /Weekly 0% used/);
  assert.match(words, /Next reset not known yet/);
  assert.match(words, /The provider asked CrazyAgent to wait before reading limits again\./);
  const unreported = text(render({ report: { kind: "ok", value: report({ limits: [limits({ weekly: null, plan: null })] }) } }));
  assert.match(unreported, /Weekly Not reported/);
});

test("the usage page has empty, loading, error and not-ready states", () => {
  const empty = text(render({ report: { kind: "ok", value: report({ total: tokens(), runs: 0, projects: [], pools: [], accounts: [], agents: [], days: [], lines: [], limits: [] }) } }));
  assert.match(empty, /No usage in this range/);
  assert.match(empty, /No limits read yet/);
  assert.doesNotMatch(empty, /By project/);
  assert.match(render({ report: { kind: "loading" } }), /skeleton/);
  const error = text(render({ report: { kind: "error", code: "usage_outcome_unknown", message: "" } }));
  assert.match(error, /CrazyAgent couldn't confirm what happened\./);
  assert.match(error, /Try again/);
  assert.match(text(render({ report: { kind: "not_ready" } })), /Usage isn't ready yet/);
  assert.match(text(render({ report: { kind: "error", code: "usage_unavailable", message: "" } })), /Usage isn't ready yet/);
  assert.match(text(render({ failure: "That reset time isn't valid. Pick a time in the future." })), /That reset time isn't valid/);
});

test("Settings has a Usage tab", () => {
  assert.ok(router.SETTINGS_TABS.includes("usage"));
  assert.deepEqual(router.parseRoute("#/settings/usage"), { name: "settings", tab: "usage" });
  assert.equal(router.formatRoute({ name: "settings", tab: "usage" }), "#/settings/usage");
  const settings = readFileSync(new URL("../src/screens/settings/index.tsx", import.meta.url), "utf8");
  assert.match(settings, /\{ id: "usage", label: "Usage", icon: "trending-up" \}/);
  assert.match(settings, /tab === "usage" \? <UsageSettings \/>/);
});
