import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import intake from "../../src/screens/intake/intake.css?inline";
import graph from "../../src/screens/team/team-graph.css?inline";
import frame from "../../src/screens/settings/settings.css?inline";
import { Settings } from "../../src/screens/settings/index.tsx";
import { updateStore } from "../../src/screens/settings/updates-state.ts";
import { SessionContext } from "../../src/state.ts";

export { contrast } from "./contrast.mjs";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main" id="main" tabindex="-1"><div class="page" id="mount"></div></main></div>';
const FAILURE = { code: "engine_failed" };
const HOME_PATH = "/Users/mira/Projects";
const SHIPPED = { question: true, finished: true, failed: true, change_set_ready: true, progress: false, hourly_cap: 30 };
const note = (id, project) => ({ id, project_id: project, kind: "escalation", text: `Note ${id}`, created_ms: id, seen_ms: null });
const NOTES = [note(3, "alpha"), note(2, "alpha"), note(1, "gone")];
const FOUND = { version: "0.3.0", current_version: "0.2.0", notes: "Faster startup." };
const CHECKED = { kind: "check_done", update: FOUND, at: 1 };
const INSTALLING = [CHECKED, { kind: "install_started" }, { kind: "progress", progress: { kind: "downloading", downloaded: 5, total: 10 } }];
const PHASES = {
  current: [{ kind: "check_done", update: null, at: 1 }],
  available: [CHECKED],
  installing: INSTALLING,
  installed: [...INSTALLING, { kind: "install_done", engineStarted: false }],
  failed: [{ kind: "check_started" }, { kind: "check_failed", code: "update_download_failed" }],
};
let root = null;

/** Lets the engine's answers and the renders they cause land. Input: the wait in
 * milliseconds. Output: nothing. */
export async function settle(ms) {
  await act(async () => { await new Promise((done) => setTimeout(done, ms)); });
}

/** Answers one read for a scene. Inputs: the state the scene names, the command and the
 * value of a read that works. Output: the value, a failure thrown, or a read that never lands. */
function read(state, command, value) {
  if (state === "error") throw FAILURE;
  if (state === "missing") throw `Command ${command} not found`;
  return state === "pending" ? new Promise(() => {}) : value;
}

/** Answers one engine command for a scene. Inputs: the scene, the command and its arguments.
 * Output: the answer, or a failure thrown. */
function answer(scene, command, args) {
  switch (command) {
    case "desktop_projects_home": return read(scene.home, command, { path: scene.home === "none" ? null : HOME_PATH, watching: scene.home !== "cold" });
    case "desktop_pick_folder": return scene.pick ?? "/Users/mira/Work";
    case "desktop_folder_size": return { bytes: 3 * 1024 ** 3 };
    case "desktop_sandbox_settings": return read(scene.sandbox, command, { linux_sockets: scene.stored ?? "tcp_only" });
    case "desktop_set_sandbox_settings": throw FAILURE;
    case "desktop_cleanup_settings": return read(scene.finished, command, { mode: "after_days", days: 1 });
    case "desktop_set_cleanup_settings": throw FAILURE;
    case "desktop_quit_settings": return read(scene.quit, command, { on_quit: "keep_running", left_running: "after_hours", hours: 24 });
    case "desktop_set_quit_settings": throw FAILURE;
    case "desktop_master_step_settings": return read(scene.master, command, { limit: scene.limit ?? "no_limit" });
    case "desktop_set_master_step_settings": throw FAILURE;
    case "desktop_team_events": return read(scene.events, command, { rule: null, rule_version: 0, effective: SHIPPED, notifications: scene.events === "none" ? [] : NOTES, unseen: 3, projects: [] });
    case "desktop_system_notifications": return "available";
    case "desktop_update_settings": return read(scene.updates, command, { current_version: "0.2.0", channel: scene.channel ?? "stable" });
    case "desktop_engine_versions": return { engine: "0.2.0", app: "0.2.0", message: scene.engine === "differs" ? "The engine is on another version." : null };
    case "desktop_set_update_channel": throw FAILURE;
    case "desktop_link_cli": throw FAILURE;
    default: throw `Command ${command} not found`;
  }
}

