import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { ContextTab } from "../../src/screens/project/context.tsx";
import { contrast } from "./contrast.mjs";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main"><div class="page pj" id="mount"></div></main></div>';

let root = null;
let sent = [];

/** Lets the bridge answers and the renders they cause land. Input: the wait in
 * milliseconds. Output: nothing. */
export async function settle(ms) {
  await act(async () => { await new Promise((done) => setTimeout(done, ms)); });
}

/** Mounts the real Context tab with the real styles over a listed context; a file read answers
 * with a short Markdown text and any other command is not in this build. Inputs: the page's
 * CSS and the context the engine listed. Output: nothing. */
export async function mount(css, context) {
  sent = [];
  window.__TAURI_INTERNALS__ = {
    plugins: {},
    transformCallback: () => 0,
    async invoke(command, args) {
      sent.push({ command, args });
      if (command === "desktop_project_context_item") return { path: args.path, bytes: 8, offset: 0, text: "# Store\n", next_offset: null };
      throw `Command ${command} not found`;
    },
  };
  const style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);
  document.body.innerHTML = PAGE;
  root = createRoot(document.getElementById("mount"));
  const data = {
    id: "p-1", name: "Harbor Notes", setup: undefined, research: { kind: "ok", value: { project_id: "p-1", jobs: [] } }, context: { kind: "ok", value: context },
    pools: undefined, harnesses: undefined, assigned: null, now: 60_000, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: { result: undefined, at: 0 }, reloadTeam: () => {},
  };
  await act(async () => { root.render(createElement(ContextTab, { data })); });
  await settle(40);
}

/** What the tree shows for each of some words: whether an element in the tree with exactly
 * those words is drawn, and the contrast of its text on what is behind it. Input: the words.
 * Output: one fact per words, in order. */
export function facts(words) {
  const nodes = [...document.querySelectorAll("nav.pj-tree *")];
  return words.map((text, index) => {
    const found = nodes.filter((node) => node.textContent.trim() === text).at(-1);
    if (!found) return { words: text, shown: false, contrast: 0 };
    found.setAttribute("data-probe", String(index));
    const box = found.getBoundingClientRect();
    return { words: text, shown: box.width > 0 && box.height > 0, contrast: contrast(`[data-probe="${index}"]`) };
  });
}

/** Whether the tree scrolls sideways. Output: the answer. */
export function treeOverflows() {
  const tree = document.querySelector("nav.pj-tree");
  return tree.scrollWidth > tree.clientWidth + 1;
}

/** The reader's heading. Output: its text, or null. */
export function heading() {
  return document.querySelector(".pj-reader h2")?.textContent ?? null;
}

/** The screen centre of the element a selector finds with the given words, scrolled into
 * view. Inputs: the selector and the words. Output: the point, or null when nothing matches. */
export function boxOf(selector, words) {
  const found = [...document.querySelectorAll(selector)].find((element) => element.textContent.trim() === words);
  if (!found) return null;
  found.scrollIntoView({ block: "center" });
  const box = found.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}

/** Every call to one bridge command, oldest first. Input: the command. Output: their
 * arguments. */
export function sentTo(command) {
  return sent.filter((call) => call.command === command).map((call) => call.args);
}
