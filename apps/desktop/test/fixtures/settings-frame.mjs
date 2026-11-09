import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import frame from "../../src/screens/settings/settings.css?inline";
import { SettingsFrame } from "../../src/screens/settings/index.tsx";
import { AppShell } from "../../src/ui/shell.tsx";

let root = null;

/** Mounts the real Settings frame in the real shell with a tall panel. Inputs: the open section
 * and the theme. */
export function mount(tab, theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, frame].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.height = "100%";
  document.body.style.height = "100%";
  document.body.style.margin = "0";
  document.body.innerHTML = '<div id="root" style="height:100%"></div>';
  root = createRoot(document.getElementById("root"));
  const panel = h("div", { id: "panel", style: { height: "2400px" } }, h("button", { id: "in-panel", type: "button" }, "In the panel"));
  flushSync(() => root.render(h(AppShell, { sidebar: h("nav", { className: "sidebar" }) }, h(SettingsFrame, { tab }, panel))));
}

/** Reads the frame. Output: each group's name and links, the current links, how many tab roles
 * are left and the page heading. */
export function structure() {
  const nav = document.querySelector('nav[aria-label="Settings sections"]');
  return {
    groups: [...nav.querySelectorAll('[role="group"]')].map((group) => ({
      name: document.getElementById(group.getAttribute("aria-labelledby"))?.textContent,
      links: [...group.querySelectorAll("a")].map((link) => link.textContent),
    })),
    current: [...nav.querySelectorAll('a[aria-current="page"]')].map((link) => link.textContent),
    tabs: document.querySelectorAll('[role="tablist"], [role="tab"], [role="tabpanel"]').length,
    heading: document.querySelector("h1")?.textContent,
  };
}

/** Reads where the list and the panel sit. Output: their edges and whether the main region
 * scrolls sideways. */
export function layout() {
  const main = document.querySelector(".main");
  const nav = document.querySelector(".settings-nav").getBoundingClientRect();
  const panel = document.querySelector(".settings-content").getBoundingClientRect();
  return { navRight: nav.right, navTop: nav.top, panelLeft: panel.left, panelTop: panel.top, sideways: main.scrollWidth > main.clientWidth };
}

/** Reads the focused element. Output: its words and its outline style. */
export function focused() {
  const node = document.activeElement;
  return { text: node?.textContent?.trim() ?? "", outline: node ? getComputedStyle(node).outlineStyle : "" };
}

const channels = (color) => color.match(/[\d.]+/g).slice(0, 3).map(Number);
const lit = (value) => { const part = value / 255; return part <= 0.03928 ? part / 12.92 : ((part + 0.055) / 1.055) ** 2.4; };
const luminance = (color) => { const [red, green, blue] = channels(color).map(lit); return 0.2126 * red + 0.7152 * green + 0.0722 * blue; };
const backdrop = (node) => {
  for (let at = node; at; at = at.parentElement) {
    const color = getComputedStyle(at).backgroundColor;
    if (!/rgba\(.*, 0\)|transparent/u.test(color)) return color;
  }
  return "rgb(255, 255, 255)";
};

/** The contrast ratio of one element's text on its background. Input: the selector. Output:
 * the ratio. */
export function contrast(selector) {
  const node = document.querySelector(selector);
  const [high, low] = [luminance(getComputedStyle(node).color), luminance(backdrop(node))].sort((one, two) => two - one);
  return (high + 0.05) / (low + 0.05);
}
