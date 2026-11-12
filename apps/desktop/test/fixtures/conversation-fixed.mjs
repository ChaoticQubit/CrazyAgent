import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import activityStyles from "../../src/screens/project/team-activity.css?inline";
import consoleStyles from "../../src/screens/project/console.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";
import { ProjectHead } from "../../src/screens/project/head.tsx";
import { LiveMasterView } from "../../src/screens/project/live-master.tsx";
import { TeamActivityList, activityRows } from "../../src/screens/project/team-activity.tsx";

let root = null;

const TABS = ["Overview", "Conversation", "Agents", "Changes", "Questions", "Context", "Process", "Metrics", "Settings"]
  .map((label) => ({ id: label.toLowerCase(), label, href: `#/projects/alpha/${label.toLowerCase()}` }));
const ABOUT = "A booking web app for small studios: a live calendar, deposits at checkout and reminders by email.";
const projects = Array.from({ length: 12 }, (_, index) => ({ id: `p${index}`, name: `Project ${index}` }));

/** One sub-agent of the team graph, live, with its own terminal. Input: its number. Output: the node. */
const agent = (id) => ({ id: `team-${id}`, kind: "sub", parent: "m", label: `Agent ${id} (@task-${id})`, harness: "codex", status: "working",
  status_label: "Working", account: null, terminal: `team-${id}`, agent: id });

/** The project data the live master view reads: a ready setup with a Codex master and no team
 * read yet, so nothing calls the engine for a switch. */
const data = {
  id: "alpha", name: "Alpha", research: undefined, context: undefined, pools: undefined, harnesses: undefined, assigned: null, now: 0,
  setup: { kind: "ok", value: { project_id: "alpha", name: "Alpha", about: ABOUT, tech_stack: [], platforms: [], research: [], pool: null, documents: [],
    folder: "/tmp/alpha", created_ms: 0, revision: 1, team: { master: "codex", master_tools: true, sub_harnesses: [] } } },
  navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {}, team: { result: undefined, at: 0 }, reloadTeam: () => {},
};

/** Mounts the project page's Conversation tab the way ProjectPage draws it (DEC-125): the app
 * shell, the fixed project page with its head, the banner box with `banners` notes in it, and
 * the tab panel with the master starting on the left and on the right the Team bar listing
 * `agents` sub-agents, or an open agent terminal in its place when `terminal` is set. The
 * terminal is its panel's outer box only, since the check is the layout the CSS gives it.
 * Input: the agents, the banners and whether an agent terminal is open. */
export function mount({ agents = 30, banners = 0, terminal = false } = {}) {
  root?.unmount();
  document.head.innerHTML = "";
  for (const text of [styles, projectStyles, activityStyles, consoleStyles, teamStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const graph = { project_id: "alpha", edges: [], nodes: [
    { id: "m", kind: "master", parent: null, label: "Master", harness: "codex", status: "working", status_label: "Working", account: null, terminal: "team-0" },
    ...Array.from({ length: agents }, (_, index) => agent(index + 1)),
  ] };
  const side = terminal ? h("div", { className: "tg-terminal" }) : [
    h("div", { key: "head", className: "ta-head" }, h("h3", null, "Team")),
    h(TeamActivityList, { key: "list", rows: activityRows(graph, null), now: 0, open: null, onOpen: () => {} }),
  ];
  const notes = Array.from({ length: banners }, (_, index) => h("div", { key: index, className: "pj-note warn", role: "status" },
    h("p", null, `Banner ${index + 1}: edits from an agent wait in Changes for your review before they land on main.`)));
  flushSync(() => root.render(h(AppShell, {
    sidebar: h(Sidebar, { route: { name: "home" }, projects, profileName: "Mira Okafor", engine: "running", onRetry: () => {} }),
  }, h("div", { className: "pj pj-project pj-fixed" },
    h(ProjectHead, { title: "Alpha", about: ABOUT, tabs: TABS, current: "conversation", scrolled: false, actions: null }),
    h("div", { className: "pj-banners" }, notes),
    h("div", { className: "pj-tab-panel", id: "pj-tab-panel", role: "tabpanel" },
      h("div", { className: `ta-split${terminal ? " with-terminal" : ""}` },
        h("div", { className: "ta-main" }, h(LiveMasterView, { data, harness: "codex", live: { kind: "starting" }, onStart: async () => {} })),
        h("aside", { className: "ta-side", "aria-label": "Team activity" }, h("div", { className: "ta-scroll" }, side))))))));
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Reads the layout: the window, the document's and the main region's scroll sizes and
 * offsets, the boxes of the panel, the master, the two columns and the agent terminal, the
 * Team bar's and the banner box's scroll sizes and the number of agent rows. Output: them. */
export function measure() {
  const box = (selector) => {
    const rect = document.querySelector(selector)?.getBoundingClientRect();
    return rect ? { top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right, width: rect.width, height: rect.height } : null;
  };
  const sizes = (element) => ({ scroll: element.scrollHeight, client: element.clientHeight, top: element.scrollTop });
  return {
    window: { width: window.innerWidth, height: window.innerHeight, scrollY: window.scrollY },
    document: sizes(document.scrollingElement),
    main: sizes(document.querySelector(".main")),
    panel: box(".pj-tab-panel"),
    master: box(".pj-master-live"),
    left: box(".ta-main"),
    side: box(".ta-side"),
    terminal: box(".ta-scroll > .tg-terminal"),
    team: sizes(document.querySelector(".ta-scroll")),
    banners: sizes(document.querySelector(".pj-banners")),
    rows: document.querySelectorAll(".ta-row").length,
  };
}

/** The middle of an element on screen, where a wheel turn lands on it. Input: the selector.
 * Output: the point. */
export function middle(selector) {
  const rect = document.querySelector(selector).getBoundingClientRect();
  return { x: Math.round(rect.left + rect.width / 2), y: Math.round(rect.top + rect.height / 2) };
}
