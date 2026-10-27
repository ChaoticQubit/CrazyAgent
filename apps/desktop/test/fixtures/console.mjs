import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { Console } from "../../src/screens/project/console.tsx";
import { agentMentions } from "../../src/screens/project/console-model.ts";

let root = null;
export const sent = [];
export const routes = [];
export const queries = [];
export let stops = 0;

const graphNode = (id, label) => ({ id, kind: "agent", parent: "g", label, harness: "codex", status: "working", status_label: null, account: null, agents: null, terminal: null });
const lines = [
  { id: "1", kind: "owner", text: "Ask @master about the login" },
  { id: "2", kind: "reply", text: "The login form posts to /api/session." },
  { id: "3", kind: "delegate", text: "Ada (@onboarding-fix): Fix the login" },
];

export function mount() {
  root?.unmount();
  sent.length = 0;
  routes.length = 0;
  queries.length = 0;
  stops = 0;
  document.body.innerHTML = '<div id="root" style="height:480px;width:720px"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(Console, {
    projectId: "p-1", lines, agents: agentMentions([graphNode("team-1", "Ada (@onboarding-fix)"), graphNode("team-2", "Grace")]), files: null,
    onSend: (text) => sent.push(text), onStop: () => { stops += 1; }, onNavigate: (route) => routes.push(route),
    onTagQuery: (query) => queries.push(query),
  })));
  return document.querySelector(".cs textarea").id;
}

export function syntheticEnter(id, patch) {
  const event = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true, ...patch });
  if (patch.keyCode !== undefined) Object.defineProperty(event, "keyCode", { value: patch.keyCode });
  document.getElementById(id).dispatchEvent(event);
  return event.defaultPrevented;
}

export function state() {
  const field = document.querySelector(".cs textarea");
  return {
    value: field.value,
    rows: field.rows,
    sent: [...sent],
    routes: [...routes],
    stops,
    queries: [...queries],
    lines: [...document.querySelectorAll(".cs-line")].map((line) => line.textContent),
    options: [...document.querySelectorAll(".cs-menu [role=option]")].map((option) => option.textContent),
    selected: document.querySelector(".cs-menu [aria-selected=true]")?.textContent ?? null,
    expanded: field.getAttribute("aria-expanded"),
    status: document.querySelector(".cs-status")?.textContent ?? null,
  };
}
