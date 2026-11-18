import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import projectStyles from "../../src/screens/project/projects.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { plainError } from "../../src/api.ts";
import { nativeTerminalEnabled } from "../../src/screens/team/native-terminal.ts";
import { loadTerminalKind } from "../../src/screens/team/terminal-kind.ts";
import { endCopy, selectNote } from "../../src/screens/team/terminal-io.ts";
import { keepTerminalsOf, TerminalPanel, useKeptTerminalProjects } from "../../src/screens/team/terminal-panel.tsx";
import { isMac } from "../../src/ui/index.ts";

const WEBVIEW = ["desktop_terminal_attach", "desktop_terminal_input", "desktop_terminal_resize", "desktop_terminal_detach"];
const roots = new Map();
let probe = null;
const desk = { calls: [], callbacks: new Map(), views: new Map(), opened: 0, nodes: [], enabled: true, openFailure: null, copyFailure: null, promptFailure: null, hold: false, held: [] };

/** Gives the page a working store: the test page is about:blank, whose storage refuses every
 * call, so an in-memory store takes its place. Output: none. */
function keepStorage() {
  try {
    window.localStorage.getItem("crazyagent.kept-terminals");
  } catch {
    const kept = new Map();
    const store = { getItem: (key) => kept.get(key) ?? null, setItem: (key, value) => kept.set(key, String(value)), removeItem: (key) => kept.delete(key), clear: () => kept.clear() };
    Object.defineProperty(window, "localStorage", { configurable: true, value: store });
  }
}

/** The stand-in desktop: records every call with its arguments, opens native views with ids
 * 1, 2, ... (held back while `hold` is set), answers the terminal kind, the team graph and the
 * copy as staged, and accepts every other native and webview terminal command. Inputs: the
 * command and its arguments. Output: the answer. */
async function invoke(command, args) {
  desk.calls.push({ command, args: args === undefined ? null : JSON.parse(JSON.stringify(args)) });
  if (command === "desktop_native_terminal_enabled") {
    if (typeof desk.enabled === "boolean") return desk.enabled;
    throw desk.enabled;
  }
  if (command === "desktop_native_terminal_open") {
    if (desk.openFailure !== null) throw desk.openFailure;
    desk.opened += 1;
    const view = desk.opened;
    desk.views.set(view, { channel: args.events.id, next: 0 });
    if (!desk.hold) return view;
    return new Promise((resolve, reject) => desk.held.push((thrown) => (thrown === undefined ? resolve(view) : reject(thrown))));
  }
  if (command === "desktop_native_terminal_copy" && desk.copyFailure !== null) throw desk.copyFailure;
  if (command === "desktop_terminal_prompt" && desk.promptFailure !== null) throw desk.promptFailure;
  if (command === "desktop_team_graph") return { nodes: desk.nodes, edges: [] };
  if (command.startsWith("desktop_native_terminal_") || command === "desktop_terminal_prompt" || WEBVIEW.includes(command)) return null;
  throw { code: "unknown", message: command };
}

/** Starts a fresh page over the stand-in desktop and reads the terminal kind as desktop start
 * does: the views the window kept go first. Input: `{ enabled, nodes }`, where `enabled` is
 * the desktop's answer, true, false or the value its call throws. Output: none. */
