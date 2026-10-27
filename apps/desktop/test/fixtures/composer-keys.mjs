import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { ConversationTab } from "../../src/screens/project/conversation.tsx";
import { Composer } from "../../src/screens/team/terminal-panel.tsx";

let root = null;
export const sent = [];

function mount(element) {
  root?.unmount();
  sent.length = 0;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(element));
}

const agent = { id: "team-1", kind: "master", parent: null, label: "Master", harness: "codex", status: null, status_label: null, account: null, agents: null, terminal: "team-1" };
const terminalComposer = (live) => createElement(Composer, { node: agent, live, onSend: (text) => { sent.push(text); return true; } });

export function mountTerminalComposer(live = true) {
  mount(terminalComposer(live));
  return document.querySelector("textarea").id;
}

export function setLive(live) {
  flushSync(() => root.render(terminalComposer(live)));
  return { disabled: document.querySelector("textarea").disabled, send: document.querySelector("button[type=submit]").disabled };
}

export function mountMasterComposer() {
  const data = {
    id: "alpha", name: "Alpha", setup: undefined, research: undefined, context: undefined, pools: undefined, harnesses: undefined,
    assigned: null, now: 0, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
    team: { kind: "loading" }, reloadTeam: () => {},
  };
  mount(createElement(ConversationTab, { data }));
  return "pj-composer";
}

export function field(id) {
  const element = document.getElementById(id);
  return { value: element.value, focused: document.activeElement === element, help: document.getElementById(element.getAttribute("aria-describedby"))?.textContent ?? "", sent: [...sent] };
}

export function syntheticEnter(id, init) {
  const element = document.getElementById(id);
  const event = new KeyboardEvent("keydown", { key: "Enter", code: "Enter", bubbles: true, cancelable: true, ...init });
  element.dispatchEvent(event);
  return event.defaultPrevented;
}
