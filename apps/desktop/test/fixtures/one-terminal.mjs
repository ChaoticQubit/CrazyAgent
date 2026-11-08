import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { AgentsTab } from "../../src/screens/project/agents.tsx";
import { teamStatus } from "./team-status.mjs";

let root = null;
const engine = { serial: 0, clients: new Map(), attaches: [], detaches: [], cancels: [], notes: [], callbacks: new Map(), next: new Map(), delayMs: 0, graph: "ok", release: () => {} };

/** One team task the team status lists, running on Ada. Input: its id. Output: the task. */
export function runningTask(id) {
  return {
    id, source: "master", harness: "codex", tools: true, prompt: "Calendar API endpoints", status: "running", status_label: "Running", agent: 1,
    attempts: 1, result: null, problem: null, delivered: false, output_bytes: 0, created_ms: 0, started_ms: 0, finished_ms: null,
  };
}

const node = (id, kind, extra = {}) => ({
  id, kind, parent: null, label: id, harness: "codex", status: "working", status_label: "Working", account: null, terminal: null, ...extra,
});

/** The team the engine reports: a master leading three sub-agents, each with its own terminal. */
const TEAM = {
  project_id: "alpha",
  nodes: [
    node("m", "master", { label: "Master", status: "idle", terminal: "team-9", agent: 9 }),
    node("team-1", "sub", { parent: "m", label: "Ada", terminal: "team-1", agent: 1 }),
    node("team-2", "sub", { parent: "m", label: "Brook", terminal: "team-2", agent: 2 }),
    node("team-3", "sub", { parent: "m", label: "Cove", terminal: "team-3", agent: 3 }),
  ],
  edges: [],
};

/** The owner's Questions inbox: Ada waits on a permission prompt in its terminal, and so does
 * Dune, an agent the graph does not draw. */
const INBOX = {
  project_id: "alpha", questions: [], more: 0,
  waiting: [
    { agent: 1, label: "Ada", waiting: "permission", since_ms: 0, terminal: "team-1" },
    { agent: 5, label: "Dune", waiting: "question", since_ms: 0, terminal: "team-5" },
  ],
};

/** Sends one message down a terminal channel as the bridge does, in order. Inputs: the
 * channel id and the message. */
function deliver(channel, message) {
  const callback = engine.callbacks.get(channel);
  if (!callback) return;
  const index = engine.next.get(channel) ?? 0;
  engine.next.set(channel, index + 1);
  callback({ index, message });
}

/** The team graph read the engine gives for a graph mode: the team ("ok"), a team with no
 * agents ("empty"), a read that never answers ("pending"), the team once `release` is called
 * ("late"), a failed read ("error") or a build without the graph ("not_ready"). Input: the
 * mode. Output: the read's answer. */
function graphRead(mode) {
  if (mode === "pending") return new Promise(() => {});
  if (mode === "late") return new Promise((done) => { engine.release = () => done(TEAM); });
  if (mode === "error") return Promise.reject({ code: "engine_unreachable", message: "The engine isn't running." });
  if (mode === "not_ready") return Promise.reject("Command desktop_team_graph not found");
  return Promise.resolve(mode === "empty" ? { ...TEAM, nodes: [] } : TEAM);
}

/** Answers the bridge calls the Agents tab and its terminal panels make: the team graph as
 * its mode reads, the Questions inbox, and terminal attach and detach, which the engine counts
 * as attached terminal clients. An attach answers after `delayMs`, as the engine does once
 * tmux attached, and its channel then says attached. Every other call is refused, as an
 * engine without it would. */
function bridge() {
  window.__TAURI_INTERNALS__ = {
    invoke: async (command, args) => {
      if (command === "desktop_team_graph") return graphRead(engine.graph);
      if (command === "desktop_coord_questions") return INBOX;
      if (command === "desktop_terminal_attach") {
        engine.attaches.push(args.agent);
        await new Promise((done) => setTimeout(done, engine.delayMs));
        engine.serial += 1;
        const serial = engine.serial;
        engine.clients.set(serial, args.agent);
        deliver(args.output.id, { kind: "attached", cols: 80, rows: 24 });
        return serial;
      }
      if (command === "desktop_terminal_detach") {
        engine.detaches.push(args.agent);
        engine.clients.delete(args.serial);
        return null;
      }
      if (command === "desktop_terminal_resize") return null;
      if (command === "desktop_team_cancel") {
        engine.cancels.push(args.task);
        return { generation: args.generation, outcome: "applied", pending: false, rejection: null };
      }
      throw { code: "unknown", message: command };
    },
    transformCallback: (callback) => {
      const id = engine.callbacks.size + 1;
      engine.callbacks.set(id, callback);
      return id;
    },
  };
}

