import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { QuestionsPanel } from "../../src/screens/team/questions.tsx";
import { Drawer } from "../../src/ui/overlay.tsx";

const idle = () => {};
const QUESTION = { id: 7, agent: 3, label: "Builder 3", severity: "low", asked_for: "me", for_label: "Me", text: "Which database?", assumption: "SQLite", created_ms: 0, terminal: null };
const WAITING = { agent: 4, label: "Tester 4", waiting: "permission", terminal: null };
const inbox = () => ({ shown: { kind: "ok", value: { project_id: "alpha", questions: [QUESTION], more: 0, waiting: [WAITING] } }, stale: null });
let root = null;
let startOn = null;

function render(read) {
  flushSync(() => root.render(h(Drawer, { open: true, title: "Questions for you", onClose: idle },
    h(QuestionsPanel, { projectId: "alpha", read, reload: idle, onOpen: idle, focus: startOn }))));
}

function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Opens the Questions drawer on the Agents tab, as a toast does. Inputs: whether the inbox is
 * already read and the row kind the keyboard starts on. Output: nothing. */
export async function mount(loaded, focus) {
  root?.unmount();
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  startOn = focus;
  render(loaded ? inbox() : { shown: undefined, stale: null });
  await frames();
}

/** Lands a new read of the inbox, as the poll does. Output: nothing. */
export async function reread() {
  render(inbox());
  await frames();
}

/** Clicks Answer and puts the cursor in the answer box, as the owner does. Output: whether the
 * box took the focus. */
export async function openAnswer() {
  const answer = [...document.querySelectorAll("dialog[open] button")].find((button) => button.textContent.trim() === "Answer");
  flushSync(() => answer.click());
  await frames();
  const box = document.querySelector("dialog[open] textarea");
  box.focus();
  return document.activeElement === box;
}

/** Where the keyboard is. Output: the focused element's tag name, and its words when it is a row. */
export function where() {
  const active = document.activeElement;
  const tag = active.tagName.toLowerCase();
  return { tag, row: tag === "li" ? active.textContent : null };
}

/** The answer box's text. Output: the text. */
export function typed() {
  return document.querySelector("dialog[open] textarea").value;
}
