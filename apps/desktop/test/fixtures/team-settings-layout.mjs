import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import * as conversation from "../../src/screens/project/conversation.tsx";
import * as agents from "../../src/screens/project/agents.tsx";
import * as team from "../../src/screens/team/controls.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let root = null;
const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main"><div class="page"><div class="pj" id="mount"></div></div></main></div>';
const DRAWER = '<dialog class="drawer" open><div class="dialog-body pj" id="mount"></div></dialog>';
const SAVE_MS = 60;
const RECEIPT = { outcome: "applied", pending: false, rejection: null };
const entryOf = (id, name, extra = {}) => ({ id, name, status: { kind: "ready" }, status_label: "Ready", executable: null, headless: "", tools: { kind: "switchable" }, model_selection: false, account_kinds: [], team: { kind: "available" }, aliases: [], ...extra });
const HARNESSES = [entryOf("claude", "Claude Code", { aliases: ["claude-code"] }), entryOf("codex", "Codex")];
const POOLS = [{ pool: { id: "pool-a", name: "Pool A", members: [] }, members: [] }, { pool: { id: "pool-b", name: "Pool B", members: [] }, members: [] }];
const SETUP = {
  project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research: [], pool: "pool-a", revision: 4,
  team: { master: "codex", master_tools: true, sub_harnesses: [{ harness: "codex", agents: 2, tools: true }, { harness: "claude", agents: 3, tools: true }], min_agents: 1, max_agents: 6 },
};
const POLICY = {
  access: "full", network: "open", toolchains: [], ide_git_hooks: false, version: 3, networks: [],
  levels: [{ access: "sandboxed", label: "Sandboxed", unavailable: null }, { access: "own_branch", label: "Own branch", unavailable: null }, { access: "full", label: "Full access", unavailable: null }],
};
const server = (name) => ({ id: name, name, scope: { kind: "global" }, scope_label: "", spec: { kind: "stdio", command: name, args: [] }, runs: "", enabled: true,
  status: "approved", status_label: "Approved", problem: null, tools: [], checks: 0, secrets: [], on_here: null, version: 1 });
const SERVERS = [server("graphify"), server("context-mode")];
let sent = [];
let running = 0;
let most = 0;

/** Answers the app's engine calls as a quick engine does: the reads these screens make, and a
 * coordination save that takes a moment, counting how many run at once; any other command is
 * not in this build. Output: nothing; it installs the answers on the window. */
function engine() {
  sent = [];
  running = 0;
  most = 0;
  window.__TAURI_INTERNALS__ = {
    plugins: {},
    transformCallback: () => 0,
    async invoke(command, args) {
      sent.push({ command, args });
      if (command === "desktop_project_agent_policy") return POLICY;
      if (command === "desktop_plugin_servers") return SERVERS;
      if (command === "desktop_plugin_skills") return [];
      if (command === "desktop_team_coordination") {
        running += 1;
        most = Math.max(most, running);
        await new Promise((done) => setTimeout(done, SAVE_MS));
        running -= 1;
        return RECEIPT;
      }
      throw `Command ${command} not found`;
    },
  };
}

/** Mounts Agent settings in the app's real page column, the master's settings in the app's
 * drawer, the Agents tab at a route's section, or the Master settings button, styled by the
 * app's own CSS, over a quick fake engine. Inputs: that CSS, "page", "drawer", "agents" or
 * "button", the team status and the route's section. Output: false when the screen has no such
 * view, else true. */
