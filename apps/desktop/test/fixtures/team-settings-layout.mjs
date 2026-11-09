import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import * as conversation from "../../src/screens/project/conversation.tsx";
import * as agents from "../../src/screens/project/agents.tsx";
import * as team from "../../src/screens/team/controls.tsx";
import * as coordination from "../../src/screens/team/coordination.tsx";

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
let coordSaved = null;
let policy = POLICY;
let saveAnswer = null;
let render = null;
let poolAssigned = "pool-a";
const poolRecord = (id) => ({ kind: "pool", pool: { id, projects: poolAssigned === id ? ["alpha"] : [] } });

/** Answers the app's engine calls as a quick engine does: the reads these screens make, the
 * team read with the coordination settings saved since, an agent access save that the policy
 * read then shows, and a coordination save that takes a moment, counting how many run at once;
 * any other command is not in this build, unless `answerSaves` gave the team saves an answer.
 * Input: the team status. Output: nothing; it installs the answers on the window. */
function engine(status) {
  sent = [];
  running = 0;
  most = 0;
  coordSaved = null;
  policy = POLICY;
  saveAnswer = null;
  poolAssigned = "pool-a";
  window.__TAURI_INTERNALS__ = {
    plugins: {},
    transformCallback: () => 0,
    async invoke(command, args) {
      sent.push({ command, args });
      if (command === "desktop_project_agent_policy") return policy;
      if (command === "desktop_set_project_agent_policy") {
        policy = { ...policy, access: args.policy.access, version: policy.version + 1 };
        return RECEIPT;
      }
      if (command === "desktop_plugin_servers") return SERVERS;
      if (command === "desktop_plugin_skills") return [];
      if (command === "desktop_team_status") {
        return coordSaved === null ? status : { ...status, coordination: { settings: coordSaved, harnesses: status.coordination?.harnesses ?? [] } };
      }
      if (command === "desktop_team_coordination") {
        running += 1;
        most = Math.max(most, running);
        await new Promise((done) => setTimeout(done, SAVE_MS));
        running -= 1;
        if (saveAnswer !== null) return saveAnswer;
        coordSaved = { ...args.settings };
        return RECEIPT;
      }
      if (saveAnswer !== null) {
        if (command.startsWith("desktop_team_") || command === "desktop_update_project_setup") return saveAnswer;
        if (command === "desktop_open_project") return { fresh: true, project_id: "alpha" };
        if (command === "desktop_model_access_page") return { writable: true, ticket: "ticket", has_more: false, items: [poolRecord("pool-a"), poolRecord("pool-b")] };
        if (command === "desktop_edit_model_access") {
          poolAssigned = Object.values(args).find((value) => value?.kind === "assign_project_pool").pool;
          return saveAnswer;
        }
      }
      throw `Command ${command} not found`;
    },
  };
}

/** Gives every team save this answer until the next mount. Input: the receipt. Output: nothing. */
export function answerSaves(receipt) {
  saveAnswer = receipt;
}

/** Changes the coordination settings the fake engine holds, as the TUI or another window
 * would, while the mounted screen still shows its older team read. Input: the change. Output:
 * nothing. */
export function changedElsewhere(patch) {
  coordSaved = { ...(coordSaved ?? coordination.COORD_DEFAULTS), ...patch };
}

/** Mounts Agent settings in the app's real page column, the master's settings in the app's
 * drawer, the Agents tab at a route's section, or the Master settings button, styled by the
 * app's own CSS, over a quick fake engine. Inputs: that CSS, "page", "drawer", "agents" or
 * "button", the team status and the route's section. Output: false when the screen has no such
 * view, else true. */
export async function mount(css, view, status, section) {
  root?.unmount();
  engine(status);
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
  render = (next) => root.render(createElement(component, next === undefined ? props : { ...props, status: next }));
  await act(async () => {
    render();
    await new Promise((done) => setTimeout(done, 0));
  });
  await settle(20);
  return true;
}

