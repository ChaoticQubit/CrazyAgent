import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import frame from "../../src/screens/settings/settings.css?inline";
import { Settings } from "../../src/screens/settings/index.tsx";
import { SessionContext } from "../../src/state.ts";

export { contrast } from "./contrast.mjs";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main" id="main" tabindex="-1"><div class="page" id="mount"></div></main></div>';
const FAILURE = { code: "engine_failed" };
const APPLIED = { outcome: "applied", pending: false, rejection: null };

const harness = (id, name, status, patch = {}) => ({ id, name, status, status_label: "", executable: null, headless: `${id} --print`, tools: { kind: "switchable" }, model_selection: false, account_kinds: [], ...patch });
const CODEX = harness("codex", "Codex", { kind: "ready" }, { executable: "/Users/someone/.local/bin/codex", headless: "codex exec", model_selection: true, account_kinds: ["subscription", "api_key"] });
const CLAUDE = harness("claude", "Claude Code", { kind: "ready" }, { executable: "/opt/bin/claude", account_kinds: ["subscription"] });
const RCLI = harness("rcli", "Rcli", { kind: "limited", reason: "It has no switch for tools" }, { executable: "/opt/bin/rcli", tools: { kind: "limited", reason: "Its tools stay on" }, account_kinds: ["open_ai_compatible_url", "local"] });
const GEMINI = harness("gemini", "Gemini CLI", { kind: "not_installed" });
const HARNESSES = { ok: [CODEX, CLAUDE, RCLI, GEMINI], ready: [CODEX] };

const tool = (name, label, state) => ({ name, label, state });
const TOOLS = [tool("tmux", "tmux", { state: "found", version: "3.5a" }), tool("uv", "uv", { state: "installed", version: "0.6.1" }), tool("graphify", "Graphify command", { state: "found", version: "0.4.2" })];
const entry = (id, kind, group, on_by_default, state, patch = {}) => ({ id, kind, group, source: `github.com/example/${id}`, license: "MIT", on_by_default, state, hooks_refused: null, ...patch });
const HEADROOM = entry("headroom", "mcp_server", "optional", false, { state: "available" });
const PACK = [
  entry("graphify", "skill", "graph", true, { state: "on" }),
  entry("superpowers", "skill", "workflow", true, { state: "off" }, { hooks_refused: "the hook is outside its folder" }),
  entry("taste", "skill", "design", false, { state: "available" }),
  entry("caveman", "skill", "reply_style", true, { state: "failed", reason: "the download failed" }),
  entry("context-mode", "mcp_server", "context", true, { state: "on" }),
  HEADROOM,
  entry("oddity", "skill", "later", false, { state: "on" }),
];
const status = (patch = {}) => ({ dependencies: TOOLS, pack: PACK, running: false, notes: [], version: 7, ...patch });
const SETUP = {
  ok: status(),
  wanted: status({ dependencies: [TOOLS[0], tool("uv", "uv", { state: "missing" }), tool("graphify", "Graphify command", { state: "failed", reason: "uv could not fetch it" })], notes: ["Agent git needs the Command Line Tools."] }),
  running: status({ running: true, pack: PACK.map((item) => (item === HEADROOM ? { ...item, state: { state: "installing" } } : item)) }),
  bare: status({ dependencies: [] }),
};
const SKILLS = [{ id: "skill-superpowers", name: "superpowers", description: "How agents plan and build.", line: "" }];

const memory = (id, text, patch = {}) => ({ id, text, scope: "global", project_id: null, origin: "written", from_project_id: null, source_path: null, agent: null, ...patch });
const MEMORIES = [memory("m1", "The owner prefers Rust for engines."), memory("m2", "Harbor deploys on Fridays.", { scope: "project", project_id: "harbor", origin: "requested", agent: "agent-7" })];
const page = (items, next = null) => ({ items, next, agent_writes: "save", agent_scope: "project" });
const PAGES = { ok: page(MEMORIES), ask: page(MEMORIES), older: page(MEMORIES, 2), empty: page([]), long: page([memory("m1", "W".repeat(400))]) };
const SUGGESTION = { id: "entry-1", version: 3, text: "Competitors charge per seat.", status: "Provisional", human_reviewed: false };
const PROJECTS = [{ id: "harbor", name: "Harbor Notes" }];