/** Mounts the Agents tab of project alpha on a fresh page, as the project page shows it, with
 * Pool A assigned, the engine answering each terminal attach after the given delay, the team
 * graph read as the given mode and the team status listing the given tasks, with any other
 * status fields changed. Inputs: the delay in ms, the graph mode, the tasks and the changes. */
export function mount(delayMs = 0, graph = "ok", tasks = [], patch = {}) {
  root?.unmount();
  Object.assign(engine, { serial: 0, clients: new Map(), attaches: [], detaches: [], cancels: [], notes: [], callbacks: new Map(), next: new Map(), delayMs, graph });
  bridge();
  document.head.innerHTML = "";
  for (const text of [styles, projectStyles, teamStyles, xtermStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="pj pj-project"><div class="pj-tab-panel" id="panel"></div></div></main></div>';
  root = createRoot(document.getElementById("panel"));
  const data = {
    id: "alpha", name: "Alpha", setup: undefined, research: undefined, context: undefined, pools: undefined, harnesses: undefined,
    assigned: { pool: { id: "pool-a", name: "Pool A", members: [] } }, now: 0, navigate: () => {}, notify: (note) => engine.notes.push(note), reload: () => {}, reloadPools: () => {},
    team: { result: { kind: "ok", value: teamStatus({ tasks, ...patch }) }, at: 1 }, reloadTeam: () => {},
  };
  flushSync(() => root.render(createElement(AgentsTab, { data })));
}

/** Waits until a check holds or the time runs out. Inputs: the check and the limit in ms.
 * Output: whether it held. */
export async function until(check, limitMs = 5000) {
  const end = Date.now() + limitMs;
  while (Date.now() < end) {
    if (check()) return true;
    await new Promise((done) => setTimeout(done, 20));
  }
  return check();
}

/** Clicks the graph card of the agent with the given name. Input: the name. Output: whether
 * the card was there. */
export function click(name) {
  const card = [...document.querySelectorAll(".tg-card-open")].find((button) => button.querySelector(".tg-card-title")?.textContent.includes(name));
  if (!card) return false;
  card.click();
  return true;
}

/** Clicks Open terminal on the agent's row under "Waiting on you in their terminal", opening
 * the drawer behind the toolbar's "N ask you" first when the rows are not on the page. Input:
 * the name. Output: whether the row and its button were there. */
export async function openWaiting(name) {
  const list = '[aria-label="Waiting on you in their terminal"]';
  if (!document.querySelector(list)) {
    document.querySelector(".tg-asks")?.click();
    await until(() => document.querySelector(list) !== null);
  }
  const rows = document.querySelector(list);
  const row = [...(rows?.querySelectorAll("li") ?? [])].find((item) => item.textContent.includes(name));
  const button = [...(row?.querySelectorAll("button") ?? [])].find((item) => item.textContent.includes("Open terminal"));
  if (!button) return false;
  button.click();
  return true;
}

/** Answers the team graph read held by the "late" mode, as the engine does once it reads. */
export function release() {
  engine.release();
}

/** Types a draft into the open terminal panel's message box, as the owner does. Input: the
 * text. Output: whether the box was there. */
export function type(text) {
  const box = document.querySelector(".tg-composer-input");
  if (!box) return false;
  Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(box, text);
  box.dispatchEvent(new Event("input", { bubbles: true }));
  return true;
}

/** Clicks the button named `name`, by its text or its label, inside the first element the
 * selector finds. Inputs: the selector and the name. Output: whether the button was there. */
export function press(selector, name) {
  const scope = document.querySelector(selector);
  const button = [...(scope?.querySelectorAll("button") ?? [])].find((item) => item.textContent.trim() === name || item.getAttribute("aria-label") === name);
  if (!button) return false;
  button.click();
  return true;
}

/** What the owner sees around the panel: the names of the graph head's own buttons in order,
 * where the cursor is, the open dialog's title and text, the pause calls the engine took and
 * the notes the page left. */
export function around() {
  const dialog = document.querySelector("dialog[open]");
  return {
    head: [...(document.querySelector(".tg-head")?.querySelectorAll(":scope > button") ?? [])].map((item) => item.textContent.trim()),
    headGaps: [...(document.querySelector(".tg-head")?.querySelectorAll(":scope > button") ?? [])].map((item, index, all) =>
      index === 0 ? 0 : Math.round(item.getBoundingClientRect().left - all[index - 1].getBoundingClientRect().right)),
    focused: document.activeElement?.getAttribute("placeholder") ?? document.activeElement?.tagName ?? null,
    dialog: dialog ? { title: dialog.querySelector("h2")?.textContent ?? "", text: dialog.textContent } : null,
    cancels: [...engine.cancels],
    notes: [...engine.notes],
  };
}

/** What the page and the engine show now: the terminal panels and their names, the xterm
 * views on the page, the panels whose terminal is live, and the engine's attached terminal
 * clients with every attach and detach. */
export function shown() {
  const panels = [...document.querySelectorAll("section.tg-terminal")];
  return {
    panels: panels.map((panel) => panel.getAttribute("aria-label")),
    draft: document.querySelector(".tg-composer-input")?.value ?? null,
    xterms: document.querySelectorAll(".xterm").length,
    live: panels.filter((panel) => panel.querySelector(".tg-terminal-note")).length,
    clients: [...engine.clients.values()],
    attaches: [...engine.attaches],
    detaches: [...engine.detaches],
  };
}

/** The Agents toolbar as the owner reads it, left to right: every button and the team line
 * above the graph, outside dialogs, with how many rows they take, the space between the
 * lowest of them and the graph, and whether a Questions for you section shows on the page. */
export function toolbar() {
  const split = document.querySelector(".tg-split");
  const above = (item) => split !== null && (item.compareDocumentPosition(split) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
  const items = [...document.querySelectorAll("#panel button, #panel .tg-summary")]
    .filter((item) => above(item) && item.offsetParent !== null && !item.closest("dialog") && !item.parentElement.closest(".tg-summary"));
  const boxes = items.map((item) => item.getBoundingClientRect());
  const middles = boxes.map((box) => box.top + box.height / 2).sort((a, b) => a - b);
  const rows = middles.filter((middle, index) => index === 0 || middle - middles[index - 1] > 8).length;
  return {
    names: items.map((item) => item.textContent.replace(/\s+/gu, " ").trim() || item.getAttribute("aria-label")),
    rows,
    gap: split === null || boxes.length === 0 ? null : Math.round(split.getBoundingClientRect().top - Math.max(...boxes.map((box) => box.bottom))),
    questions: [...document.querySelectorAll("#panel section")].some((item) => !item.closest("dialog") && /^Questions for you/u.test(item.getAttribute("aria-label") ?? item.querySelector("h3")?.textContent ?? "")),
  };
}

/** The Team view's folds under the graph and where each team part sits: Stop all, the Hand a
 * task form, the team table, the task list, the wake rules and the Usage card, each as the
 * label of the fold it is in, "page" when outside every fold, or null when it is not on the
 * page. */
export function folds() {
  const heading = (text) => [...document.querySelectorAll("#panel h2, #panel h3, #panel h4")].find((item) => item.textContent.trim() === text) ?? null;
  const parts = {
    "Stop all": [...document.querySelectorAll("#panel button")].find((item) => item.textContent.trim() === "Stop all") ?? null,
    "Hand a task": document.querySelector("#panel form.tg-hand"),
    "Team agents": document.querySelector("#pj-team-title"),
    "Team tasks": document.querySelector('#panel [aria-label="Team tasks"]'),
    "Wake rules": heading("Wake the master for this project"),
    "Usage": heading("Usage"),
  };
  return {
    folds: [...document.querySelectorAll("#panel details")].map((item) => ({ label: item.getAttribute("aria-label"), open: item.open })),
    parts: Object.fromEntries(Object.entries(parts).map(([name, item]) => [name, item === null ? null : item.closest("details")?.getAttribute("aria-label") ?? "page"])),
  };
}

/** The open drawer: its title and the labels of its sections, or null when none is open. */
export function drawer() {
  const open = document.querySelector("dialog.drawer[open]");
  if (!open) return null;
  return {
    title: open.querySelector("h2")?.textContent ?? "",
    sections: [...open.querySelectorAll("section")].map((item) => item.getAttribute("aria-label") ?? item.querySelector("h3")?.textContent ?? ""),
  };
}
