import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { AgentSettingsView } from "../../src/screens/process.tsx";

let root = null;
const saved = [];

/** Mounts a project's agent settings with the settings given, as read at version 0, recording
 * each save. Every save is refused, so the view goes back to the read and each click starts from
 * the settings given. */
export function mount(value) {
  root?.unmount();
  saved.length = 0;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(AgentSettingsView, {
    settings: { kind: "ok", value: { settings: value, version: 0 } }, busy: false, failure: "", onSave: async (next) => { saved.push(next); return "Refused."; },
  })));
}

/** Clicks the radio or switch with this accessible text and lets React render and the refused
 * save go back. Output: whether it found the control. */
export async function click(text) {
  const control = [...document.querySelectorAll("[role=radio], [role=switch]")]
    .find((item) => item.textContent.trim() === text || item.getAttribute("aria-label") === text || item.id === text);
  if (!control) return false;
  flushSync(() => control.click());
  await new Promise((done) => setTimeout(done, 20));
  return true;
}

/** The settings each change asked to save, in order. */
export function saves() {
  return saved.map((value) => ({ ...value }));
}
