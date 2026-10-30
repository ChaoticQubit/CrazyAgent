import { Terminal } from "@xterm/xterm";
import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { LiveMasterView, MasterArea } from "../../src/screens/project/live-master.tsx";
import { TerminalMenu, terminalOptions, typeKeys } from "../../src/screens/team/terminal-panel.tsx";
import { muteQueries } from "../../src/screens/team/terminal-io.ts";

let root = null;
let menuRoot = null;
let terminal = null;
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

function data(master, on) {
  const setup = master === undefined ? undefined : {
    kind: "ok",
    value: { project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research: [], pool: null, documents: [], folder: "/tmp/alpha", created_ms: 0, revision: 1,
      team: { master, master_tools: true, sub_harnesses: [] } },
  };
  return {
    id: "alpha", name: "Alpha", setup, research: undefined, context: undefined, pools: undefined, harnesses: undefined,
    assigned: null, now: 0, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: on === undefined ? { result: undefined, at: 0 } : { result: { kind: "ok", value: status(on) }, at: 1 },
    reloadTeam: () => { reloads += 1; },
  };
}

function status(on) {
  return { project_id: "alpha", keep_warm: false, paused: false, min_agents: 0, max_agents: 1, harnesses: [], agents: [], tasks: [], messages: [], plan: [], live_master: on, live_problem: null };
}

const node = { id: "team-1", kind: "master", parent: null, label: "Master", harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: "team-1" };

/** Mounts the project's master area for a team plan whose master is the given harness, or
 * whose setup is still loading when it is undefined, with the engine's live switch read as
 * `on` (true, false or null for never set), or the team not read yet when it is undefined. */
export function mountArea(master, on) {
  const panel = page();
  root = createRoot(panel);
  flushSync(() => root.render(createElement(MasterArea, { data: data(master, on) })));
}

/** Mounts the live master view in one of its states: running with the master's terminal,
 * running before the terminal opens, starting, refused with an engine code, or stopped with
 * the engine's reason when the code is given. */
export function mountView(state, code = "") {
  const panel = page();
  const live = state === "running" ? { kind: "running", node }
    : state === "waiting" ? { kind: "running", node: null }
    : state === "refused" ? { kind: "refused", code, copy: "The engine refused." }
    : state === "stopped" ? { kind: "stopped", problem: code || null }
    : { kind: state };
  root = createRoot(panel);
  flushSync(() => root.render(createElement(LiveMasterView, { data: data("codex"), harness: "codex", live, onStart: async () => { starts += 1; } })));
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

/** What the master area shows: which views are mounted, the buttons, and how much of the
 * area the terminal fills. */
export function shown() {
  const area = document.querySelector(".pj-master-live")?.getBoundingClientRect();
  const box = document.querySelector(".pj-master-live > .tg-terminal")?.getBoundingClientRect();
  const above = document.querySelector(".pj-master-live > .tg-terminal")?.previousElementSibling?.getBoundingClientRect();
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
