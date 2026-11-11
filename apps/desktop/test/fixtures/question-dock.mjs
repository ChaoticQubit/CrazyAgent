import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { QuestionDock } from "../../src/screens/project/question-popup.tsx";

const NOW = Date.UTC(2026, 10, 11, 13, 0);
const ask = (id, text) => ({
  id, project_id: "p-1", asker: "master:s-1", question: text, kind: "single", options: ["Yes", "No"], recommended: "Yes",
  context: null, status: "pending", version: 0, answer: null, created_ms: NOW - 60_000 + id, settled_ms: null,
});
let root = null;
let props = null;
const answered = [];

function render() {
  flushSync(() => root.render(h(QuestionDock, props)));
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Mounts the pop-up above a stand-in terminal field, as the Conversation tab holds it.
 * Input: how many questions wait. Output: nothing. */
export async function mount(count) {
  root?.unmount();
  answered.length = 0;
  document.body.innerHTML = '<textarea id="terminal" aria-label="Master terminal"></textarea><div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  props = {
    waiting: Array.from({ length: count }, (_, at) => ask(at + 1, `Question number ${at + 1}?`)),
    shown: true, now: NOW, busy: false, note: "", settled: 0,
    onAnswer: (question) => {
      answered.push(question.id);
      props = { ...props, settled: props.settled + 1 };
      render();
      props = { ...props, waiting: props.waiting.filter((item) => item.id !== question.id) };
      render();
    },
    onDismiss: () => {}, onOpenTab: () => {},
  };
  render();
  await frames();
}

/** Switches to another tab and back, as the project page does with the pop-up mounted.
 * Output: nothing. */
export async function roundTrip() {
  props = { ...props, shown: false };
  render();
  await frames();
  props = { ...props, shown: true };
  render();
  await frames();
}

/** Clicks the button with these words inside the pop-up. Input: the words. Output: nothing. */
export async function press(words) {
  const button = [...document.querySelectorAll(".qp-dock button")].find((item) => item.textContent.trim() === words);
  if (!button) throw new Error(`no ${words} button: ${document.querySelector(".qp-dock").innerHTML}`);
  flushSync(() => button.click());
  await frames();
}

/** Puts the keyboard in the stand-in terminal, then moves it to the first option of the
 * card and picks it, as a keyboard owner does. Output: nothing. */
export async function pickFirst() {
  const option = document.querySelector(".qp-popup:not([hidden]) .qs-option input");
  option.focus();
  flushSync(() => option.click());
  await frames();
}

/** Puts the keyboard in the stand-in terminal. Output: nothing. */
export function focusTerminal() {
  document.getElementById("terminal").focus();
}

/** Opens the comment box and focuses it so the test types into it. Output: nothing. */
export async function openComment() {
  const box = document.querySelector(".qp-popup .qs-comment input");
  flushSync(() => box.click());
  await frames();
  document.querySelector(".qp-popup textarea").focus();
}

/** What the pop-up shows now. Output: whether the card is open, the words of its title, the
 * Show button's words, the comment box text, the live line and the aria wiring. */
export function seen() {
  const section = document.querySelector(".qp-popup");
  const show = [...document.querySelectorAll(".qp-dock button")].find((item) => item.getAttribute("aria-expanded") === "false");
  const hide = [...document.querySelectorAll(".qp-dock button")].find((item) => item.textContent.trim() === "Hide");
  return {
    open: section ? !section.hidden : false,
    title: section?.querySelector("h2")?.textContent ?? null,
    show: show?.textContent.trim() ?? null,
    comment: document.querySelector(".qp-popup textarea")?.value ?? null,
    live: document.querySelector('.qp-dock [role="status"]')?.textContent ?? null,
    controls: [show?.getAttribute("aria-controls") ?? null, hide?.getAttribute("aria-controls") ?? null, section?.id ?? null],
    answered: [...answered],
  };
}

/** Where the keyboard is. Output: the focused element's tag, id and words. */
export function where() {
  const active = document.activeElement;
  return { tag: active.tagName.toLowerCase(), id: active.id || null, words: active.textContent.trim().slice(0, 60) };
}
