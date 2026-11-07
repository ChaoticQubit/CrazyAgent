import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { RecentActivity } from "../../src/screens/project/overview.tsx";

export const EVERY = 300;
let root = null;
let asked = [];
let replies = [];
let undos = [];
let undoReplies = [];
let every = EVERY;
let hidden = false;
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
Object.defineProperty(document, "visibilityState", { configurable: true, get: () => (hidden ? "hidden" : "visible") });

/** Reads activity the way the engine does, answering only when the test replies. Inputs:
 * the project id and the limit. Output: the read, once replied. */
function read(projectId) {
  asked.push(projectId);
  return new Promise((done) => replies.push(done));
}

/** Undoes a merge the way the engine does, answering only when the test replies. Input: the
 * change set id. Output: the outcome, once replied. */
function undo(id) {
  undos.push(id);
  return new Promise((done) => undoReplies.push(done));
}

async function settle(ms) {
  await new Promise((done) => setTimeout(done, ms));
}

async function acted(work) {
  await act(async () => {
    work();
    await settle(0);
  });
}

function rowText(row) {
  return row.querySelector(".pj-activity-text")?.firstChild?.textContent ?? "";
}

function state() {
  const section = document.querySelector("section[aria-labelledby='pj-activity']");
  const all = (selector) => [...(section?.querySelectorAll(selector) ?? [])];
  return {
    groups: all(".pj-activity-goal").map((group) => ({ label: group.getAttribute("aria-label"), open: group.querySelector("[aria-expanded]")?.getAttribute("aria-expanded") ?? null, rows: [...group.querySelectorAll(".pj-activity-row")].map(rowText) })),
    timeline: all(".pj-activity-timeline .pj-activity-row").map((row) => ({ text: rowText(row), goal: row.querySelector(".pj-activity-tag")?.textContent ?? null })),
    pressed: all("[aria-pressed=true]").map((chip) => chip.textContent),
    view: all("[role=radio][aria-checked=true]").map((choice) => choice.textContent),
    asked: [...asked],
    waiting: replies.length,
    text: section?.innerText ?? null,
    live: section?.querySelector(".pj-live") !== null && section !== null,
    rows: section?.querySelectorAll(".pj-activity-row").length ?? 0,
    alert: section?.querySelector("[role=alert]")?.textContent ?? null,
    undos: [...undos],
    buttons: all(".pj-activity-row button").map((button) => ({ text: button.textContent, row: rowText(button.closest(".pj-activity-row")), disabled: button.disabled })),
    note: section?.querySelector(".pj-note[role=status]")?.textContent ?? null,
  };
}

/** Mounts Recent activity for a project with the test's read and undo and an interval.
 * Inputs: the project id and the interval, short unless given. Output: what it shows and
 * what it read. */
export async function mount(id, interval = EVERY) {
  root?.unmount();
  asked = [];
  replies = [];
  undos = [];
  undoReplies = [];
  every = interval;
  hidden = false;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  await acted(() => root.render(createElement(RecentActivity, { projectId: id, every, read, undo })));
  return state();
}

/** Shows another project in the same section. Input: its id. Output: the state. */
export async function move(id) {
  await acted(() => root.render(createElement(RecentActivity, { projectId: id, every, read, undo })));
  return state();
}

/** Answers the oldest undo still waiting. Input: the undo's result. Output: the state. */
export async function answerUndo(result) {
  const answer = undoReplies.shift();
  if (!answer) throw new Error("no undo is waiting for a reply");
  await acted(() => answer(result));
  return state();
}

/** Answers the oldest read still waiting. Input: the read's result. Output: the state. */
export async function reply(result) {
  const answer = replies.shift();
  if (!answer) throw new Error("no activity read is waiting for a reply");
  await acted(() => answer(result));
  return state();
}

/** Waits, letting the section poll. Input: the time in ms. Output: the state. */
export async function wait(ms) {
  await settle(ms);
  return state();
}

/** Hides or shows the window. Input: whether it is hidden. Output: the state. */
export async function hide(value) {
  hidden = value;
  await acted(() => document.dispatchEvent(new Event("visibilitychange")));
  return state();
}

/** Unmounts the section, then waits. Input: the time in ms. Output: the state. */
export async function unmount(ms) {
  await acted(() => root.unmount());
  root = null;
  await settle(ms);
  return state();
}

/** Clicks the first element matching a selector whose text has the given words. Inputs: the
 * selector and the words. Output: the state, with missed set when nothing matched. */
export async function click(selector, words) {
  const target = [...document.querySelectorAll(selector)].find((element) => element.textContent.includes(words));
  if (!target) return { ...state(), missed: `${selector} ${words}` };
  await acted(() => target.click());
  return { ...state(), missed: null };
}
