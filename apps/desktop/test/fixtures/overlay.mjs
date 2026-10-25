import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { ConfirmDialog, Menu } from "../../src/ui/overlay.tsx";

export const events = [];
let root = null;

function mount(element) {
  root?.unmount();
  events.length = 0;
  document.body.innerHTML = '<main tabindex="-1"><div id="root"></div><button type="button" id="outside">Outside</button></main>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(element));
}

export function mountMenu() {
  mount(createElement(Menu, { label: "More for Pool A", items: [{ label: "Delete pool", danger: true, onSelect: () => events.push("delete") }] }));
}

export function mountConfirm(pending) {
  mount(createElement(ConfirmDialog, {
    open: true, title: "Remove Harbor?", confirmLabel: "Remove project", cancelLabel: "Keep project", pending,
    onConfirm: () => events.push("confirm"), onCancel: () => events.push("cancel"),
  }, "CrazyAgent deletes the project."));
}
