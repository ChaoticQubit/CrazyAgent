import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { keepTerminalsOf, TerminalPanel } from "../../src/screens/team/terminal-panel.tsx";

let root = null;
const engine = { callbacks: new Map(), channel: null, next: 0, typed: [], attaches: 0 };

/** Sends one message down the attached terminal's channel as the bridge does, in order.
 * Input: the message, a control frame or raw output bytes. */
function deliver(message) {
  const callback = engine.callbacks.get(engine.channel);
  if (!callback) throw new Error("no terminal is attached");
  const index = engine.next;
  engine.next += 1;
  callback({ index, message });
}

/** Mounts the real terminal panel for one agent on a fresh page, over a stand-in engine that
 * attaches its terminal at 80 by 24 and keeps every chunk the panel types into it, mouse
 * reports included. The views the window kept go first, as when the session starts again,
 * so the panel attaches anew. Inputs: the agent's role, "master" or "sub", and its harness id. */
export function open(role, harness) {
  root?.unmount();
  keepTerminalsOf(() => false);
  Object.assign(engine, { callbacks: new Map(), channel: null, next: 0, typed: [], attaches: 0 });
  window.__TAURI_INTERNALS__ = {
    invoke: async (command, args) => {
      if (command === "desktop_terminal_attach") {
        engine.channel = args.output.id;
        engine.next = 0;
        engine.attaches += 1;
        deliver({ kind: "attached", cols: 80, rows: 24 });
        return 1;
      }
      if (command === "desktop_terminal_input") {
        engine.typed.push(String.fromCharCode(...args.bytes));
        return null;
      }
      if (command === "desktop_terminal_resize" || command === "desktop_terminal_detach") return null;
      throw { code: "unknown", message: command };
    },
    transformCallback: (callback) => {
      const id = engine.callbacks.size + 1;
      engine.callbacks.set(id, callback);
      return id;
    },
  };
  document.head.innerHTML = "";
  for (const text of [styles, projectStyles, teamStyles, xtermStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="pj pj-project"><div class="pj-tab-panel" id="panel"></div></div></main></div>';
  back(role, harness);
}

/** Takes the panel off the page as a switch to another page does; the window keeps its view.
 * Output: none. */
export function away() {
  root?.unmount();
  root = null;
}

/** Mounts the panel for the agent again over the same stand-in engine, as a switch back does.
 * Inputs: the agent's role and its harness id. Output: the ms the mount took. */
export function back(role, harness) {
  const master = role === "master";
  const node = {
    id: "team-1", kind: role, parent: master ? null : "m", label: master ? "Master" : "Ada", harness, status: "working", status_label: "Working",
    account: null, terminal: "team-1", agent: 1,
  };
  const start = performance.now();
  root = createRoot(document.getElementById("panel"));
  flushSync(() => root.render(createElement(TerminalPanel, { node, projectId: "p1" })));
  return performance.now() - start;
}

/** Ends the attached terminal as the engine does when the agent's app exits. Output: none. */
export function close() {
  deliver({ kind: "closed", reason: "exited" });
}

/** How many times the panel attached since the page was opened. Output: the count. */
export function attaches() {
  return engine.attaches;
}

/** Writes what the agent's app prints into the attached terminal, as the engine relays pane
 * output. Input: the text, escape sequences included. */
export function output(text) {
  deliver(new TextEncoder().encode(text).buffer);
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

/** The panel as the owner sees it: whether its terminal is live, its status line, the text on
 * its screen, the screen's box and the height of one row in page coordinates, the open
 * right-click menu's items with their boxes, and every chunk the engine got typed. */
export function view() {
  const box = (element) => {
    if (!element) return null;
    const rect = element.getBoundingClientRect();
    return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
  };
  const rows = document.querySelector(".tg-terminal-screen .xterm-rows");
  const status = document.querySelector(".tg-terminal-note [role=status]");
  return {
    live: status !== null,
    note: status?.textContent ?? null,
    text: rows?.textContent ?? "",
    screen: box(document.querySelector(".tg-terminal-screen .xterm-screen")),
    rowHeight: rows?.firstElementChild?.getBoundingClientRect().height ?? 0,
    menu: [...document.querySelectorAll(".tg-terminal-menu [role=menuitem]")].map((item) => ({ text: item.textContent.trim(), disabled: item.disabled, ...box(item) })),
    typed: [...engine.typed],
  };
}
