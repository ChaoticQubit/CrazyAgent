import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import { RemoveDialogView } from "../../src/screens/project/remove-dialog.tsx";
import { Dialog } from "../../src/ui/overlay.tsx";

let root = null;

/** Opens the remove dialog as project Settings draws it, with the real stylesheets, Delete
 * everything picked and the project's name typed. Input: the engine's removal preview. */
export function mount(preview) {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const idle = () => {};
  flushSync(() => root.render(h(Dialog, { open: true, title: `Remove ${preview.name} from CrazyAgent?`, onClose: idle, wide: true },
    h(RemoveDialogView, { preview, choice: "everything", typed: preview.name, pending: false, onChoice: idle, onTyped: idle }))));
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Moves focus to the name box as the owner does before typing. Output: true when it exists. */
export function focusName() {
  const input = document.querySelector("dialog[open] input[aria-label='Type the project name']");
  input?.focus();
  return Boolean(input);
}

/** Whether a node lies wholly inside the window and inside every box up to its dialog that
 * clips what overflows it. Inputs: the node and its dialog. Output: true or false, or null
 * when the node is not shown. */
function seen(node, dialog) {
  if (!node) return null;
  const rect = node.getBoundingClientRect();
  const inside = (outer) => rect.top >= outer.top - 0.5 && rect.bottom <= outer.bottom + 0.5 && rect.left >= outer.left - 0.5 && rect.right <= outer.right + 0.5;
  if (!inside({ top: 0, left: 0, bottom: innerHeight, right: innerWidth })) return false;
  for (let up = node.parentElement; up && up !== dialog.parentElement; up = up.parentElement) {
    const style = getComputedStyle(up);
    if ((style.overflowY !== "visible" || style.overflowX !== "visible") && !inside(up.getBoundingClientRect())) return false;
  }
  return true;
}

/** Measures the open dialog, its body and its top choice, rounded to whole pixels. Output: the
 * window height, the dialog's, its body's and its folder and loss lists' visible and full
 * heights and scroll offsets, the top choice's edges against the dialog's visible edges, and
 * whether the loss list and the Delete everything button are wholly in view. */
export function fit() {
  const dialog = document.querySelector("dialog[open]");
  const remove = [...dialog.querySelectorAll("button")].find((button) => button.textContent.trim() === "Delete everything");
  const body = dialog.querySelector(".dialog-body");
  const frame = dialog.getBoundingClientRect();
  const card = dialog.querySelector(".pj-radio-card").getBoundingClientRect();
  const box = (node) => ({ client: node.clientHeight, scroll: node.scrollHeight, top: Math.round(node.scrollTop) });
  const details = dialog.querySelector(".pj-remove-details");
  return {
    window: innerHeight, dialog: box(dialog), body: box(body), details: details ? box(details) : null,
    cardTop: Math.round(card.top - frame.top), cardBottom: Math.round(card.bottom - frame.top), frame: Math.round(frame.height),
    losses: seen(dialog.querySelector("[aria-label='What would be lost']"), dialog), remove: seen(remove, dialog),
  };
}
