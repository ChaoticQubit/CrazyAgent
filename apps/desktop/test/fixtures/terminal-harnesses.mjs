import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { keepTerminalsOf, TerminalPanel, useKeptTerminalProjects } from "../../src/screens/team/terminal-panel.tsx";
import * as state from "../../src/state.ts";

let root = null;
let probe = null;
let gpu = false;
let identity = "";
let failing = 0;
let asked = 0;
const contexts = [];
const engine = { callbacks: new Map(), channel: null, next: 0, typed: [], attaches: 0, detaches: 0, nodes: [], graphs: [], resizes: [] };
const canvasContext = HTMLCanvasElement.prototype.getContext;
const glParameter = WebGL2RenderingContext.prototype.getParameter;
const UNMASKED_VENDOR = 0x9245;
const UNMASKED_RENDERER = 0x9246;

/** The renderer a Mac's GPU reports, as test Chrome on the build Mac does. */
export const HARDWARE = "ANGLE (Apple, ANGLE Metal Renderer: Apple M2 Max, Unspecified Version)";

/** The renderer a machine without a GPU reports, as the Linux farm's Chrome does. */
export const SOFTWARE = "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero)), SwiftShader driver)";

/** Lets the page's canvases hand out a WebGL2 context that reports one renderer, or answers
 * null for one as a machine without WebGL2 does. Off by default, so every row that does not
 * name a renderer runs on the DOM renderer; a renderer row turns it on before `open`. Inputs:
 * whether WebGL2 is offered and the renderer it reports. */
export function offerGpu(offered, renderer = HARDWARE) {
  gpu = offered;
  identity = renderer;
}

/** Makes the next WebGL2 contexts the page asks for fail, as a GPU that refuses a context
 * does. Input: how many. */
export function failGpu(count) {
  failing = count;
}

HTMLCanvasElement.prototype.getContext = function getContext(kind, ...rest) {
  if (kind !== "webgl2") return canvasContext.call(this, kind, ...rest);
  asked += 1;
  if (!gpu) return null;
  if (failing > 0) {
    failing -= 1;
    return null;
  }
  const context = canvasContext.call(this, kind, ...rest);
  if (context && !contexts.includes(context)) contexts.push(context);
  return context;
};

WebGL2RenderingContext.prototype.getParameter = function getParameter(name) {
  if (name === UNMASKED_RENDERER) return identity;
  if (name === UNMASKED_VENDOR) return "Google Inc.";
  return glParameter.call(this, name);
};

/** How many times the page asked a canvas for a WebGL2 context, a probe's scratch canvas
 * included, whether or not one was given. Output: the count. */
export function gpuAsked() {
  return asked;
}

/** How many WebGL2 contexts the page holds that were not lost. Output: the count. */
export function liveGpu() {
  return contexts.filter((context) => !context.isContextLost()).length;
}

/** Gives the page a working store: the test page is about:blank, whose storage refuses every
 * call, so an in-memory store takes its place, as the app's own window storage keeps a
 * choice. Output: none. */
function keepStorage() {
  try {
    window.localStorage.getItem("crazyagent.terminal-renderer");
  } catch {
    const kept = new Map();
    const store = { getItem: (key) => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, String(value)), removeItem: (key) => kept.delete(key), clear: () => kept.clear() };
    Object.defineProperty(window, "localStorage", { configurable: true, value: store });
  }
}

/** Saves a Terminal renderer choice as the Profile setting does. Input: the choice. */
export function choose(choice) {
  keepStorage();
  state.saveTerminalRenderer(choice);
}

/** Saves a Terminal renderer choice as another window of the app does: the store changes
 * and this window hears only the storage event. Input: the choice. */
export function chooseElsewhere(choice) {
  keepStorage();
  window.localStorage.setItem("crazyagent.terminal-renderer", choice);
  window.dispatchEvent(new StorageEvent("storage", { key: "crazyagent.terminal-renderer", newValue: choice }));
}

/** Makes the page report one platform with nothing chosen yet, as a first start on that
 * machine. Input: the platform name, as navigator.platform gives it. */
export function startOn(platform) {
  keepStorage();
  Object.defineProperty(navigator, "platform", { configurable: true, value: platform });
}

/** Which renderer draws the page's one terminal, shown or parked: "webgl" while its screen
 * holds the WebGL canvas and no DOM rows, "dom" while it holds DOM rows, "none" before it
 * opens. Output: the name. */
export function renderer() {
  const screen = document.querySelector(".tg-terminal-host .xterm-screen");
  if (!screen) return "none";
  if (screen.querySelector(".xterm-rows")) return "dom";
  return screen.querySelector("canvas") ? "webgl" : "none";
}

/** The text the shown terminal's DOM rows draw, as the owner reads it on screen, so a row
 * can tell a drawn screen from one whose buffer holds the text while nothing is drawn.
 * Output: the text, or null while no DOM rows draw it. */
export function domText() {
  const rows = document.querySelector(".tg-terminal-screen .xterm-rows");
  return rows ? rows.textContent.replace(/\u00a0/g, " ") : null;
}

