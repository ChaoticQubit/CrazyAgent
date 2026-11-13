import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const view = await import("../src/screens/project/roadmap-status.tsx");
const api = await import("../src/api-projects.ts");

const NOW = Date.UTC(2026, 10, 11, 4, 0);
const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const status = (extra = {}) => ({
  items: [
    { item: { section: "V1", text: "Sign in" }, status: "tested", evidence: "tests/sign_in_test.rs" },
    { item: { section: "V1", text: "Search" }, status: "in_progress", evidence: "branch search" },
    { item: { section: "V2", text: "Share" }, status: "pending", evidence: "" },
  ],
  checked_ms: NOW - 60_000, running: false, stale: false, refresh: "after_landing", source: "docs/plan-2026.md", problem: null,
  waiting_for_account: false, ...extra,
});
const shown = (value, extra = {}) => renderToStaticMarkup(createElement(view.RoadmapStatusView, {
  status: value, now: NOW, busy: false, onRefresh: () => {}, ...extra,
}));

test("the roadmap status lists each item with its word and the counts on top", () => {
  const html = shown(status());
  assert.match(html, /1 tested, 0 done, 1 in progress, 1 pending/);
  assert.match(html, /Checked 1 min ago/);
  assert.match(html, /From docs\/plan-2026\.md/);
  for (const word of ["Tested", "In progress", "Pending"]) assert.match(html, new RegExp(`>${word}<`));
  assert.doesNotMatch(html, />Done</);
  assert.equal((html.match(/<h4/g) ?? []).length, 2, "one heading per section");
  assert.ok(html.indexOf(">V1<") < html.indexOf("Sign in") && html.indexOf("Search") < html.indexOf(">V2<"));
});

test("the evidence of an item opens under it, and an item without evidence shows none", () => {
  const html = shown(status());
  assert.equal((html.match(/<details/g) ?? []).length, 2);
  assert.match(html, /<details[^>]*><summary>Evidence<\/summary><p>tests\/sign_in_test\.rs<\/p><\/details>/);
});

test("the counts name every status once, in the order tested, done, in progress, pending", () => {
  assert.equal(view.statusCounts([]), "0 tested, 0 done, 0 in progress, 0 pending");
  assert.equal(view.statusCounts(status({ items: [...status().items, { item: { section: "V2", text: "Export" }, status: "done", evidence: "set 7" }] }).items),
    "1 tested, 1 done, 1 in progress, 1 pending");
  assert.deepEqual(Object.keys(view.STATUS_WORDS), ["tested", "done", "in_progress", "pending"]);
  assert.deepEqual(Object.values(view.STATUS_WORDS).map((entry) => entry.word), ["Tested", "Done", "In progress", "Pending"]);
});

