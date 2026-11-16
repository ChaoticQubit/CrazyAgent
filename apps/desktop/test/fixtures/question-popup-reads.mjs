import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { QuestionPopup } from "../../src/screens/project/question-popup.tsx";

const NOW = Date.UTC(2026, 10, 11, 13, 0);
const QUESTION = {
  id: 31, project_id: "p-1", asker: "master:s-1", question: "Should checkout keep a guest option?",
  kind: "single", options: ["Yes", "No"], recommended: "Yes", context: null, status: "pending",
  version: 0, answer: null, created_ms: NOW - 60_000, settled_ms: null,
};
const DATA = { id: "p-1", now: NOW, navigate: () => {}, notify: () => {} };
let waiting = [];
let reads = 0;
let shown = true;
let root = null;

window.__TAURI_INTERNALS__ = {
  plugins: {},
  transformCallback: () => 0,
  invoke: async (command) => {
    if (command !== "desktop_questions") throw `Command ${command} not found`;
    reads += 1;
    return { items: waiting.map((item) => ({ ...item })), next: null };
  },
};

const settle = () => new Promise((done) => setTimeout(done, 20));

function render() {
  flushSync(() => root.render(h(QuestionPopup, { data: DATA, shown })));
}

/** Mounts the project's pop-up on the Conversation tab with one question waiting, as the
 * project page holds it, and lets its first read land. Output: nothing. */
export async function mount() {
  root?.unmount();
  waiting = [QUESTION];
  reads = 0;
  shown = true;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  render();
  await settle();
}

/** Switches to the Questions tab, where the owner answers the question, then back to the
 * Conversation tab well inside the pop-up's 5 s poll. Output: nothing. */
export async function answerOnTheTab() {
  shown = false;
  render();
  await settle();
  waiting = [];
  shown = true;
  render();
  await settle();
}

/** What the pop-up shows now. Output: whether its card is open, the card's title, the live
 * line and how many question reads it made. */
export function seen() {
  const section = document.querySelector(".qp-popup");
  return {
    open: section ? !section.hidden : false,
    title: section?.querySelector("h2")?.textContent ?? null,
    live: document.querySelector('.qp-dock [role="status"]')?.textContent ?? null,
    reads,
  };
}
