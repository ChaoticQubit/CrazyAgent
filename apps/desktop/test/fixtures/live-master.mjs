import { Terminal } from "@xterm/xterm";
import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { LiveMasterView, MasterArea } from "../../src/screens/project/live-master.tsx";
import { TerminalMenu, TerminalPanel, terminalOptions, typeKeys } from "../../src/screens/team/terminal-panel.tsx";
import { muteQueries } from "../../src/screens/team/terminal-io.ts";
import { teamStatus } from "./team-status.mjs";

let root = null;
let menuRoot = null;
let terminal = null;
let seen = null;
export const notes = [];
export const typed = [];
let starts = 0;
let reloads = 0;

function page() {
  root?.unmount();
  root = null;
  menuRoot?.unmount();
  menuRoot = null;
  notes.length = 0;
  terminal?.dispose();
  terminal = null;
  typed.length = 0;
  starts = 0;
  reloads = 0;
  document.head.innerHTML = "";
  for (const text of [styles, projectStyles, teamStyles, xtermStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="pj pj-project"><div class="pj-tab-panel" id="panel"></div></div></main></div>';
  return document.getElementById("panel");
}

/** Builds one ready harness catalog entry. Inputs: the id, the name and its aliases.
 * Output: the entry as the engine lists it. */
const entry = (id, name, aliases = []) => ({ id, name, status: { kind: "ready" }, status_label: "Ready", executable: null, headless: "", tools: { kind: "switchable" },
  model_selection: false, account_kinds: [], team: { kind: "available" }, aliases });

/** The engine's harness catalog as the project page reads it: Claude Code answers to its
 * alias "claude", as plans saved by older builds store it. */
const CATALOG = [entry("codex", "Codex"), entry("claude-code", "Claude Code", ["claude"]), entry("rcli", "RCLI")];

function data(master, on, team = undefined, catalog = undefined) {
  const setup = master === undefined ? undefined : {
    kind: "ok",
    value: { project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research: [], pool: null, documents: [], folder: "/tmp/alpha", created_ms: 0, revision: 1,
      team: { master, master_tools: true, sub_harnesses: [] } },
  };
  return {
    id: "alpha", name: "Alpha", setup, research: undefined, context: undefined, pools: undefined, harnesses: catalog === undefined ? undefined : { kind: "ok", value: catalog },
    assigned: null, now: 0, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: team !== undefined ? { result: team, at: 0 } : on === undefined ? { result: undefined, at: 0 } : { result: { kind: "ok", value: status(on) }, at: 1 },
    reloadTeam: () => { reloads += 1; },
  };
}

function status(on) {
  return teamStatus({ live_master: on, live_problem: null });
}

const node = { id: "team-1", kind: "master", parent: null, label: "Master", harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: "team-1" };

/** Mounts the project's master area for a team plan whose master is the given harness, or
 * whose setup is still loading when it is undefined, with the engine's live switch read as
 * `on` (true, false or null for never set), or the team not read yet when it is undefined,
 * and the harness catalog read unless `catalog` is false. */
export function mountArea(master, on, catalog = true) {
  const panel = page();
  root = createRoot(panel);
  flushSync(() => root.render(createElement(MasterArea, { data: data(master, on, undefined, catalog ? CATALOG : undefined) })));
}

/** Mounts the project's master area with no master known yet, the plan not read. Output:
 * none. */
export function mountAreaNoMaster() {
  const panel = page();
  root = createRoot(panel);
  const read = data("codex", true, undefined, CATALOG);
  read.setup = { kind: "not_ready" };
  flushSync(() => root.render(createElement(MasterArea, { data: read })));
}

/** Mounts the project's master area with the Live terminal setting Off. Inputs: the master
 * harness and the engine's run flag. Output: none. */
export function mountAreaOff(master, on) {
  const panel = page();
  root = createRoot(panel);
  const team = { kind: "ok", value: teamStatus({ live_master: on, live_problem: null, master_live_terminal: false }) };
  flushSync(() => root.render(createElement(MasterArea, { data: data(master, on, team, CATALOG) })));
}

/** Mounts the project's master area for a team stopped by Stop all. Inputs: the master
 * harness and the engine's live switch. Output: none. */
export function mountAreaStopped(master, on) {
  const panel = page();
  root = createRoot(panel);
  const team = { kind: "ok", value: teamStatus({ live_master: on, live_problem: null, paused: true }) };
  flushSync(() => root.render(createElement(MasterArea, { data: data(master, on, team, CATALOG) })));
}

/** Renders the mounted master area again with the engine's live switch read as `on`, as the
 * team read after an idle stop does. Inputs: the master harness, the switch and more fields
 * of the read. Output: none. */
export function readSwitch(master, on, patch = {}) {
  const read = data(master, on, undefined, CATALOG);
  read.team = { result: { kind: "ok", value: teamStatus({ live_master: on, live_problem: null, ...patch }) }, at: 1 };
  flushSync(() => root.render(createElement(MasterArea, { data: read })));
}

/** Mounts the live master view in one of its states: running with the master's terminal,
 * running before the terminal opens, starting, refused with an engine code, or stopped with
 * the engine's reason when the code is given, idle-stopped when the state says so, with the
 * team status read so far, if any. */
export function mountView(state, code = "", team = undefined) {
  const panel = page();
  const live = state === "running" ? { kind: "running", node }
    : state === "waiting" ? { kind: "running", node: null }
    : state === "refused" ? { kind: "refused", code, copy: "The engine refused." }
    : state === "stopped" ? { kind: "stopped", problem: code || null, idle: false }
    : state === "idle-stopped" ? { kind: "stopped", problem: code, idle: true }
    : { kind: state };
  root = createRoot(panel);
  flushSync(() => root.render(createElement(LiveMasterView, { data: data("codex", undefined, team), harness: "codex", live, onStart: async () => { starts += 1; } })));
}

/** How many times the mounted view asked to start the live master. */
export function startsAsked() {
  return starts;
}

/** How many team re-reads the master area asked for; each start of the live master ends
 * with one. */
export function teamReloads() {
  return reloads;
}

/** Lets the page's pending replies and renders finish, then says what the area shows. */
export async function settle() {
  for (let round = 0; round < 20; round += 1) await new Promise((done) => setTimeout(done, 10));
  return shown();
}

/** Clicks the button with this exact text and lets React render. */
export function click(text) {
  const button = [...document.querySelectorAll("button")].find((item) => item.textContent.trim() === text);
  if (!button) return false;
  flushSync(() => button.click());
  return true;
}

/** Puts keyboard focus on the button with this exact text, as Tab does. A mouse click in the
 * app's web view leaves focus where it was. Output: whether the button holds focus. */
export function focusButton(text) {
  const button = [...document.querySelectorAll("button")].find((item) => item.textContent.trim() === text);
  button?.focus();
  return button !== undefined && document.activeElement === button;
}

/** Leaves focus on the page, where a mouse click in the app's web view finds it. Output:
 * whether the page holds focus. */
export function leaveFocus() {
  document.activeElement?.blur();
  return document.activeElement === document.body;
}

/** Puts keyboard focus on the drawer's Live terminal switch, as Tab does. Output: whether the
 * switch holds focus. */
export function focusSwitch() {
  const field = document.getElementById("tg-live-terminal-alpha");
  field?.focus();
  return field !== null && document.activeElement === field;
}

/** Reads the drawer's Live terminal row. Output: the switch's aria-disabled value and the
 * row's text. */
export function liveTerminalRow() {
  const field = document.getElementById("tg-live-terminal-alpha");
  return { disabled: field?.getAttribute("aria-disabled") ?? null, text: field?.closest(".set-row")?.textContent ?? "" };
}

/** Where the Master settings drawer and keyboard focus are. Output: whether the drawer is open
 * and the focused element's id and text. */
export function focusState() {
  const active = document.activeElement;
  return { drawer: document.querySelector("dialog.drawer")?.open === true, id: active?.id ?? "", text: active?.textContent?.trim() ?? "" };
}

/** Closes the Master settings drawer with its Close button. Output: whether one was open. */
export function closeDrawer() {
  const close = document.querySelector('dialog.drawer [aria-label="Close"]');
  if (!close) return false;
  flushSync(() => close.click());
  return true;
}

/** What the master area shows: which views are mounted, the buttons, how much of the area
 * the terminal fills below the Sessions bar, and how far in from the console's edges the
 * head's title and the Sessions bar's text and last button sit. */
export function shown() {
  const area = document.querySelector(".pj-master-live")?.getBoundingClientRect();
  const box = document.querySelector(".pj-master-live > .tg-terminal")?.getBoundingClientRect();
  const above = document.querySelector(".pj-master-live > .tg-terminal")?.previousElementSibling?.getBoundingClientRect();
  const sessions = document.querySelector(".pj-master-live > .pj-sessions")?.getBoundingClientRect();
  const head = document.querySelector(".pj-master-live > .pj-console-head");
  const title = head?.querySelector(".pj-console-title")?.getBoundingClientRect();
  const said = document.querySelector('.pj-master-live > .pj-sessions [data-testid="master-live-session"]')?.getBoundingClientRect();
  const last = document.querySelector('.pj-master-live > .pj-sessions [data-testid="sessions-toggle"]')?.getBoundingClientRect();
  const edge = document.querySelector(".pj-master-live")?.getBoundingClientRect();
  return {
    live: Boolean(document.querySelector(".pj-master-live")),
    terminal: Boolean(document.querySelector(".tg-terminal")),
    terminalLabel: document.querySelector(".tg-terminal")?.getAttribute("aria-label") ?? null,
    close: Boolean(document.querySelector('[aria-label="Close terminal"]')),
    conversation: Boolean(document.querySelector(".pj-convo")),
    console: Boolean(document.querySelector(".pj-convo > .cs")),
    prompt: document.querySelector(".cs textarea")?.value ?? null,
    status: document.querySelector(".cs-status")?.textContent ?? null,
    owner: [...document.querySelectorAll(".cs-owner")].map((line) => line.textContent),
    buttons: [...document.querySelectorAll("button")].map((item) => item.textContent.trim()).filter(Boolean),
    titles: Object.fromEntries([...document.querySelectorAll("button[title]")].map((item) => [item.textContent.trim(), item.title])),
    state: document.querySelector(".pj-console-state")?.textContent ?? null,
    text: document.getElementById("panel").textContent,
    viewport: window.innerHeight,
    area: area ? { top: Math.round(area.top), bottom: Math.round(area.bottom) } : null,
    box: box ? { top: Math.round(box.top), bottom: Math.round(box.bottom), height: Math.round(box.height) } : null,
    above: above ? Math.round(above.bottom) : null,
    sessions: sessions ? { top: Math.round(sessions.top), bottom: Math.round(sessions.bottom) } : null,
    inset: head && title && said && last && edge ? {
      head: Math.round(title.left - edge.left),
      bar: Math.round(said.left - edge.left),
      headEnd: Math.round(parseFloat(getComputedStyle(head).paddingRight)),
      barEnd: Math.round(edge.right - last.right - parseFloat(getComputedStyle(document.querySelector(".pj-master-live")).borderRightWidth)),
    } : null,
  };
}

/** Types into the console prompt and presses Enter, as the owner does. */
export function typeAndEnter(text) {
  const field = document.querySelector(".cs textarea");
  const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
  flushSync(() => {
    set.call(field, text);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
  flushSync(() => field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", bubbles: true, cancelable: true })));
}

/** Opens a real xterm set up as the panel sets it up: queries muted and keys going through
 * the panel's key routing for a Mac or not, as `mac` says, and focuses it. */
export function mountKeys(mac) {
  page().innerHTML = '<section class="tg-terminal"><div class="tg-terminal-screen" id="screen"></div></section>';
  terminal = new Terminal({ ...terminalOptions(), rows: 10, cols: 80 });
  muteQueries(terminal);
  terminal.open(document.getElementById("screen"));
  typeKeys(terminal, (data) => typed.push(data), mac);
  terminal.focus();
}

/** Writes agent output into the terminal and waits until xterm parsed it. */
export function write(text) {
  return new Promise((done) => terminal.write(text, done));
}

/** Pastes clipboard text into the focused terminal as the browser does for Cmd+V. */
export function paste(text) {
  const data = new DataTransfer();
  data.setData("text/plain", text);
  terminal.textarea.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
}

/** Selects everything in the terminal, as a drag over all of it does. */
export function selectAll() {
  terminal.selectAll();
}

/** The text xterm holds as the selection. */
export function selection() {
  return terminal.getSelection();
}

/** Clears the terminal's selection. */
export function clearSelection() {
  terminal.clearSelection();
}

/** Whether the panel's xterm settings let a right-click replace the selection with the word
 * under the pointer. Output: the setting as `terminalOptions` gives it. */
export function rightClickSelectsWord() {
  return terminalOptions().rightClickSelectsWord;
}

/** The text the routed keys typed, in order. */
export function typedText() {
  return [...typed];
}

/** Opens a real xterm as `mountKeys` does, in a panel no bigger than the terminal, as the
 * panel's fit makes it, with the panel's right-click menu on its screen, opened from the
 * click as the panel opens it. */
export function mountMenu(mac) {
  mountKeys(mac);
  const section = document.querySelector(".tg-terminal");
  Object.assign(section.style, { width: "fit-content", height: "fit-content" });
  const holder = document.createElement("div");
  section.append(holder);
  menuRoot = createRoot(holder);
  const close = (note) => {
    notes.push(note);
    menuRoot.render(null);
  };
  document.getElementById("screen").addEventListener("contextmenu", (event) => {
    event.preventDefault();
    flushSync(() => menuRoot.render(createElement(TerminalMenu, { terminal, at: { x: event.clientX, y: event.clientY, selection: terminal.getSelection() }, onDone: close })));
  });
}

/** The menu as the owner sees it: its items, which one or whether the terminal has focus,
 * and the menu, panel and screen boxes in page coordinates. */
export function menuView() {
  const box = (element) => {
    if (!element) return null;
    const rect = element.getBoundingClientRect();
    return { left: Math.round(rect.left), top: Math.round(rect.top), right: Math.round(rect.right), bottom: Math.round(rect.bottom) };
  };
  const menu = document.querySelector(".tg-terminal-menu");
  const active = document.activeElement;
  return {
    open: Boolean(menu),
    items: [...document.querySelectorAll(".tg-terminal-menu [role=menuitem]")].map((item) => ({ text: item.textContent.trim(), disabled: item.disabled, ...box(item) })),
    focus: active === terminal.textarea ? "terminal" : active?.closest(".tg-terminal-menu") ? active.textContent.trim() : active?.tagName ?? null,
    menu: box(menu),
    panel: box(document.querySelector(".tg-terminal")),
    screen: box(document.querySelector("#screen .xterm-screen")),
    notes: [...notes],
  };
}

/** Mounts the real agent terminal panel for a team node whose terminal has the given name,
 * keyed by the node as the team page keys it. The page has no engine bridge, so the real
 * xterm opens and the attach then fails. */
export function mountPanel(name) {
  root = createRoot(page());
  seen = null;
  retarget(name);
}

/** Renders the mounted panel again for the same node with this terminal, as a team poll
 * does when the node's terminal changes or stays the same. */
export function retarget(name) {
  flushSync(() => root.render(createElement(TerminalPanel, { key: node.id, node: { ...node, terminal: name } })));
}

/** Waits until the panel's screen holds an opened xterm terminal other than the one this
 * last saw, whether or not the earlier one is still in the page, and remembers it. */
export async function newTerminal() {
  const fresh = () => [...document.querySelectorAll(".tg-terminal-screen .xterm")].find((element) => element !== seen);
  for (let waited = 0; !fresh(); waited += 10) {
    if (waited > 3000) throw new Error("no new terminal opened");
    await new Promise((done) => setTimeout(done, 10));
  }
  seen = fresh();
}

/** Right-clicks the middle of the panel's terminal screen, as the owner does. */
export function rightClick() {
  const screen = document.querySelector(".tg-terminal-screen");
  const box = screen.getBoundingClientRect();
  flushSync(() => screen.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 })));
}

/** Whether the panel's right-click menu is open and whether focus is on one of its items. */
export function panelView() {
  return {
    menu: Boolean(document.querySelector(".tg-terminal-menu")),
    menuFocus: Boolean(document.activeElement?.closest(".tg-terminal-menu")),
  };
}
