import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { AppMetrics } from "../../src/screens/metrics/index.tsx";
import { MetricsView } from "../../src/screens/metrics/view.tsx";
import { SessionContext } from "../../src/state.ts";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main"><div class="page" id="mount"></div></main></div>';
const PROJECTS = [{ id: "alpha", name: "Alpha" }, { id: "beta", name: "Beta" }];
let root = null;
let sent = [];
let toasts = [];

/** Lets the engine's answers and the renders they cause land. Input: the wait in
 * milliseconds. Output: nothing. */
export async function settle(ms) {
  await act(async () => { await new Promise((done) => setTimeout(done, ms)); });
}

/** Mounts one element inside the session against answers the fixture gives for the engine
 * commands; any other command is not in this build. Inputs: the page's CSS, the answers by
 * command and the element. Output: nothing. */
async function mountWith(css, answers, element) {
  sent = [];
  toasts = [];
  window.__TAURI_INTERNALS__ = {
    plugins: {},
    transformCallback: () => 0,
    async invoke(command, args) {
      sent.push({ command, args });
      if (command in answers) return answers[command];
      throw `Command ${command} not found`;
    },
  };
  const style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);
  document.body.innerHTML = PAGE;
  root = createRoot(document.getElementById("mount"));
  const session = {
    projects: { kind: "ok", value: PROJECTS },
    noteResult: (result) => result,
    showToast: (words) => toasts.push(words),
  };
  await act(async () => {
    root.render(createElement(SessionContext.Provider, { value: session }, element));
  });
  await settle(40);
}

/** Mounts the app Metrics page. Inputs: the page's CSS and the bridge answers by command.
 * Output: nothing. */
export async function mount(css, answers) {
  await mountWith(css, answers, createElement(AppMetrics, { projects: PROJECTS }));
}

/** Mounts one project's Metrics tab, as the project page shows it. Inputs: the styles, the
 * bridge answers and the project id. Output: nothing. */
export async function mountProject(css, answers, projectId) {
  await mountWith(css, answers, createElement(MetricsView, { projectId }));
}

/** Picks a select's option as the owner does and lets the reads it starts land. Inputs: the
 * select's id and the option's value. Output: nothing. */
export async function pick(id, value) {
  const select = document.getElementById(id);
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(select, value);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle(120);
}

/** The screen centre of the element a selector finds with the given words, scrolled into
 * view. Inputs: the selector and the words, or null for the first match. Output: the point,
 * or null when nothing matches. */
export function boxOf(selector, words) {
  const found = [...document.querySelectorAll(selector)].find((element) => words === null || element.textContent.trim() === words);
  if (!found) return null;
  found.scrollIntoView({ block: "center" });
  const box = found.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}

/** Sets a date field's value as its picker does. Inputs: the field's id and the value.
 * Output: nothing. */
export async function setField(id, value) {
  const input = document.getElementById(id);
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** Every call to one engine command, oldest first. Input: the command. Output: their
 * arguments. */
export function sentTo(command) {
  return sent.filter((call) => call.command === command).map((call) => call.args);
}

/** The toasts shown since the mount. Output: their words. */
export function shownToasts() {
  return toasts;
}
