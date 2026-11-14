import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const view = await import("../src/screens/project/overview-tabs.tsx");
const screen = await import("../src/screens/project/overview.tsx");

const TAB_IDS = ["quick_info", "features", "competitors", "install_and_use"];
const FOREIGN = "This tab's file came with the project's files: CrazyAgent didn't write it and you didn't save it. Check it before you use its commands.";
const FOREIGN_REVIEW = "This tab's file came with the project's files. Accept replaces it with the proposal; Reject keeps it.";
const INSTALL = "## Install\n\n```sh\ncargo install alpha\nalpha init\n```\n";

const base = () => ({
  readme: null, tabs: TAB_IDS.map((tab) => ({ tab, text: null, version: "", edited: false, foreign: false, proposal: null, proposal_version: "", blocked: null })),
  competitors: { kind: "missing" }, running: false, stale: false, checked_ms: null, failure: null, waiting_for_account: false, refresh: "after_landing", apply: "always_ask", needs_home: false,
});
const withTab = (overview, id, text, patch = {}) => ({ ...overview, tabs: overview.tabs.map((tab) => tab.tab === id ? { ...tab, text, version: `v-${id}`, ...patch } : tab) });
const stored = {
  project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research: ["market_research"], pool: null,
  team: { master: "codex", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 }, documents: [], folder: "/tmp/alpha", created_ms: 0, revision: 4,
};
const projectData = () => ({
  id: "alpha", name: "Alpha", setup: { kind: "ok", value: stored }, research: { kind: "ok", value: { project_id: "alpha", jobs: [] } }, context: undefined,
  pools: undefined, harnesses: undefined, assigned: null, now: 0, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
  team: { result: undefined, at: 0 }, reloadTeam: () => {},
});
const idle = () => Promise.resolve({ kind: "ok", value: undefined });
const actions = { refresh: idle, save: idle, accept: idle, reject: idle, reload: () => {} };

/** The Overview with one tab picked, rendered to HTML. Inputs: the picked tab, the overview
 * and the Run mode. Output: the markup. */
function page(selected, overview, runMode) {
  return renderToStaticMarkup(createElement(view.OverviewTabs, {
    data: projectData(), overview, problem: undefined, now: 0, selected, onSelect: () => {}, actions, runMode,
    quickInfo: (text) => createElement(screen.OverviewFacts, { about: "", overview: text }),
    activity: createElement("p", null, "Today's activity"), store: undefined,
  }));
}

/** The words a page shows: tags dropped, entities read, spaces collapsed. Input: the markup.
 * Output: the text. */
function text(html) {
  return html.replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/&quot;/g, "\"").replace(/&amp;/g, "&").replace(/\s+/g, " ");
}

test("RV-DS241-2: Install & use from the project's files says so and gets Copy only, never Run, whatever the Run mode", () => {
  for (const runMode of ["type_and_run", "type_only"]) {
    const html = page("install_and_use", withTab(base(), "install_and_use", INSTALL, { foreign: true }), runMode);
    assert.ok(text(html).includes(FOREIGN), runMode);
    assert.equal((html.match(/>Copy</g) ?? []).length, 2, runMode);
    assert.doesNotMatch(html, />Run in project terminal</, runMode);
    assert.doesNotMatch(html, /You edited this tab/, runMode);
  }
});

test("Install & use the engine wrote or the owner saved keeps Run, with no note about the project's files", () => {
  for (const patch of [{}, { edited: true }]) {
    const html = page("install_and_use", withTab(base(), "install_and_use", INSTALL, patch), "type_and_run");
    assert.equal((html.match(/>Run in project terminal</g) ?? []).length, 2);
    assert.ok(!text(html).includes(FOREIGN));
  }
});

test("any tab from the project's files says so, and only that tab", () => {
  const overview = withTab(base(), "features", "- Search papers\n", { foreign: true });
  assert.ok(text(page("features", overview, "off")).includes(FOREIGN));
  assert.ok(!text(page("install_and_use", withTab(overview, "install_and_use", INSTALL), "off")).includes(FOREIGN));
});

test("the review of a proposal for a tab from the project's files never says the owner edited it", () => {
  const state = { tab: "features", text: "- theirs\n", version: "v1", edited: false, foreign: true, proposal: "- Search\n", proposal_version: "p1", blocked: null };
  const html = renderToStaticMarkup(createElement(view.ProposalReview, { label: "Features", state, busy: false, note: "", onAccept: () => {}, onReject: () => {}, onClose: () => {} }));
  assert.ok(text(html).includes(FOREIGN_REVIEW));
  assert.doesNotMatch(html, /You edited this tab/);
});
