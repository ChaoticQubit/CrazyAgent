import { Terminal } from "@xterm/xterm";
import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { LiveMasterView, MasterArea } from "../../src/screens/project/live-master.tsx";
import { terminalOptions, typeKeys } from "../../src/screens/team/terminal-panel.tsx";

let root = null;
let terminal = null;
export const typed = [];

function page() {
  root?.unmount();
  root = null;
  terminal?.dispose();
  terminal = null;
  typed.length = 0;
  document.head.innerHTML = "";
  for (const text of [styles, projectStyles, teamStyles, xtermStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="pj pj-project"><div class="pj-tab-panel" id="panel"></div></div></main></div>';
  return document.getElementById("panel");
}

function data(master) {
  const setup = master === undefined ? undefined : {
    kind: "ok",
    value: { project_id: "alpha", name: "Alpha", about: "", tech_stack: [], platforms: [], research: [], pool: null, documents: [], folder: "/tmp/alpha", created_ms: 0, revision: 1,
      team: { master, master_tools: true, sub_harnesses: [] } },
  };
  return {
    id: "alpha", name: "Alpha", setup, research: undefined, context: undefined, pools: undefined, harnesses: undefined,
    assigned: null, now: 0, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: { result: undefined, at: 0 }, reloadTeam: () => {},
  };
}

const node = { id: "team-1", kind: "master", parent: null, label: "Master", harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: "team-1" };

/** Mounts the project's master area for a team plan whose master is the given harness, or
 * whose setup is still loading when it is undefined. */
export function mountArea(master) {
  const panel = page();
  root = createRoot(panel);
  flushSync(() => root.render(createElement(MasterArea, { data: data(master) })));
}

/** Mounts the live master view in one of its states: running with the master's terminal,
 * running before the terminal opens, starting, or refused with an engine code. */
export function mountView(state, code = "") {
  const panel = page();
  const live = state === "running" ? { kind: "running", node }
    : state === "waiting" ? { kind: "running", node: null }
    : state === "refused" ? { kind: "refused", code, copy: "The engine refused." }
    : { kind: state };
  root = createRoot(panel);
  flushSync(() => root.render(createElement(LiveMasterView, { data: data("codex"), harness: "codex", live, onRetry: () => {} })));
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
    text: document.getElementById("panel").textContent,
    viewport: window.innerHeight,
    area: area ? { top: Math.round(area.top), bottom: Math.round(area.bottom) } : null,
    box: box ? { top: Math.round(box.top), bottom: Math.round(box.bottom), height: Math.round(box.height) } : null,
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

/** Opens a real xterm whose keys go through the panel's key routing, and focuses it. */
export function mountKeys() {
  page().innerHTML = '<section class="tg-terminal"><div class="tg-terminal-screen" id="screen"></div></section>';
  terminal = new Terminal({ ...terminalOptions(), rows: 10, cols: 80 });
  terminal.open(document.getElementById("screen"));
  typeKeys(terminal, (data) => typed.push(data));
  terminal.focus();
}

/** The text the routed keys typed, in order. */
export function typedText() {
  return [...typed];
}
