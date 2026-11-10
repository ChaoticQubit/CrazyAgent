import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { AgentsToolbar } from "../../src/screens/team/index.tsx";
import { TerminalWall } from "../../src/screens/terminals/index.tsx";

const WAIT_STEP_MS = 20;

let root = null;
const engine = { reads: [], setting: 6, calls: [] };

/** A team as the engine reports it. Input: how many terminals it has, or "live" for a working
 * agent with no terminal, which keeps the wall on its quick read. Output: the graph. */
function team(terminals) {
  const count = terminals === "live" ? 1 : terminals;
  const nodes = Array.from({ length: count }, (_, at) => ({
    id: `a${at}`, kind: at === 0 ? "master" : "sub", parent: at === 0 ? null : "a0", label: `a${at}`, harness: "codex",
    status: "working", status_label: "Working", account: null, terminal: terminals === "live" ? null : `team-${at}`,
  }));
  return { project_id: "alpha", nodes, edges: [] };
}

function bridge() {
  window.__TAURI_INTERNALS__ = {
    invoke: async (command, args) => {
      engine.calls.push({ command, args });
      if (command === "desktop_team_graph") {
        const read = engine.reads.length > 1 ? engine.reads.shift() : engine.reads[0];
        if (read.fail) throw { code: read.fail, message: read.fail };
        return team(read.terminals);
      }
      if (command === "desktop_terminals_per_window") return engine.setting;
      return null;
    },
    transformCallback: () => 1,
  };
}

function start(reads, setting) {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [styles, projectStyles, teamStyles].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  engine.reads = reads;
  engine.setting = setting;
  engine.calls = [];
  bridge();
  root = createRoot(document.getElementById("root"));
}

/** Mounts the real wall window of project alpha. Input: the graph reads the engine answers in
 * turn, each { terminals } or { fail: code }; the last one repeats. */
export function mountWall(reads) {
  start(reads, 6);
  flushSync(() => root.render(h(TerminalWall, { projectId: "alpha", page: 1, name: "Alpha" })));
}

/** Mounts the real Agents toolbar drawing an older read of the team. Inputs: the terminals and
 * the terminals per window the toolbar was given, the graph reads the engine answers and its
 * Terminals per window now. */
export function mountToolbar(shown, perWindow, reads, setting) {
  start(reads, setting);
  flushSync(() => root.render(h(AgentsToolbar, { projectId: "alpha", name: "Alpha", perWindow, team: team(shown), line: null })));
}

/** The middle of Open all terminals on the page, for a real click. Output: the point. */
export function openAllAt() {
  const box = document.querySelector(".tg-open-all").getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}

/** The arguments of each call of one command so far. Input: the command. Output: the list. */
export function calls(command) {
  return engine.calls.filter((call) => call.command === command).map((call) => call.args);
}

/** Waits until one command has been called a number of times, or the time is up. Inputs: the
 * command, how many calls and the longest wait in ms. Output: the arguments of its calls. */
export async function seen(command, times, ms) {
  const deadline = performance.now() + ms;
  while (performance.now() < deadline && calls(command).length < times) await new Promise((done) => setTimeout(done, WAIT_STEP_MS));
  return calls(command);
}

/** The refresh failure the wall window says, or null. Output: the text. */
export function stale() {
  return document.querySelector(".tw-stale")?.textContent ?? null;
}