/** Mounts the real Settings page on one tab against a fake engine. Inputs: the tab, the scene
 * naming each read's state, and the theme. Output: nothing. */
export async function mount(tab, scene = {}, theme = "light") {
  window.__TAURI_INTERNALS__ = { plugins: {}, transformCallback: () => 0, async invoke(command, args) { return answer(scene, command, args); } };
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, intake, graph, frame].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.style.margin = "0";
  document.body.innerHTML = PAGE;
  root = createRoot(document.getElementById("mount"));
  const session = {
    projects: { kind: "ok", value: [{ id: "alpha", name: "Alpha", repository_count: 0 }] },
    noteResult: (result) => result, showToast: () => {}, refreshProjects: async () => {},
  };
  await act(async () => { root.render(createElement(SessionContext.Provider, { value: session }, createElement(Settings, { tab }))); });
  await settle(60);
}

/** Moves the real update store to a phase, as a check or an install does. Input: the phase's
 * name. Output: nothing. */
export async function phase(name) {
  await act(async () => { for (const event of PHASES[name]) updateStore.dispatch(event); });
  await settle(20);
}

const words = (node) => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
const nameOf = (node) => node.getAttribute("aria-label") ?? (words(node) || node.value || "");
const firstText = (node) => [...(node?.childNodes ?? [])].find((part) => part.nodeType === Node.TEXT_NODE)?.textContent ?? words(node);

/** Reads the tab's top line. Output: its heading, purpose and action names, or null. */
export function top() {
  const node = document.querySelector(".settings-top");
  if (!node) return null;
  return { heading: words(node.querySelector("h2")), purpose: words(node.querySelector("p")), actions: [...node.querySelectorAll("button, a")].map(words) };
}

/** Reads the tab's sections. Output: each one's heading, purpose line, buttons in its head and
 * rows with their label, changed mark, help, edges and button words. */
export function sections() {
  return [...document.querySelectorAll(".set-group")].map((group) => ({
    heading: firstText(group.querySelector("h3")).trim(), about: words(group.querySelector(".set-head p")),
    head: [...group.querySelectorAll(".set-head button")].map(words),
    headRight: group.querySelector(".set-head button")?.getBoundingClientRect().left ?? null,
    titleRight: group.querySelector(".set-head h3")?.getBoundingClientRect().left ?? null,
    rows: [...group.querySelectorAll(".set-row")].map((row) => {
      const label = row.querySelector(".set-label").getBoundingClientRect();
      const control = row.querySelector(".set-ctl")?.getBoundingClientRect() ?? null;
      return {
        label: firstText(row.querySelector(".set-label")).trim(), mark: words(row.querySelector(".set-changed")) || null, help: words(row.querySelector(".set-help")),
        labelRight: label.right, labelBottom: label.bottom, controlLeft: control?.left ?? null, controlTop: control?.top ?? null,
        visible: [...row.querySelectorAll("button")].map(words), radios: row.querySelectorAll('input[type="radio"], [role="radio"]').length,
      };
    }),
  }));
}

/** Reads what the page says. Output: its words on one line. */
export function text() {
  return document.querySelector("#mount").innerText.replace(/\s+/g, " ").trim();
}

/** Reads the tab's alert. Output: its words and whether it sits below the top line. */
export function alert() {
  const node = document.querySelector('#mount [role="alert"]');
  const head = document.querySelector(".settings-top");
  return { words: words(node), belowTop: Boolean(node && head && head.getBoundingClientRect().bottom <= node.getBoundingClientRect().top) };
}

/** Reads the open dialog. Output: its words, empty with none open. */
export function dialog() {
  return words(document.querySelector('[role="dialog"], dialog[open]'));
}

/** The focused element's accessible name. Output: the name. */
export function focused() {
  const node = document.activeElement;
  return node ? nameOf(node) : "";
}

/** The screen centre of the element a selector finds with the given words, scrolled into
 * view. Inputs: the selector and the words, or null for the first match. Output: the point,
 * or null when nothing matches. */
export function boxOf(selector, wanted = null) {
  const node = [...document.querySelectorAll(selector)].find((item) => wanted === null || words(item) === wanted) ?? null;
  if (!node) return null;
  node.scrollIntoView({ block: "center" });
  const box = node.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}