test("a status never checked says so, and a running check says Checking now with Refresh off", () => {
  const never = shown(status({ items: [], checked_ms: null, source: null }));
  assert.match(never, /Not checked yet/);
  assert.doesNotMatch(never, /Checked \d|From /);
  assert.match(never, /<button[^>]*>Refresh<\/button>/);
  assert.doesNotMatch(never, /<button[^>]*disabled/);
  const running = shown(status({ running: true }));
  assert.match(running, /Checking now/);
  assert.match(running, /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  const busy = shown(status(), { busy: true });
  assert.match(busy, /<button[^>]*disabled=""[^>]*aria-busy="true"/);
});

test("Off turns Refresh off and says where to turn it on; Refresh only keeps the button", () => {
  const off = shown(status({ refresh: "off" }));
  assert.match(off, /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.match(off, /Roadmap status is off in Project settings/);
  const only = shown(status({ refresh: "refresh_only" }));
  assert.doesNotMatch(only, /<button[^>]*disabled/);
  assert.doesNotMatch(only, /Roadmap status is off/);
});

test("a check that is due and a failed check are both said in plain words", () => {
  const stale = shown(status({ stale: true }));
  assert.match(stale, />A new check is waiting to run\. After a landed change set it starts about 10 minutes after the last one\.</);
  const asked = shown(status({ stale: true, refresh: "refresh_only" }));
  assert.match(asked, />A new check is waiting to run\.</);
  assert.doesNotMatch(asked, /landed/);
  for (const quiet of [status(), status({ stale: true, running: true }), status({ stale: true, refresh: "off" })]) {
    assert.doesNotMatch(shown(quiet), /A new check is waiting to run/);
  }
  const failed = shown(status({ problem: "No account in this project's pool can run the check" }));
  assert.match(failed, /role="alert"[^>]*>The last check failed: No account in this project&#x27;s pool can run the check\.?</);
  const refused = shown(status(), { problem: "Roadmap status is off in Project settings." });
  assert.match(refused, /role="alert"[^>]*>Roadmap status is off in Project settings\.</);
});

test("a check that waits for an account says why under a status, never as a failed check alert, while it is due or running", () => {
  const reason = "No account in this project's pool can run the check";
  const waiting = { stale: true, problem: reason, waiting_for_account: true };
  for (const value of [status(waiting), status({ ...waiting, running: true })]) {
    const html = shown(value);
    assert.doesNotMatch(html, /role="alert"/);
    assert.doesNotMatch(html, /The last check failed/);
    assert.match(html, /role="status">No account in this project&#x27;s pool can run the check\.</);
  }
  assert.match(shown(status({ ...waiting, refresh: "off" })), /role="alert"[^>]*>The last check failed: No account/);
  const refused = shown(status(waiting), { problem: "Roadmap status is off in Project settings." });
  assert.match(refused, /role="alert"[^>]*>Roadmap status is off in Project settings\.</);
});

test("an ended failure stays a failed check alert while a new check is due and while it runs (IW-L1)", () => {
  const reason = "The last check did not finish";
  for (const value of [status({ stale: true, problem: reason }), status({ stale: true, running: true, problem: reason }), status({ running: true, problem: reason })]) {
    const html = shown(value);
    assert.match(html, /role="alert"[^>]*>The last check failed: The last check did not finish\.</, JSON.stringify(value));
    assert.doesNotMatch(html, /role="status">The last check did not finish\.</);
  }
});

test("text from the roadmap and the agent is shown through displayText, so hidden characters cannot hide", () => {
  const html = shown(status({
    items: [{ item: { section: "V1‮", text: "Sign​in" }, status: "done", evidence: "set‮41" }],
    source: "docs/plan‮.md",
  }));
  assert.doesNotMatch(html, /[‮​]/);
});

test("every Refresh option is offered once and the default is after each landed change set", () => {
  assert.deepEqual(view.REFRESH_OPTIONS.map((option) => option.value), ["after_landing", "refresh_only", "off"]);
  assert.deepEqual(view.REFRESH_OPTIONS.map((option) => option.label), ["After each landed change set and Refresh", "Refresh only", "Off"]);
  assert.equal(view.DEFAULT_REFRESH, "after_landing");
  const html = renderToStaticMarkup(createElement(view.RoadmapRefreshView, { value: "refresh_only", busy: false, problem: "", onValue: () => {} }));
  assert.match(html, /role="radiogroup"[^>]*aria-label="Roadmap status"/);
  assert.match(html, /Recommended: After each landed change set and Refresh\./);
  assert.equal((html.match(/aria-checked="true"/g) ?? []).length, 1);
});

test("a reply that is not a roadmap status is refused before it is shown", () => {
  assert.equal(api.isRoadmapStatus(status()), true);
  for (const broken of [
    null, {}, status({ items: "none" }), status({ refresh: "sometimes" }), status({ checked_ms: "now" }),
    status({ items: [{ item: { section: "V1", text: "Sign in" }, status: "shipped", evidence: "" }] }),
    status({ items: [{ item: { section: 1, text: "Sign in" }, status: "done", evidence: "" }] }),
    status({ items: Array.from({ length: api.ROADMAP_ITEMS_MAX + 1 }, () => status().items[0]) }),
    status({ running: "yes" }), status({ problem: 7 }), status({ waiting_for_account: "yes" }),
    (({ waiting_for_account, ...rest }) => rest)(status()),
  ]) assert.equal(api.isRoadmapStatus(broken), false, JSON.stringify(broken)?.slice(0, 80));
});

test("every roadmap status code the engine or the bridge can answer has its own sentence", () => {
  const transport = read("../../crates/crazyagent/src/engine/owner/transport/roadmap.rs");
  const list = transport.slice(transport.indexOf("pub const ROADMAP_CODES"));
  const codes = (list.slice(0, list.indexOf("];")).match(/"roadmap_status_[a-z_]+"/g) ?? []).map((code) => code.slice(1, -1));
  assert.ok(codes.length >= 8, String(codes.length));
  for (const code of ["roadmap_status_reply_bounds", "roadmap_status_request_bounds"]) assert.ok(codes.includes(code), code);
  const sentences = [...codes, "roadmap_status_invalid"].map((code) => [code, api.failureSentence(code)]);
  for (const [code, sentence] of sentences) {
    assert.equal(typeof sentence, "string", code);
    assert.ok(!sentence.includes(code), code);
  }
});

test("the three roadmap status calls go through the bridge with only the project id and the choice", () => {
  const source = read("src/api-projects.ts");
  assert.match(source, /call<RoadmapStatus>\("desktop_roadmap_status", \{ id \}\)/);
  assert.match(source, /call<void>\("desktop_refresh_roadmap_status", \{ id \}\)/);
  assert.match(source, /call<void>\("desktop_set_roadmap_refresh", \{ id, refresh \}\)/);
  const planning = read("src/screens/project/planning.tsx");
  assert.match(planning, /<RoadmapStatusPanel data=\{data\} settings=\{settings\} \/>/);
  assert.match(read("src/screens/project/settings.tsx"), /<PlanningLine data=\{data\} settings \/>/);
});
