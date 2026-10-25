import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { GoToLayer } from "../../src/ui/go-to.tsx";

let root = null;

export function mountGoTo() {
  root?.unmount();
  document.body.innerHTML = '<div id="root"></div><div class="xterm"><textarea class="xterm-helper-textarea"></textarea></div><input id="field">';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(GoToLayer, { projects: [{ id: "harbor", name: "Harbor Notes" }] })));
}

export function openDialog() {
  return document.querySelector("dialog[open] h2")?.textContent ?? null;
}

export function shortcutKeys() {
  return [...document.querySelectorAll("dialog[open] kbd")].map((node) => node.textContent);
}
