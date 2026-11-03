import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { AgentSettingsView } from "../../src/screens/process.tsx";

let root = null;
const saved = [];

/** Mounts a project's agent settings with the engine read given, recording each save. */
export function mount(value) {
  root?.unmount();
  saved.length = 0;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(AgentSettingsView, {
    settings: { kind: "ok", value }, busy: false, failure: "", onSave: (next) => saved.push(next),
  })));
}

/** Clicks the radio or switch with this accessible text and lets React render. */
export function click(text) {
  const control = [...document.querySelectorAll("[role=radio], [role=switch]")]
    .find((item) => item.textContent.trim() === text || item.getAttribute("aria-label") === text || item.id === text);
  if (!control) return false;
  flushSync(() => control.click());
  return true;
}

/** The settings each change asked to save, in order. */
export function saves() {
  return saved.map((value) => ({ ...value }));
}