/** Loses the shown terminal's WebGL context, as a GPU reset or a sleeping Mac does. Output:
 * whether a context was there to lose. */
export function loseGpu() {
  for (const canvas of document.querySelectorAll(".tg-terminal-screen .xterm-screen canvas")) {
    const context = canvasContext.call(canvas, "webgl2");
    const lose = context instanceof WebGL2RenderingContext ? context.getExtension("WEBGL_lose_context") : null;
    if (!lose) continue;
    lose.loseContext();
    return true;
  }
  return false;
}

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
  stage();
  back(role, harness);
}

function stage() {
  root?.unmount();
  root = null;
  probe?.unmount();
  probe = null;
  keepTerminalsOf(() => false);
  Object.assign(engine, { callbacks: new Map(), channel: null, next: 0, typed: [], attaches: 0, detaches: 0, nodes: [], graphs: [], resizes: [] });
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
      if (command === "desktop_terminal_detach") {
        engine.detaches += 1;
        return null;
      }
      if (command === "desktop_terminal_resize") {
        engine.resizes.push({ cols: args.cols, rows: args.rows });
        return null;
      }
      if (command === "desktop_team_graph") {
        engine.graphs.push(args.id);
        return { nodes: engine.nodes, edges: [] };
      }
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
  document.body.innerHTML = '<div id="root"><main class="main"><div class="pj pj-project"><div class="pj-tab-panel" id="panel"></div></div></main></div><div id="probe"></div>';
}

function Probe({ projects }) {
  useKeptTerminalProjects({ kind: "ok", value: projects.map((id) => ({ id, name: id })) }, false);
  return null;
}

/** Starts a fresh page as app start does with the projects listed: the stored terminals and
 * last project are set first, the team graph answers with the given nodes, and only the
 * app's kept-terminal hook is mounted, no panel. Input: `{ saved, last, nodes, projects }`.
 * Output: how many attaches ran before the hook's mount returned. */
export function preload({ saved, last, nodes, projects }) {
  stage();
  window.localStorage.clear();
  window.localStorage.setItem("crazyagent.kept-terminals", JSON.stringify(saved));
  if (last) window.localStorage.setItem("crazyagent.last-project", last);
  engine.nodes = nodes;
  probe = createRoot(document.getElementById("probe"));
  flushSync(() => probe.render(createElement(Probe, { projects })));
  return engine.attaches;
}

/** Lists the projects again, as a project list refresh does, to the hook mounted by `preload`.
 * Input: the project ids. Output: none. */
export function list(projects) {
  flushSync(() => probe.render(createElement(Probe, { projects })));
}

/** How many views were let go with a detach, and the stored terminals, since the page was
 * staged. Output: both. */
export function released() {
  return { detaches: engine.detaches, saved: JSON.parse(window.localStorage.getItem("crazyagent.kept-terminals") ?? "null") };
}

/** The project ids the team graph was read for since the page was staged. Output: the ids. */
export function graphs() {
  return [...engine.graphs];
}

/** Every terminal size the panel sent the engine since the page was staged, oldest first.
 * Output: the columns and rows of each. */
export function resizes() {
  return engine.resizes.map((size) => ({ ...size }));
}

/** Where the kept view's element is and whether a terminal has the focus. Output: both. */
export function placed() {
  return {
    parked: document.querySelectorAll(".tg-terminal-parking > .tg-terminal-host").length,
    shown: document.querySelectorAll(".tg-terminal-screen > .tg-terminal-host").length,
    focus: Boolean(document.activeElement?.closest(".xterm")),
  };
}

/** Takes the panel off the page as a switch to another page does; the window keeps its view.
 * Output: none. */
export function away() {
  root?.unmount();
  root = null;
}

/** Takes the panel off the page and lets the window's views go, as closing the project does.
 * Output: none. */
export function drop() {
  away();
  keepTerminalsOf(() => false);
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
 * its screen, the screen's box and the height of one row in page coordinates (the screen's
 * height over its lines, so it holds under every Terminal renderer), the open right-click
 * menu's items with their boxes, and every chunk the engine got typed. */
export function view() {
  const box = (element) => {
    if (!element) return null;
    const rect = element.getBoundingClientRect();
    return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
  };
  const lines = document.querySelector(".tg-terminal-screen .tg-terminal-host")?.readScreen?.() ?? [];
  const screen = document.querySelector(".tg-terminal-screen .xterm-screen");
  const status = document.querySelector(".tg-terminal-note [role=status]");
  return {
    live: status !== null,
    note: status?.textContent ?? null,
    text: lines.join("\n"),
    screen: box(screen),
    rowHeight: screen && lines.length ? screen.getBoundingClientRect().height / lines.length : 0,
    menu: [...document.querySelectorAll(".tg-terminal-menu [role=menuitem]")].map((item) => ({ text: item.textContent.trim(), disabled: item.disabled, ...box(item) })),
    typed: [...engine.typed],
  };
}
