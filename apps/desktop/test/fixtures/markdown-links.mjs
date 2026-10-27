import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { MarkdownView } from "../../src/screens/project/markdown.tsx";

export const opened = [];
let root = null;

export function mount(source) {
  root?.unmount();
  opened.length = 0;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(MarkdownView, { source, onOpenLink: (href) => opened.push(href) })));
}

export function state() {
  const dialog = document.querySelector("dialog[open]");
  return {
    opened: [...opened],
    question: dialog?.querySelector("h2")?.textContent ?? null,
    body: dialog?.querySelector(".dialog-body")?.textContent ?? null,
  };
}