export async function mount(css, view, status, section) {
  root?.unmount();
  engine();
  document.head.querySelector("style[data-app]")?.remove();
  const style = document.createElement("style");
  style.dataset.app = "";
  style.textContent = css;
  document.head.append(style);
  document.body.innerHTML = view === "drawer" ? DRAWER : PAGE;
  root = createRoot(document.getElementById("mount"));
  const data = {
    id: "alpha", name: "Alpha", now: 0, setup: { kind: "ok", value: SETUP }, harnesses: { kind: "ok", value: HARNESSES }, research: undefined, context: undefined,
    pools: { kind: "ok", value: POOLS }, assigned: POOLS[0], team: { result: { kind: "ok", value: status }, at: 0 },
    navigate() {}, notify() {}, reload() {}, reloadPools() {}, reloadTeam() {},
  };
  const views = {
    page: [team.TeamControls, { projectId: "alpha", status, reload() {}, navigate() {}, data }],
    drawer: [conversation.MasterSettings, { data }],
    agents: [agents.AgentsTab, { data, section: section ?? undefined }],
    button: [conversation.MasterSettingsButton, { data }],
  };
  const [component, props] = views[view];
  if (typeof component !== "function") return false;
  await act(async () => {
    root.render(createElement(component, props));
    await new Promise((done) => setTimeout(done, 0));
  });
  await settle(20);
  return true;
}

/** Lets the engine's answers and the renders they cause land. Input: how long to wait, in
 * milliseconds. Output: nothing. */
export async function settle(ms) {
  await act(async () => { await new Promise((done) => setTimeout(done, ms)); });
}

/** Clicks the element a selector finds whose text is the given words. Inputs: the selector and
 * the words. Output: whether such an element was there to click. */
export function click(selector, words) {
  const found = [...document.querySelectorAll(selector)].find((element) => element.textContent.trim() === words);
  found?.click();
  return found !== undefined;
}

/** What the engine was asked with one command and the most of them that ran at once. Input:
 * the command. Output: the arguments of each call in order and that most. */
export function sentTo(command) {
  return { args: sent.filter((call) => call.command === command).map((call) => call.args), most };
}

/** What the Agents tab and its drawer show now. Output: the Agents view choice, whether Agent
 * settings and the Team view's Stop all show, the focused element's id, and the open drawer's
 * title and group headings, or null. */
export function shown() {
  const drawer = document.querySelector("dialog.drawer[open]");
  return {
    view: document.querySelector('[aria-label="Agents view"] [aria-checked="true"]')?.textContent ?? null,
    settings: document.querySelector('section[aria-label="Agent settings"]') !== null,
    stopAll: [...document.querySelectorAll("button")].some((button) => button.textContent.trim() === "Stop all"),
    focused: document.activeElement?.id || null,
    drawer: drawer ? { title: drawer.querySelector(".dialog-head h2")?.textContent ?? null, groups: [...drawer.querySelectorAll(".set-group h3")].map((heading) => heading.textContent) } : null,
  };
}

/** Measures the mounted settings where they are drawn. Output: each group's heading and row
 * labels, the rows and parts that run past their group's panel, how far the scrolling area
 * scrolls sideways and the width the groups take. */
export function measure() {
  const groups = [...document.querySelectorAll(".set-group")];
  const past = groups.flatMap((group) => {
    const edge = group.getBoundingClientRect();
    return [...group.querySelectorAll("*")].filter((part) => {
      const box = part.getBoundingClientRect();
      return box.width > 1 && (box.right > edge.right + 0.5 || box.left < edge.left - 0.5);
    }).map((part) => part.closest(".set-row")?.dataset.setting ?? part.getAttribute("class") ?? part.tagName);
  });
  const scroller = document.querySelector(".main") ?? document.querySelector("dialog");
  return {
    groups: groups.map((group) => ({ title: group.querySelector("h2, h3")?.textContent ?? null, rows: [...group.querySelectorAll(".set-row")].map((row) => row.dataset.setting) })),
    past: [...new Set(past)],
    sideways: scroller.scrollWidth - scroller.clientWidth,
    width: Math.round(document.querySelector(".set-groups")?.getBoundingClientRect().width ?? 0),
  };
}

/** Says which setting row holds the focused element. Output: the row's label, or null. */
export function focused() {
  return document.activeElement?.closest(".set-row")?.dataset.setting ?? null;
}

/** Lets the page draw for a screenshot: the page and its scrolling area grow to their content.
 * Output: nothing. */
export function unclip() {
  for (const selector of ["html", "body", ".shell", ".main"]) {
    const element = document.querySelector(selector);
    if (element) Object.assign(element.style, { height: "auto", overflow: "visible" });
  }
}
