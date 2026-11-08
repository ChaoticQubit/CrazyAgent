import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import changesStyles from "../../src/screens/changes/changes.css?inline";
import projectsStyles from "../../src/screens/project/projects.css?inline";
import { Detail } from "../../src/screens/changes/index.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let root = null;
let props = null;
let asked = [];
let waiting = [];
let moved = 0;

/** Reads a diff page the way the engine does, answering only when the test replies.
 * Inputs: the change set, the head and the page. Output: the reply the test gives. */
function load(id, head, page) {
  asked.push({ id, head, page });
  return new Promise((done) => waiting.push(done));
}

async function acted(work) {
  await act(async () => {
    work();
    await new Promise((done) => setTimeout(done, 0));
  });
}

function render() {
  root.render(createElement(Detail, props));
}

/** What the owner sees: the pages read, how often a refused page asked for the list again,
 * whether Accept and Discard are offered and enabled, the paging buttons, the diff box,
 * the error and the notes. */
export function state() {
  const button = (word) => [...document.querySelectorAll("button")].find((element) => element.textContent === word);
  const enabled = (word) => (button(word) ? !button(word).disabled : null);
  const box = document.querySelector(".ch-diff");
  return {
    asked: [...asked],
    moved,
    accept: enabled("Accept"),
    discard: enabled("Discard"),
    paging: Boolean(button("Next page") || button("Previous page")),
    diff: box?.textContent ?? null,
    scrollable: box ? box.scrollHeight > box.clientHeight : null,
    alert: document.querySelector("[role=alert]")?.textContent ?? null,
    notes: [...document.querySelectorAll(".pj-note")].map((note) => note.textContent),
  };
}

/** Mounts one change set's details with the app's styles and a page reader that waits for
 * the test. Inputs: the change set and now. Output: the state. */
export async function mount(set, now) {
  root?.unmount();
  asked = [];
  waiting = [];
  moved = 0;
  document.head.innerHTML = "";
  for (const text of [styles, projectsStyles, changesStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root" style="width: 900px"></div>';
  root = createRoot(document.getElementById("root"));
  props = { set, now, busy: false, onAsk: () => {}, onUpdate: () => {}, onRestore: () => {}, onMoved: () => { moved += 1; }, load };
  await acted(render);
  return state();
}

/** Shows the change set as the next list read gives it. Input: the change set. Output: the
 * state. */
export async function show(set) {
  props = { ...props, set };
  await acted(render);
  return state();
}

/** Answers the oldest page read waiting. Input: the reply. Output: the state. */
export async function reply(result) {
  const done = waiting.shift();
  if (!done) throw new Error("no page read is waiting for a reply");
  await acted(() => done(result));
  return state();
}

/** Clicks a button by its words. Input: the words. Output: the state. */
export async function click(words) {
  const target = [...document.querySelectorAll("button")].find((element) => element.textContent === words);
  if (!target) throw new Error(`no ${words} button`);
  await acted(() => target.click());
  return state();
}

/** Scrolls the diff box, when one is shown, to a share of its scroll height, 0 the top and
 * 1 the end, then waits two rendered frames so the browser reports what came on screen.
 * Input: the share. Output: the state. */
export async function scrollTo(share) {
  const box = document.querySelector(".ch-diff");
  if (box) box.scrollTop = (box.scrollHeight - box.clientHeight) * share;
  await act(async () => {
    await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(done, 50))));
  });
  return state();
}