let root = null;
let sent = [];
let told = [];
let writes = "save";
let reach = "project";

/** Lets the engine's answers and the renders they cause land. Input: the wait in
 * milliseconds. Output: nothing. */
export async function settle(ms) {
  await act(async () => { await new Promise((done) => setTimeout(done, ms)); });
}

function failing(command, state, list) {
  if (state === "error") throw FAILURE;
  if (state === "missing") throw `Command ${command} not found`;
  return state === "pending" ? new Promise(() => {}) : list[state];
}

function savedPage(scene, args) {
  if (args.before !== null) return { ...page([memory("m0", "An older memory.")]), agent_writes: writes, agent_scope: reach };
  const listed = failing("desktop_memories", scene.memories ?? "ok", PAGES);
  if (listed instanceof Promise) return listed;
  return { ...listed, items: listed.items.filter((item) => !args.search || item.text.includes(args.search)), agent_writes: writes, agent_scope: reach };
}

/** Answers one engine command for a scene. Inputs: the scene, the command and its arguments.
 * Output: the answer, a failure thrown, or a read that never lands. */
function answer(scene, command, args) {
  if (scene.refuse === command) throw FAILURE;
  if (scene.absent === command) throw `Command ${command} not found`;
  if (scene.hold === command) return new Promise(() => {});
  if (command === "desktop_harness_catalog") return failing(command, scene.harnesses ?? "ok", HARNESSES);
  if (command === "desktop_setup_status") return failing(command, scene.setup ?? "ok", SETUP);
  if (command === "desktop_plugin_skills") return SKILLS;
  if (command === "desktop_memories") return savedPage(scene, args);
  if (command === "desktop_project_memories") return args.after === null ? [SUGGESTION] : [];
  if (["desktop_set_pack_entry", "desktop_remove_pack_entry", "desktop_run_setup"].includes(command)) return SETUP.ok;
  if (command === "desktop_pick_folder") return "/Users/someone/notes";
  if (command === "desktop_import_memory") return { imported: 2, skipped: 1 };
  if (command === "desktop_set_agent_memory_writes") writes = args.writes;
  if (command === "desktop_set_agent_memory_scope") reach = args.scope;
  if (["desktop_set_agent_memory_writes", "desktop_set_agent_memory_scope", "desktop_add_memory", "desktop_delete_memory", "desktop_approve_memory", "desktop_review_memory", "desktop_promote_memory"].includes(command)) return APPLIED;
  throw `Command ${command} not found`;
}

/** Mounts the real Settings page on one tab against a fake engine. Inputs: the tab, the scene,
 * naming the harnesses, setup and memories reads, a command to refuse, leave out or never
 * answer and whether projects exist, and the theme. Output: nothing. */
export async function mount(tab, scene = {}, theme = "light") {
  sent = [];
  told = [];
  writes = scene.memories === "ask" ? "ask" : "save";
  reach = scene.reach === "agent" ? "agent" : "project";
  window.__TAURI_INTERNALS__ = {
    plugins: {},
    transformCallback: () => 0,
    async invoke(command, args) { sent.push({ command, args }); return answer(scene, command, args); },
  };
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, frame].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.style.margin = "0";
  document.body.innerHTML = PAGE;
  root = createRoot(document.getElementById("mount"));
  const session = { noteResult: (result) => result, showToast: (words) => { told.push(words); }, projects: { kind: "ok", value: scene.projects === "none" ? [] : PROJECTS } };
  await act(async () => {
    root.render(createElement(SessionContext.Provider, { value: session }, createElement(Settings, { tab })));
  });
  await settle(60);
}