/** Hands the mounted Agent settings a newer team read, as a reload or poll does. Input: the
 * team status. Output: nothing. */
export async function reread(status) {
  await act(async () => { render(status); });
  await settle(20);
}

function shownIn(found) {
  if (found.select) return found.select.value;
  if (found.radio) return found.radio.closest("[role=radiogroup]").querySelector('[aria-checked="true"]')?.textContent.trim() ?? null;
  return found.card.closest("[role=radiogroup]").querySelector("input:checked")?.value ?? null;
}

function controlOf(row) {
  const shell = document.querySelector(`[data-setting="${row}"]`);
  const select = shell?.querySelector("select");
  if (select) return { select };
  const radios = [...(shell?.querySelectorAll('[role="radio"]') ?? [])];
  if (radios.length > 0) return { radio: radios.find((radio) => radio.getAttribute("aria-checked") !== "true") };
  const card = shell?.querySelector('input[type="radio"]:not(:checked)');
  return card ? { card } : null;
}

/** Picks a value in one Agent settings row as the owner does and lets its save run, while the
 * row's team read stays as it was. Inputs: the row's label and, for a select, the value to pick;
 * without one it picks the first other value. Output: the value shown before, the one picked
 * and the one shown after, or null when the row has no other value. */
export async function pickOther(row, wanted) {
  const found = controlOf(row);
  if (found === null) return null;
  const before = shownIn(found);
  const other = found.select ? wanted ?? [...found.select.options].find((option) => !option.disabled && option.value !== before)?.value : null;
  if (found.select && other === undefined) return null;
  const picked = found.select ? other : found.radio ? found.radio.textContent.trim() : found.card.value;
  await act(async () => {
    if (found.select) {
      found.select.value = picked;
      found.select.dispatchEvent(new Event("change", { bubbles: true }));
    } else (found.radio ?? found.card).click();
  });
  await settle(SAVE_MS * 2);
  return { before, picked, shown: shownIn(controlOf(row) ?? found) };
}

/** Types a value into a text setting and presses its Save. Inputs: the input's id and the text.
 * Output: the input's text after the save ran. */
export async function saveText(id, text) {
  const input = document.getElementById(id);
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => { input.form.requestSubmit(); });
  await settle(SAVE_MS * 2);
  return document.getElementById(id).value;
}

/** Reads the value one select shows now. Input: its id. Output: the value, or null. */
export function selected(id) {
  return document.getElementById(id)?.value ?? null;
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

/** Presses a key on the element a selector finds, as the keyboard does on a focused control.
 * Inputs: the selector and the key's name. Output: whether such an element was there. */
export async function press(selector, key) {
  const found = document.querySelector(selector);
  await act(async () => { found?.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })); });
  return found !== null;
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

/** Measures where each shown row's help line sits against its label and control. Output: the
 * rows measured and the rows whose help runs under the control or away from the label. */
function helpBeside() {
  const rows = [...document.querySelectorAll(".set-row:not(.wide)")].filter((row) => row.offsetParent !== null);
  const measured = rows.flatMap((row) => {
    const label = row.querySelector(":scope > .set-label, :scope > .field > label, :scope > .switch-label");
    const control = row.querySelector(":scope > .segmented, :scope > .set-ctl, :scope > .field > .select");
    const help = row.querySelector(":scope > .tg-terminal-note:not(.set-ctl), :scope > .field > .helper");
    if (!label || !control || !help) return [];
    const [l, c, h] = [label, control, help].map((part) => part.getBoundingClientRect());
    return [{ row: row.dataset.setting, apart: h.right > c.left + 0.5 || Math.abs(h.left - l.left) > 0.5 || h.top < l.bottom - 0.5 }];
  });
  return { rows: measured.length, apart: measured.filter((row) => row.apart).map((row) => row.row) };
}

/** Measures the mounted settings where they are drawn. Output: each group's heading and row
 * labels, the rows and parts that run past their group's panel, how far the scrolling area
 * scrolls sideways, the width the groups take and where each row's help line sits. */
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
    help: helpBeside(),
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
