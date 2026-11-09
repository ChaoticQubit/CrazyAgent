import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

let root = null;

/** Mounts the real shell and sidebar with the real stylesheet. Inputs: how many projects, the
 * route, the engine state and the theme. */
export function mount(count, route = { name: "home" }, engine = "running", theme = "light") {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = shell;
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.height = "100%";
  document.body.style.height = "100%";
  document.body.style.margin = "0";
  document.body.innerHTML = '<div id="root" style="height:100%"></div>';
  root = createRoot(document.getElementById("root"));
  const projects = Array.from({ length: count }, (_, at) => ({ id: `p${at}`, name: `Project ${at}` }));
  const sidebar = h(Sidebar, { route, projects, profileName: "Mira Okafor", engine, onRetry: () => {} });
  flushSync(() => root.render(h(AppShell, { sidebar }, h("p", null, "Body"))));
}

const box = (selector) => {
  const rect = document.querySelector(selector).getBoundingClientRect();
  return { top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right };
};

/** Reads the sidebar's structure. Output: each nav's label and links, the direct children's
 * classes and the links marked as the current page. */
export function structure() {
  const aside = document.querySelector("aside.sidebar");
  return {
    children: [...aside.children].map((node) => node.className),
    navs: [...aside.querySelectorAll("nav")].map((nav) => ({
      name: nav.getAttribute("aria-label") ?? document.getElementById(nav.getAttribute("aria-labelledby"))?.textContent ?? null,
      links: [...nav.querySelectorAll("a")].map((link) => link.textContent),
    })),
    current: [...aside.querySelectorAll('a[aria-current="page"]')].map((link) => link.textContent),
  };
}

/** Reads where the sidebar's parts sit and how they scroll. Output: the boxes and scroll sizes. */
export function layout() {
  const list = document.querySelector(".nav-group");
  const aside = document.querySelector("aside.sidebar");
  const divider = getComputedStyle(document.querySelector(".app-nav"));
  return {
    list: box(".nav-group"), app: box(".app-nav"), foot: box(".sidebar-foot"), window: innerHeight,
    listScrolls: list.scrollHeight > list.clientHeight, asideScrolls: aside.scrollHeight > aside.clientHeight,
    divider: [divider.borderTopWidth, divider.borderTopStyle],
  };
}

/** The words of the focused element. Output: the text. */
export function focused() {
  return document.activeElement?.textContent?.trim() ?? "";
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
