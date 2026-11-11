import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const view = await import("../src/screens/project/overview-tabs.tsx");
const screen = await import("../src/screens/project/overview.tsx");
const api = await import("../src/api-overview.ts");

const TAB_IDS = ["quick_info", "features", "competitors", "install_and_use"];
const NOW = 10 * 60_000;

const base = () => ({
  readme: null, tabs: TAB_IDS.map((tab) => ({ tab, text: null, version: "", edited: false, proposal: null })),
  competitors: { kind: "missing" }, running: false, stale: false, checked_ms: null, failure: null, refresh: "after_landing", apply: "always_ask",
});
const withTab = (overview, id, text) => ({ ...overview, tabs: overview.tabs.map((tab) => tab.tab === id ? { ...tab, text, version: `v-${id}` } : tab) });
const job = (kind, status) => ({
  id: 1, project_id: "alpha", kind, kind_label: "Competitor research", status, status_label: "", report_path: `context/research/${kind}.md`,
  harness: "codex", tools: true, attempts: 0, claimer: null, error: null, cancel_requested: false, output_bytes: 0,
  created_ms: 1_000, started_ms: null, heartbeat_ms: null, finished_ms: null,
});
const projectData = (jobs = []) => ({
  id: "alpha", name: "Alpha", setup: undefined, research: { kind: "ok", value: { project_id: "alpha", jobs } }, context: undefined,
  pools: undefined, harnesses: undefined, assigned: null, now: NOW, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
  team: { result: undefined, at: 0 }, reloadTeam: () => {},
});
const idle = () => Promise.resolve({ kind: "ok", value: undefined });
const actions = { refresh: idle, save: idle, accept: idle, reject: idle, reload: () => {} };

function page({ selected, overview, about = "", mappingOverview = null, researchRunning = false, problem }) {
  const data = projectData(researchRunning ? [job("competitor_research", "running")] : []);
  return renderToStaticMarkup(createElement(view.OverviewTabs, {
    data, overview, problem, now: NOW, selected, onSelect: () => {}, actions,
    quickInfo: (text) => createElement(screen.OverviewFacts, { about, overview: text ?? mappingOverview }),
    activity: createElement("p", null, "Today's activity"),
  }));
}

test("the strip lists Quick info first, then Features, Competitors, Install & use, README and Activity", () => {
  assert.deepEqual(view.STRIP.map((item) => item.label), ["Quick info", "Features", "Competitors", "Install & use", "README", "Activity"]);
  assert.deepEqual(view.STRIP.map((item) => item.id), ["quick_info", "features", "competitors", "install_and_use", "readme", "activity"]);
  const html = page({ selected: "features", overview: base() });
  assert.match(html, /role="tablist"/);
  assert.equal((html.match(/role="tab"/g) ?? []).length, 6);
  assert.match(html, /<button[^>]*role="tab"[^>]*aria-selected="true"[^>]*>Features<\/button>/);
  assert.equal((html.match(/aria-selected="true"/g) ?? []).length, 1);
});

test("Quick info shows the saved About and the generated quick info through OverviewFacts", () => {
  const html = page({ selected: "quick_info", about: "A research reader.", overview: withTab(base(), "quick_info", "Alpha reads papers.\n") });
  assert.match(html, /A research reader\./);
  assert.match(html, /Alpha reads papers\./);
});

test("Quick info falls back to the mapping's overview until the first run wrote one", () => {
  const html = page({ selected: "quick_info", about: "A research reader.", mappingOverview: "# ScholarScope\n\nReads papers.\n", overview: base() });
  assert.match(html, /Reads papers\./);
});

