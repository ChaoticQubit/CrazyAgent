import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import { RemoveDialogView, removeTitle } from "../../src/screens/project/remove-dialog.tsx";
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
  flushSync(() => root.render(h(Dialog, { open: true, title: removeTitle(preview.name), onClose: idle, wide: true },
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

/** How many lines a block's text takes. Input: the element. Output: the line count. */
function lines(node) {
  return Math.round(node.getBoundingClientRect().height / parseFloat(getComputedStyle(node).lineHeight));
}

/** Measures the open dialog, its body and its top choice, rounded to whole pixels. Output: the
 * window height, the dialog's, its body's and its folder and loss lists' visible and full
 * heights and scroll offsets, the top choice's edges against the dialog's visible edges,
 * whether the loss list, the name box and the Delete everything button are wholly in view, the
 * folder list's tab index, the dialog's full and visible widths and the title's line count. */
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
    name: seen(dialog.querySelector("input[aria-label='Type the project name']"), dialog), detailsTab: details?.tabIndex ?? null,
    wide: dialog.scrollWidth, frameWidth: dialog.clientWidth, titleLines: lines(dialog.querySelector(".dialog-head h2")),
  };
}

/** How the folder list sits in the open dialog. Output: the heading's words and whether it is in
 * view, how many folder rows show whole, the tallest row in pixels, and whether the list starts
 * below its heading and ends above the loss list. */
export function folders() {
  const dialog = document.querySelector("dialog[open]");
  const list = dialog.querySelector(".pj-remove-details");
  const heading = dialog.querySelector(".pj-remove-trash .pj-label");
  const losses = dialog.querySelector("[aria-label='What would be lost']");
  const box = list.getBoundingClientRect();
  const rows = [...list.children].map((row) => row.getBoundingClientRect());
  return {
    heading: heading.textContent, headingSeen: seen(heading, dialog),
    whole: rows.filter((row) => row.top >= box.top - 0.5 && row.bottom <= box.bottom + 0.5).length,
    tallest: Math.round(Math.max(...rows.map((row) => row.height))),
    belowHeading: box.top >= heading.getBoundingClientRect().bottom - 0.5,
    aboveLosses: losses === null || box.bottom <= losses.getBoundingClientRect().top + 0.5,
  };
}

/** How each loss line sits in the open dialog. Output: for each line its height in pixels, whether
 * the words saying what is lost are wholly inside it, its text and its hover text. */
export function lossFit() {
  const list = document.querySelector("dialog[open] [aria-label='What would be lost']");
  return [...list.children].map((line) => {
    const box = line.getBoundingClientRect();
    const what = line.querySelector(".pj-remove-what")?.getBoundingClientRect();
    return { height: Math.round(box.height), words: what !== undefined && what.left >= box.left - 0.5 && what.right <= box.right + 0.5, text: line.textContent, title: line.title };
  });
}

/** Where the keyboard is in the folder list. Output: whether the list has the focus and how far
 * it has scrolled, in pixels. */
export function listState() {
  const list = document.querySelector("dialog[open] .pj-remove-details");
  return { focused: document.activeElement === list, top: Math.round(list.scrollTop) };
}