const words = (node) => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
const firstText = (node) => [...(node?.childNodes ?? [])].find((part) => part.nodeType === Node.TEXT_NODE)?.textContent ?? words(node);

function plain(node) {
  if (!node) return "";
  const copy = node.cloneNode(true);
  copy.querySelectorAll('[aria-hidden="true"], .visually-hidden').forEach((part) => part.remove());
  return words(copy);
}

function nameOf(node) {
  const by = node.getAttribute("aria-labelledby");
  if (by) return words(document.getElementById(by));
  return node.getAttribute("aria-label") ?? (node.labels?.length ? words(node.labels[0]) : plain(node));
}

function rowOf(row) {
  const label = row.querySelector(".set-label, .field > label");
  const control = row.querySelector(".set-ctl, .field > .select");
  return {
    label: plain(label), help: words(row.querySelector(".set-help, .field > .helper")), control: plain(control), mark: words(row.querySelector(".set-changed")) || null,
    labelRight: label?.getBoundingClientRect().right ?? null, labelSpills: label ? label.scrollWidth > label.clientWidth : null, controlLeft: control?.getBoundingClientRect().left ?? null,
    buttons: [...row.querySelectorAll("button")].map(nameOf), expanded: row.querySelector("[aria-expanded]")?.getAttribute("aria-expanded") ?? null,
  };
}

/** Reads the tab's top line. Output: its heading, purpose, words and action names, or null. */
export function top() {
  const node = document.querySelector(".settings-top");
  if (!node) return null;
  return { heading: words(node.querySelector("h2")), purpose: words(node.querySelector("p")), says: words(node), actions: [...node.querySelectorAll("button, a")].map(words) };
}

/** Reads the tab's sections. Output: each one's heading, changed mark, purpose line, head
 * controls and rows. */
export function sections() {
  return [...document.querySelectorAll(".set-group")].map((group) => ({
    heading: firstText(group.querySelector("h3")).trim(), mark: words(group.querySelector("h3 .set-changed")) || null, about: words(group.querySelector(".set-head p")),
    actions: [...group.querySelectorAll(".set-head button, .set-head select, .set-head input")].map(nameOf), rows: [...group.querySelectorAll(".set-row")].map(rowOf),
  }));
}

/** Reads what the page says. Output: its words on one line. */
export function text() {
  return document.querySelector("#mount").innerText.replace(/\s+/g, " ").trim();
}

/** Reads the open dialog. Output: its words on one line, empty when none is open. */
export function dialog() {
  return (document.querySelector("dialog[open]")?.innerText ?? "").replace(/\s+/g, " ").trim();
}

/** The commands the page has sent. Output: their names. */
export function commands() {
  return sent.map((item) => item.command);
}

/** The arguments each send of one command carried. Input: the command. Output: the list. */
export function argsOf(command) {
  return sent.filter((item) => item.command === command).map((item) => item.args);
}

/** The toasts the page has shown. Output: their words. */
export function toasts() {
  return [...told];
}

/** Counts what a selector finds. Input: the selector. Output: the count. */
export function count(selector) {
  return document.querySelectorAll(selector).length;
}

/** Reads the tab's alert. Output: its words and whether it sits below the top line. */
export function alert() {
  const node = document.querySelector('#mount [role="alert"]');
  const head = document.querySelector(".settings-top");
  return { words: words(node), belowTop: Boolean(node && head && head.getBoundingClientRect().bottom <= node.getBoundingClientRect().top) };
}

/** The focused element's accessible name and id. Output: both. */
export function focused() {
  const node = document.activeElement;
  return { name: node ? nameOf(node) : "", id: node?.id ?? "" };
}

/** The screen centre of the element a selector finds with the given words, scrolled into
 * view. Inputs: the selector and the words, or null for the first match. Output: the point,
 * or null when nothing matches. */
export function boxOf(selector, wanted) {
  const found = [...document.querySelectorAll(selector)].find((element) => wanted === null || plain(element) === wanted);
  if (!found) return null;
  found.scrollIntoView({ block: "center" });
  const box = found.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}
