import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import sheet from "../../src/screens/project/overview-tabs.css?inline";
import { OverviewTabs } from "../../src/screens/project/overview-tabs.tsx";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";
export { contrast } from "./contrast.mjs";

const NOW = 10 * 60_000;
const TAB_IDS = ["quick_info", "features", "competitors", "install_and_use"];
let root = null;

const overview = () => ({
  readme: null,
  tabs: TAB_IDS.map((tab) => ({ tab, text: tab === "features" ? "- Search papers\n- Save notes\n" : null, version: `v-${tab}`, edited: false, proposal: null, proposal_version: "", blocked: null })),
  competitors: { kind: "missing" }, running: false, stale: false, checked_ms: NOW - 3 * 60_000, failure: null, refresh: "after_landing", apply: "always_ask", needs_home: false,
});

const stored = {
  project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research: ["market_research"], pool: null,
  team: { master: "codex", master_tools: true, sub_harnesses: [], min_agents: 1, max_agents: 1 }, documents: [], folder: "/tmp/alpha", created_ms: 0, revision: 4,
};
const data = {
  id: "alpha", name: "Alpha", setup: { kind: "ok", value: stored }, research: { kind: "ok", value: { project_id: "alpha", jobs: [] } }, context: undefined,
  pools: undefined, harnesses: undefined, assigned: null, now: NOW,
  navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {}, team: { result: undefined, at: 0 }, reloadTeam: () => {},
};
const idle = () => Promise.resolve({ kind: "ok", value: undefined });
const actions = { refresh: idle, save: idle, accept: idle, reject: idle, reload: () => {} };
const memory = { getItem: () => null, setItem: () => {} };

/** Mounts the real app shell and stylesheets around the Overview tabs on a written Features
 * tab checked three minutes ago, in one theme. Input: the theme. Output: nothing. */
export function mount(theme) {
  root?.unmount();
  document.documentElement.dataset.theme = theme;
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, sheet].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const sidebar = h(Sidebar, { route: { name: "home" }, projects: [{ id: "alpha", name: "Alpha" }], profileName: "Mira Okafor", engine: "running", onRetry: () => {} });
  const tabs = h(OverviewTabs, { data, overview: overview(), now: NOW, selected: "features", onSelect: () => {}, actions, runMode: "off",
    quickInfo: () => null, activity: h("p", null, "Today's activity"), store: memory });
  flushSync(() => root.render(h(AppShell, { sidebar }, h("div", { className: "pj pj-project" }, tabs))));
}

const box = (node) => {
  const rect = node.getBoundingClientRect();
  return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, middle: (rect.top + rect.bottom) / 2 };
};

/** Measures the Overview's strip and the row under it. Output: the strip's and the row's
 * boxes, the row's borders, its status and buttons with their names, and how far the main
 * region scrolls sideways. */
export function measure() {
  const bar = document.querySelector(".ov-bar");
  const main = document.querySelector(".main");
  const look = getComputedStyle(bar);
  return {
    strip: box(document.querySelector("[role=tablist]")),
    bar: box(bar),
    borders: [look.borderTopWidth, look.borderBottomWidth],
    status: box(bar.querySelector("[role=status]")),
    buttons: [...bar.querySelectorAll("button")].map((button) => ({ name: button.textContent, ...box(button) })),
    sideways: main.scrollWidth - main.clientWidth,
  };
}
