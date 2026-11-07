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
  five_hour_reset_override_ms: null, weekly_reset_override_ms: null, source: "poll", observed_ms: NOW - 3 * MINUTE, checked_ms: NOW - 3 * MINUTE, failure: null,
  five_hour_left_out: null, weekly_left_out: null, five_hour_left_out_label: null, weekly_left_out_label: null, ...patch,
});
const report = (patch = {}) => ({
  since_ms: null, total: tokens({ input: 1200, output: 300, cache_read: 4000, cache_write: 800 }), runs: 3,
  projects: [row("project-harbor")], pools: [row("pool-main")], accounts: [row("acct-claude")], agents: [row("team-7"), row("research-scout")],
  days: [{ day_ms: NOW - 9 * HOUR, tokens: tokens({ input: 1200 }) }],
  lines: [{ project: "project-harbor", change_sets: 3, added: 90, removed: 57, commits: 2, committed_added: 80, committed_removed: 55 }],
  limits: [limits()], partial_homes: 0, ...patch,
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

test("ranges and day labels follow the engine's fixed-offset days across a daylight saving change", () => {
  const zone = process.env.TZ;
  process.env.TZ = "America/Los_Angeles";
  try {
    const spring = new Date(Date.UTC(2026, 2, 11, 19));
    const offset = usage.offsetMinutes(spring);
    assert.equal(offset, -420);
    assert.equal(usage.rangeStart("7d", spring), Date.UTC(2026, 2, 5, 7));
    assert.equal(usage.rangeStart("7d", new Date(Date.UTC(2026, 10, 3, 19))), Date.UTC(2026, 9, 28, 8));
    const dates = [5, 6, 7, 8, 9, 10, 11];
    const days = dates.map((date) => ({ day_ms: Math.floor((Date.UTC(2026, 2, date, 12) + offset * MINUTE) / DAY) * DAY - offset * MINUTE, tokens: tokens({ input: date }) }));
    assert.equal(days[0].day_ms, Date.UTC(2026, 2, 5, 7));
    const label = (date) => new Date(Date.UTC(2026, 2, date)).toLocaleDateString(undefined, { timeZone: "UTC", weekday: "short", month: "short", day: "numeric" });
    assert.deepEqual(days.map((day) => usage.dayLabel(day.day_ms, offset)), dates.map(label));
    const words = text(render({ nowMs: spring.getTime(), report: { kind: "ok", value: report({ days }) } }));
    const at = dates.map((date) => words.indexOf(`${label(date)} `));
    assert.ok(at.every((index, place) => index >= 0 && (place === 0 || index > at[place - 1])), `every day keeps its own date, in order: ${words}`);
    assert.equal(words.split(label(7)).length, 2, "the Saturday before the change is listed once");
    assert.ok(!words.includes(new Date(Date.UTC(2026, 2, 4)).toLocaleDateString(undefined, { timeZone: "UTC", weekday: "short", month: "short", day: "numeric" })), "no day before the range is listed");
  } finally {
    if (zone === undefined) delete process.env.TZ;
    else process.env.TZ = zone;
  }
});

test("token counts read compactly with the cache share of prompt tokens", () => {
  assert.equal(usage.compactCount(950), "950");
  assert.equal(usage.compactCount(1234), "1.2K");
  assert.equal(usage.compactCount(3_400_000), "3.4M");
  assert.equal(usage.compactCount(999_950), "1M");
  assert.equal(usage.headlineTokens(tokens({ input: 1, output: 2, cache_read: 3, cache_write: 4 })), 7);
  assert.equal(usage.headlineTokens(tokens({ input: 194, output: 139_484, cache_read: 17_665_095, cache_write: 288_322 })), 428_000);
  assert.equal(usage.totalTokens, undefined);
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
  const copy = readFileSync(new URL("../../../crates/crazyagent/src/engine/bootstrap/native_profiles/agent_copy.rs", import.meta.url), "utf8");
  const codex = new Set([...copy.matchAll(/"(native_profile_codex_[a-z_]+)"/g)].map((match) => match[1]));
  assert.equal(codex.size, 3, `Codex credential codes: ${[...codex]}`);
  for (const code of codex) assert.ok(client.USAGE_POLL_FAILURES.includes(code), `${code} has its own sentence`);
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

test("a reading with a home cut at the scan bound says it is partial", () => {
  assert.equal(usage.partialLine(0), null);
  assert.equal(usage.partialLine(1), "Some usage may be missing: 1 agent home holds more records than one scan reads, so its least recently changed records are not counted.");
  assert.equal(usage.partialLine(3), "Some usage may be missing: 3 agent homes hold more records than one scan reads, so their least recently changed records are not counted.");
  assert.doesNotMatch(text(render()), /may be missing/);
  const cut = text(render({ report: { kind: "ok", value: report({ partial_homes: 2 }) } }));
  assert.match(cut, /Some usage may be missing: 2 agent homes hold more records than one scan reads/);
});

test("a sign-in the limits check cannot use says on the card what it is and what to do, never the fallback", () => {
  const sentences = {
    native_profile_codex_api_key: "An API-key account has no 5-hour or weekly window.",
    native_profile_codex_token_unreadable: "CrazyAgent can't read this Codex sign-in. Sign in to Codex again in Accounts.",
    native_profile_codex_sign_in_expired: "This Codex sign-in expired and CrazyAgent couldn't renew it. Sign in to Codex again in Accounts.",
    native_profile_credential_unavailable: "CrazyAgent can't read this account's sign-in. Sign in again in Accounts.",
    native_profile_renewal_failed: "CrazyAgent couldn't renew this account's sign-in. It tries again in 10 minutes.",
    native_profile_renewal_busy: "Another check of this account's sign-in is running. CrazyAgent tries again in 10 minutes.",
    native_profile_sign_in_revoked: "The provider ended this sign-in. Sign in again in Accounts.",
  };
  for (const [code, sentence] of Object.entries(sentences)) {
    assert.equal(client.pollFailure(code), sentence, code);
    const unread = { plan: null, five_hour: null, weekly: null, source: null, observed_ms: null, failure: code };
    const markup = render({ report: { kind: "ok", value: report({ limits: [limits(unread)] }) } });
    const words = text(markup);
    assert.ok(words.includes(sentence), `${code}: ${words}`);
    assert.doesNotMatch(words, /Something went wrong|native_profile/, code);
    const tone = code === "native_profile_codex_api_key" ? "notice info" : "notice";
    assert.match(markup, new RegExp(`<div class="${tone}"[^>]*>(?:(?!</div>).)*${sentence.slice(0, 10)}`), code);
  }
});

test("the usage page shows totals, limits with countdowns, named groups and lines changed", () => {
  const markup = render();
  const words = text(markup);
  assert.match(words, /Tokens 2\.3K 1\.2K in, 300 out/);
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
  assert.match(words, /Harbor Notes 1\.5K 1\.2K 300 4K 2 5 min ago/);
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
  assert.match(words, /By project Name Tokens In Out Cache read Runs Last used Harbor Notes .* Unattributed 1\.5K/);
  assert.match(words, /By account Name Tokens In Out Cache read Runs Last used Unattributed 1\.5K/);
  assert.match(words, /Master, Harbor Notes 1\.5K/);
  assert.match(words, /Onboarding 1\.5K/);
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

test("a failed read with no window shows each window as not read and points at the notice's cause, never the provider", () => {
  const sentence = "CrazyAgent can't read this account's sign-in. Sign in again in Accounts.";
  const unread = { plan: null, five_hour: null, weekly: null, source: null, observed_ms: null, failure: "native_profile_credential_unavailable" };
  const words = text(render({ report: { kind: "ok", value: report({ limits: [limits(unread)] }) } }));
  for (const label of ["5-hour", "Weekly"]) {
    assert.ok(words.includes(`${label} Not read The last read failed, for the reason above.`), `${label}: ${words}`);
  }
  assert.ok(words.indexOf(sentence) >= 0 && words.indexOf(sentence) < words.indexOf("5-hour"), words);
  assert.doesNotMatch(words, /Not reported|hasn't reported/);
});

test("an API-key account's windows keep the provider-omitted wording, since it has none by design", () => {
  const keyed = { plan: null, five_hour: null, weekly: null, source: null, observed_ms: null, failure: "native_profile_codex_api_key" };
  const words = text(render({ report: { kind: "ok", value: report({ limits: [limits(keyed)] }) } }));
  assert.ok(words.includes("An API-key account has no 5-hour or weekly window."), words);
  for (const label of ["5-hour", "Weekly"]) {
    assert.ok(words.includes(`${label} Not reported The provider hasn't reported this window.`), `${label}: ${words}`);
  }
  assert.doesNotMatch(words, /Not read The last read failed/);
});

test("a window a good read left out shows the engine's sentence for it, with no use, meter or reset time, and a failed read keeps Not read (N-D16, r1 F1)", () => {
  const card = (patch) => render({ report: { kind: "ok", value: report({ limits: [limits(patch)] }) } });
  const count = (markup, pattern) => (markup.match(pattern) ?? []).length;
  for (const sentence of ["Codex reports no 5-hour limit for this plan.", "Claude reports no 5-hour window right now.", "A sentence only the engine wrote."]) {
    const markup = card({ plan: "prolite", five_hour: null, five_hour_left_out: "codex", five_hour_left_out_label: sentence });
    const words = text(markup);
    assert.ok(words.includes(` 5-hour ${sentence} Weekly 81% used `), words);
    assert.equal(count(markup, /<meter /gu), 1, markup);
    assert.equal(count(markup, /Set reset time/gu), 1, markup);
    assert.doesNotMatch(words, /Not reported|hasn't reported|Not read/);
  }
  const weekly = text(card({ weekly: null, weekly_left_out: "claude", weekly_left_out_label: "A weekly sentence only the engine wrote." }));
  assert.ok(weekly.includes(" Weekly A weekly sentence only the engine wrote. "), weekly);
  const failed = text(card({ five_hour: null, five_hour_left_out: "claude", five_hour_left_out_label: "Claude reports no 5-hour window right now.", failure: "usage_unreachable" }));
  assert.ok(failed.includes("5-hour Not read The last read failed, for the reason above."), failed);
  assert.doesNotMatch(failed, /reports no/);
  assert.equal(usage.leftOutText, undefined, "the card keeps no copy of the engine's sentence");
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

test("a harness without a reader reads as usage not reported, never as zero", () => {
  const unreported = (key, harness) => row(key, { tokens: tokens(), runs: 0, not_reported_by: harness });
  const only = text(render({ report: { kind: "ok", value: report({
    total: tokens(), runs: 0, lines: [], pools: [], accounts: [], days: [],
    projects: [unreported("project-harbor", "RCLI")], agents: [unreported("master-project-harbor", "RCLI")], not_reported_by: ["RCLI"],
  }) } }));
  assert.match(only, /Tokens Not reported Usage not reported by RCLI/);
  assert.match(only, /By project Name Tokens In Out Cache read Runs Last used Harbor Notes Usage not reported by RCLI 5 min ago/);
  assert.match(only, /By agent .* Usage not reported by RCLI 5 min ago/);
  assert.doesNotMatch(only, /No usage in this range/);
  assert.doesNotMatch(only, /Harbor Notes 0 /);

  const mixed = text(render({ report: { kind: "ok", value: report({
    projects: [row("project-harbor", { not_reported_by: "Gemini CLI" }), unreported("project-docs", "OpenCode")],
    not_reported_by: ["Gemini CLI", "OpenCode"],
  }) } }));
  assert.match(mixed, /Tokens 2\.3K 1\.2K in, 300 out/);
  assert.match(mixed, /Not in these totals: usage not reported by Gemini CLI, OpenCode\./);
  assert.match(mixed, /Harbor Notes 1\.5K \+ usage not reported by Gemini CLI 1\.2K 300 4K 2 5 min ago/);
  assert.match(mixed, /project-docs Usage not reported by OpenCode 5 min ago/);

  const older = text(render({ report: { kind: "ok", value: report() } }));
  assert.doesNotMatch(older, /not reported/, "a report from an engine without the field shows plain counts");
});

test("a usage step that panicked shows one plain warning with its count and last time", () => {
  assert.doesNotMatch(text(render()), /internal error/u);
  const shown = text(render({ report: { kind: "ok", value: report({ health: [
    { step: "poll", panics: 1, last_ms: NOW - 30_000, recovered_ms: null },
    { step: "runs", panics: 4, last_ms: NOW - 3 * HOUR, recovered_ms: null },
  ] }) } }));
  assert.match(shown, /Checking account limits hit an internal error once since the engine started, last just now\. CrazyAgent keeps retrying; limits shown may be out of date\./u);
  assert.match(shown, /Counting research run tokens hit an internal error 4 times since the engine started, last 3 h ago\. CrazyAgent keeps retrying; research token totals may be low\./u);
  assert.equal(
    usage.healthText({ step: "team", panics: 2, last_ms: NOW - 5 * MINUTE, recovered_ms: null }, NOW),
    "Counting agent tokens hit an internal error 2 times since the engine started, last 5 min ago. CrazyAgent keeps retrying; agent and research run token totals may be low.",
  );
});

test("a usage step kind that worked again after its last panic says when, without a retry warning", () => {
  const line = usage.healthText({ step: "poll", panics: 1, last_ms: NOW - 3 * HOUR, recovered_ms: NOW - 2 * HOUR }, NOW);
  assert.equal(line, "Checking account limits hit an internal error once since the engine started, last 3 h ago. It worked again 2 h ago.");
  const shown = text(render({ report: { kind: "ok", value: report({ health: [
    { step: "poll", panics: 1, last_ms: NOW - 3 * HOUR, recovered_ms: NOW - 2 * HOUR },
  ] }) } }));
  assert.ok(shown.includes(line), shown);
  assert.doesNotMatch(shown, /keeps retrying|may be out of date/u);
});