test("the README tab renders the repo file, names it and says it is read only", () => {
  const html = page({ selected: "readme", overview: { ...base(), readme: { path: "README.md", text: "# Alpha\n\nReads papers.\n", cut: false } } });
  assert.match(html, /Reads papers\./);
  assert.match(html, /README\.md/);
  assert.match(html, /This is the project&#x27;s own file\. Edit it in the project\./);
  assert.doesNotMatch(html, />Edit</);
});

test("a cut README says only the first part is shown", () => {
  const html = page({ selected: "readme", overview: { ...base(), readme: { path: "README.md", text: "# Alpha\n", cut: true } } });
  assert.match(html, /Showing the first part of a long file\./);
});

test("a project without a README says so", () => {
  assert.match(page({ selected: "readme", overview: base() }), /This project has no README file\./);
});

test("a tab not written yet says what will write it, by the refresh setting", () => {
  assert.match(page({ selected: "features", overview: { ...base(), running: true } }), /Writing now/);
  assert.match(page({ selected: "features", overview: { ...base(), refresh: "refresh_only" } }), /Not written yet\. Choose Refresh to write it\./);
  assert.match(page({ selected: "features", overview: { ...base(), refresh: "off" } }), /Overview tabs are off in Project settings\./);
  assert.match(page({ selected: "features", overview: base() }), /Not written yet\. It is written after the next landed change set, or when you choose Refresh\./);
});

test("Refresh is disabled while a run is going and when the tabs are off", () => {
  assert.match(page({ selected: "features", overview: { ...base(), running: true } }), /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.match(page({ selected: "features", overview: { ...base(), refresh: "off" } }), /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.doesNotMatch(page({ selected: "features", overview: base() }), /<button[^>]*disabled=""[^>]*>Refresh<\/button>/);
  assert.match(page({ selected: "features", overview: base() }), />Refresh<\/button>/);
});

test("a failed run shows its plain reason and keeps the tab text", () => {
  const html = page({ selected: "features", overview: { ...withTab(base(), "features", "- Search\n"), failure: "The account is out of usage" } });
  assert.match(html, /Couldn&#x27;t refresh the tabs\. The account is out of usage/);
  assert.match(html, /Search/);
});

test("the strip says when the tabs were last checked and that a run is writing", () => {
  assert.match(page({ selected: "features", overview: { ...base(), checked_ms: NOW - 3 * 60_000 } }), /Checked 3 min ago/);
  assert.match(page({ selected: "readme", overview: { ...base(), running: true } }), /Writing now/);
  assert.doesNotMatch(page({ selected: "features", overview: base() }), /Checked/);
});

test("the Activity tab holds the activity panel, and README and Activity stay when the tabs could not be read", () => {
  assert.match(page({ selected: "activity", overview: base() }), /Today&#x27;s activity/);
  const lost = page({ selected: "activity", overview: null, problem: "Couldn't load the Overview tabs. The engine is busy." });
  assert.match(lost, /Today&#x27;s activity/);
  assert.equal((lost.match(/role="tab"/g) ?? []).length, 6);
  assert.match(page({ selected: "features", overview: null, problem: "Couldn't load the Overview tabs. The engine is busy." }), /Couldn&#x27;t load the Overview tabs\. The engine is busy\./);
  assert.match(page({ selected: "features", overview: null }), /Loading the Overview tabs/);
});

test("an arrow key moves to the next tab and wraps, Home and End jump", () => {
  assert.equal(view.stripMove("quick_info", "ArrowRight"), "features");
  assert.equal(view.stripMove("quick_info", "ArrowLeft"), "activity");
  assert.equal(view.stripMove("activity", "ArrowRight"), "quick_info");
  assert.equal(view.stripMove("features", "Home"), "quick_info");
  assert.equal(view.stripMove("features", "End"), "activity");
  assert.equal(view.stripMove("features", "a"), null);
});

test("the picked tab is kept per project and a missing or unknown stored value reads as Quick info", () => {
  const stored = new Map();
  const store = { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  assert.equal(view.storedTab("alpha", store), "quick_info");
  view.storeTab("alpha", "readme", store);
  assert.equal(view.storedTab("alpha", store), "readme");
  assert.equal(view.storedTab("beta", store), "quick_info");
  stored.set([...stored.keys()][0], "nonsense");
  assert.equal(view.storedTab("alpha", store), "quick_info");
  const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
  assert.equal(view.storedTab("alpha", broken), "quick_info");
  assert.doesNotThrow(() => view.storeTab("alpha", "readme", broken));
});

test("the tabs are read again only while a run is going or a change set landed", () => {
  assert.equal(view.pollEvery({ ...base(), running: true }), api.OVERVIEW_POLL_MS);
  assert.equal(view.pollEvery({ ...base(), stale: true }), api.OVERVIEW_POLL_MS);
  assert.equal(view.pollEvery({ ...base(), stale: true, refresh: "refresh_only" }), api.OVERVIEW_POLL_MS);
  assert.equal(view.pollEvery({ ...base(), stale: true, refresh: "off" }), null, "no run comes while the tabs are off, so nothing is waited for");
  assert.equal(view.pollEvery(base()), null);
  assert.equal(view.pollEvery(null), null);
  assert.equal(api.OVERVIEW_POLL_MS, 5_000);
});

test("every Overview call names its bridge command, and each command is in the handler list", () => {
  const source = readFileSync(new URL("../src/api-overview.ts", import.meta.url), "utf8");
  const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const command of ["desktop_overview_read", "desktop_overview_refresh", "desktop_overview_save", "desktop_overview_accept", "desktop_overview_reject", "desktop_overview_set_refresh", "desktop_overview_set_apply"]) {
    assert.match(source, new RegExp(`"${command}"`), command);
    assert.match(lib, new RegExp(`async fn ${command}\\(`), command);
    assert.match(lib, new RegExp(`\\n    ${command},\\n`), `${command} is in the handler list`);
  }
});

test("every Overview refusal has its own plain sentence", () => {
  const codes = ["overview_tab_changed", "overview_tab_blocked", "overview_tab_too_large", "overview_tab_empty", "overview_no_proposal", "overview_off", "overview_project_unknown", "overview_unavailable"];
  for (const code of codes) {
    assert.notEqual(api.overviewCopy(code), api.overviewCopy("something_else"), code);
    assert.doesNotMatch(api.overviewCopy(code), /overview_|\u{2014}/u, code);
  }
  assert.equal(new Set(codes.map((code) => api.overviewCopy(code))).size, codes.length, "no two refusals share a sentence");
  assert.equal(api.overviewCopy("overview_tab_changed"), "Someone changed this tab since you opened it. Your text is kept below; compare and save again.");
});