export async function stage({ enabled = true, nodes = [] } = {}) {
  for (const root of roots.values()) root.unmount();
  roots.clear();
  probe?.unmount();
  probe = null;
  keepTerminalsOf(() => false);
  keepStorage();
  window.localStorage.clear();
  Object.assign(desk, { calls: [], callbacks: new Map(), views: new Map(), opened: 0, nodes, enabled, openFailure: null, copyFailure: null, promptFailure: null, hold: false, held: [] });
  window.__TAURI_INTERNALS__ = {
    invoke,
    transformCallback: (callback) => {
      const id = desk.callbacks.size + 1;
      desk.callbacks.set(id, callback);
      return id;
    },
  };
  document.head.innerHTML = "";
  for (const text of [styles, projectStyles, teamStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="pj pj-project" id="slots"></div></main></div><div id="probe"></div>';
  await loadTerminalKind(nativeTerminalEnabled);
}

/** An agent as the team graph lists it. Inputs: the role, "master" or "sub", the label and
 * the terminal name, or null for an agent with no terminal yet. Output: the node. */
export function agent(role, label, terminal) {
  const master = role === "master";
  return { id: `node-${label}`, kind: role, parent: master ? null : "m", label, harness: "claude", status: "working", status_label: "Working", account: null, terminal, agent: 1 };
}

/** Mounts the real terminal panel in a slot of its own, 640 by 420 px. Inputs: the slot name,
 * the node and the panel's other props. Output: none. */
export function mount(slot, node, props = {}) {
  let host = document.getElementById(slot);
  if (host === null) {
    host = document.createElement("div");
    host.id = slot;
    host.className = "pj-tab-panel";
    host.style.cssText = "width:640px;height:420px;display:flex;flex-direction:column";
    document.getElementById("slots").append(host);
  }
  const root = createRoot(host);
  roots.set(slot, root);
  flushSync(() => root.render(createElement(TerminalPanel, { node, projectId: "p1", ...props })));
}

/** Unmounts a slot's panel, as closing it or a graph click to another agent does. Input: the
 * slot. Output: none. */
export function unmount(slot) {
  const root = roots.get(slot);
  if (root) flushSync(() => root.unmount());
  roots.delete(slot);
}

function Probe({ projects }) {
  useKeptTerminalProjects({ kind: "ok", value: projects.map((id) => ({ id, name: id })) }, false);
  return null;
}

/** Starts the app's kept-terminal hook with terminals remembered from the last run, as app
 * start does, with no panel. Inputs: the stored terminals and the listed project ids.
 * Output: none. */
export function preload(saved, projects) {
  window.localStorage.setItem("crazyagent.kept-terminals", JSON.stringify(saved));
  probe = createRoot(document.getElementById("probe"));
  flushSync(() => probe.render(createElement(Probe, { projects })));
}

/** Lets go of every kept view, as a sign-out does. Output: none. */
export function letGoAll() {
  keepTerminalsOf(() => false);
}

/** Sends one event down a native view's channel as the desktop does, in order. Inputs: the
 * view id and the event. Output: none. */
export function event(view, message) {
  const entry = desk.views.get(view);
  if (!entry) throw new Error(`no native view ${view}`);
  const callback = desk.callbacks.get(entry.channel);
  callback({ index: entry.next, message });
  entry.next += 1;
}

/** Answers every held open, with its view id or by throwing a value, and opens later answer
 * at once again. Input: the value to throw, or nothing to answer the id. Output: how many were
 * held. */
export function release(thrown) {
  desk.hold = false;
  const held = desk.held.splice(0);
  for (const answer of held) answer(thrown);
  return held.length;
}

/** Makes the next opens, copies or prompts fail as the desktop does. Inputs: which call
 * ("open", "copy" or "prompt") and the value it throws, or null to answer again. Output: none. */
export function fail(call, thrown) {
  if (call === "open") desk.openFailure = thrown;
  else if (call === "copy") desk.copyFailure = thrown;
  else desk.promptFailure = thrown;
}

/** Saves the owner's Terminal input choice, as Settings does. Input: "terminal", "box" or
 * "both". Output: none. */
export function input(choice) {
  window.localStorage.setItem("crazyagent.terminal-input", choice);
}

/** Types a prompt in a slot's message box and sends it with the Send button. Inputs: the slot
 * and the text. Output: whether the box and an enabled Send button were there. */
export async function promptBox(slot, text) {
  const box = document.querySelector(`#${slot} textarea.tg-composer-input`);
  if (!box) return false;
  Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set.call(box, text);
  box.dispatchEvent(new Event("input", { bubbles: true }));
  await new Promise((done) => setTimeout(done, 50));
  const send = [...document.querySelectorAll(`#${slot} button`)].find((button) => button.textContent.trim() === "Send");
  if (!send || send.disabled) return false;
  send.click();
  return true;
}

/** Holds every next open's answer until `release`, as a slow desktop does. Output: none. */
export function hold() {
  desk.hold = true;
}

/** The calls the page made since it was staged, in order. Input: the command name, or null
 * for every call. Output: `[{ command, args }]`. */
export function calls(command = null) {
  return desk.calls.filter((entry) => command === null || entry.command === command);
}

/** The screen element's place in a slot, as the native view must be placed. Input: the slot.
 * Output: `{ x, y, width, height }`, or null with no screen. */
export function screenRect(slot) {
  const screen = document.querySelector(`#${slot} .tg-terminal-screen`);
  if (!screen) return null;
  const box = screen.getBoundingClientRect();
  return { x: box.left, y: box.top, width: box.width, height: box.height };
}

/** What a slot's panel shows: its status lines, alerts, buttons and menu items, and whether
 * the page holds any webview terminal. Input: the slot. Output: the texts. */
export function shown(slot) {
  const panel = document.querySelector(`#${slot} .tg-terminal`);
  const texts = (selector) => [...(panel?.querySelectorAll(selector) ?? [])].map((element) => element.textContent.trim());
  return {
    status: texts("[role=status]"),
    alerts: texts("[role=alert]"),
    buttons: texts("button"),
    menu: texts("[role=menu] [role=menuitem]"),
    xterms: document.querySelectorAll(".xterm, .tg-terminal-host").length,
    webview: desk.calls.filter((entry) => WEBVIEW.includes(entry.command)).length,
  };
}

/** Clicks the button or menu item in a slot whose text is given. Inputs: the slot and the
 * text. Output: whether one was there. */
export function press(slot, text) {
  const target = [...document.querySelectorAll(`#${slot} button`)].find((button) => button.textContent.trim() === text);
  if (!target) return false;
  target.click();
  return true;
}

/** Clicks the item of a slot's open terminal menu whose text is given. Inputs: the slot and
 * the text. Output: whether one was there. */
export function choose(slot, text) {
  const target = [...document.querySelectorAll(`#${slot} [role=menuitem]`)].find((item) => item.textContent.trim() === text);
  if (!target) return false;
  target.click();
  return true;
}

/** Mounts a slot's panel and brings its new native view live: the open, then the desktop's
 * ready event, then the live status line. Inputs: the slot, the node and the panel's other props.
 * Output: the view id, or null when no view opened or it never went live. */
export async function live(slot, node, props = {}) {
  const before = desk.opened;
  mount(slot, node, props);
  if (!(await until(() => desk.opened === before + 1))) return null;
  const view = desk.opened;
  event(view, { type: "ready", cols: 80, rows: 24 });
  return await until(() => document.querySelector(`#${slot} .tg-terminal-note [role=status]`) !== null) ? view : null;
}

/** The calls the page made with a view id since a mark. Inputs: the view and the count of
 * calls at the mark. Output: the command names. */
export function callsOn(view, from) {
  return desk.calls.slice(from).filter((entry) => entry.args?.view === view).map((entry) => entry.command);
}

/** Puts a page dialog over the whole window, or takes it away. Input: whether it shows.
 * Output: none. */
export function dialog(open) {
  document.getElementById("cover")?.remove();
  if (!open) return;
  const element = document.createElement("div");
  element.id = "cover";
  element.setAttribute("role", "dialog");
  element.style.cssText = "position:fixed;left:0;top:0;width:100vw;height:100vh";
  document.body.append(element);
}

/** The words the panel shows for a copy the desktop refused, a mouse note and an end code.
 * Inputs: the refusal code and the end code. Output: the texts. */
export function words(code, ended) {
  return { refused: plainError(code), select: selectNote(isMac()), ended: endCopy(ended) };
}

/** Waits until a check holds, up to `ms`. Inputs: the check and the time. Output: whether it
 * held. */
export async function until(check, ms = 3000) {
  const end = performance.now() + ms;
  while (performance.now() < end) {
    if (check()) return true;
    await new Promise((done) => setTimeout(done, 20));
  }
  return check();
}
