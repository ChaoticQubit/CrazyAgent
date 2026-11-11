import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import { SettingLine, SettingSection } from "../../src/ui/setting.tsx";
import { Segmented } from "../../src/ui/toggle.tsx";

export { contrast } from "./contrast.mjs";

let root = null;

/** Mounts one section with a plain, a changed, a named, a wide and a two-control row. Inputs:
 * the section's width and the theme. */
export function mount(width, theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.style.overflow = "auto";
  document.body.innerHTML = `<div id="root" style="width:${width}px"></div>`;
  root = createRoot(document.getElementById("root"));
  const actions = h("button", { id: "act", type: "button" }, "New pool");
  flushSync(() => root.render(h(SettingSection, { title: "Your pools", about: "Sample words.", actions },
    h(SettingLine, { label: "Plain", help: "Help under the label." }, h("button", { id: "plain", type: "button" }, "Edit")),
    h(SettingLine, { label: "Changed", changed: true }, h("button", { id: "marked", type: "button" }, "Edit")),
    h(SettingLine, { label: "Pool name", htmlFor: "named" }, h("input", { id: "named", className: "input" })),
    h(SettingLine, { label: "Wide", help: "Under.", wide: true }, h("textarea", { id: "wide", className: "textarea", style: { width: "200px" } })),
    h(SettingLine, { label: "Two" }, h("button", { type: "button", className: "btn small" }, "Edit pool"), h("button", { type: "button", className: "btn small" }, "Delete pool")))));
}

const rect = (selector) => document.querySelector(selector)?.getBoundingClientRect() ?? null;

/** Reads the rows. Output: each row's label, help and control edges, the control's and the
 * row's vertical middles, where the control's first child starts, the edges of every control
 * in it and its changed marks. */
export function rows() {
  return [...document.querySelectorAll(".set-row")].map((row) => {
    const box = row.getBoundingClientRect();
    const label = row.querySelector(".set-label").getBoundingClientRect();
    const holder = row.querySelector(".set-ctl");
    const control = holder.getBoundingClientRect();
    const help = row.querySelector(".set-help")?.getBoundingClientRect();
    return {
      label: label.left, labelRight: label.right, labelBottom: label.bottom, control: control.left, controlTop: control.top, right: control.right,
      controlMiddle: control.top + control.height / 2, rowMiddle: box.top + box.height / 2, childLeft: holder.firstElementChild.getBoundingClientRect().left,
      parts: [...holder.children].map((child) => { const edge = child.getBoundingClientRect(); return { left: edge.left, right: edge.right, top: edge.top, bottom: edge.bottom }; }),
      help: help?.top ?? null, helpRight: help?.right ?? null, marks: [...row.querySelectorAll(".set-changed")].map((mark) => mark.textContent),
    };
  });
}

/** Reads the section. Output: its h3's words, the name its region carries, the purpose line,
 * whether the action sits inside its box, how far the action stops from the box's right edge,
 * whether it scrolls sideways and its right edge. */
export function section() {
  const group = document.querySelector(".set-group");
  const box = group.getBoundingClientRect();
  const action = rect("#act");
  return {
    heading: group.querySelector("h3")?.textContent ?? null, name: document.getElementById(group.getAttribute("aria-labelledby"))?.textContent ?? null,
    about: group.querySelector(".set-head p")?.textContent ?? null, action: action !== null && action.right <= box.right, actionGap: action === null ? null : box.right - action.right,
    sideways: group.scrollWidth > group.clientWidth, right: box.right,
  };
}

/** The focused element's id. Output: the id, or null. */
export const focused = () => document.activeElement?.id || null;

/** The center of an element. Input: the selector. Output: its coordinates. */
export const point = (selector) => { const box = rect(selector); return { x: box.left + box.width / 2, y: box.top + box.height / 2 }; };

/** Mounts a three-choice segmented control, the middle one checked, in a column of the given
 * width, as a narrow settings section draws it. Input: the column's width. */
export function mountSegmented(width) {
  mount(width);
  const options = [{ value: "one", label: "One worktree" }, { value: "clone", label: "Own clone each" }, { value: "shared", label: "Shared" }];
  flushSync(() => root.render(h("div", { style: { width: `${width}px` } }, h(Segmented, { label: "Workspace", value: "clone", options, onValue: () => {} }))));
}

/** Reads each segment of the mounted control. Output: per segment its box, its label's box,
 * how many lines its label takes and the group's box. */
export function pills() {
  const group = document.querySelector(".segmented").getBoundingClientRect();
  return [...document.querySelectorAll(".segmented button")].map((button) => {
    const range = document.createRange();
    range.selectNodeContents(button);
    const text = range.getBoundingClientRect();
    const box = button.getBoundingClientRect();
    return { box: { top: box.top, bottom: box.bottom, right: box.right }, text: { top: text.top, bottom: text.bottom }, lines: range.getClientRects().length, group: { bottom: group.bottom, right: group.right } };
  });
}
