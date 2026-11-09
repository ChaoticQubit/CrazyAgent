import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

let root = null;

/** Mounts the real shell and sidebar with the real stylesheet. Inputs: how many projects, the
 * route, the engine state, the theme, the profile name and the first project's flag. */
export function mount(count, route = { name: "home" }, engine = "running", theme = "light", profile = "Mira Okafor", flag = undefined) {
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
  const projects = Array.from({ length: count }, (_, at) => ({ id: `p${at}`, name: `Project ${at}`, flag: at === 0 ? flag : undefined }));
  const sidebar = h(Sidebar, { route, projects, profileName: profile, engine, onRetry: () => {} });
  flushSync(() => root.render(h(AppShell, { sidebar }, h("p", null, "Body"))));
}

const box = (selector) => {
  const rect = document.querySelector(selector).getBoundingClientRect();
  return { top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right };
};

/** Reads the sidebar's structure. Output: each nav's label and links, the direct children's
 * classes, the links marked as the current page and every link's address by its words. */
export function structure() {
  const aside = document.querySelector("aside.sidebar");
  return {
    children: [...aside.children].map((node) => node.className),
    navs: [...aside.querySelectorAll("nav")].map((nav) => ({
      name: nav.getAttribute("aria-label") ?? document.getElementById(nav.getAttribute("aria-labelledby"))?.textContent ?? null,
      links: [...nav.querySelectorAll("a")].map((link) => link.textContent),
    })),
    current: [...aside.querySelectorAll('a[aria-current="page"]')].map((link) => link.textContent),
    hrefs: Object.fromEntries([...aside.querySelectorAll("a")].map((link) => [link.textContent, link.getAttribute("href")])),
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

/** Reads the footer. Output: the initial and whether it is hidden from screen readers, the
 * profile and engine words, where each one sits, the footer's height and right edge, whether
 * the name is cut off and the Try again button's box, null when it is not shown. */
export function foot() {
  const avatar = document.querySelector(".sidebar-foot .avatar");
  const name = document.querySelector(".sidebar-foot .me");
  const engine = document.querySelector(".sidebar-foot .engine-state");
  const footer = document.querySelector(".sidebar-foot").getBoundingClientRect();
  const retry = document.querySelector(".sidebar-foot button")?.getBoundingClientRect() ?? null;
  return {
    initial: avatar?.textContent ?? null, hidden: avatar?.getAttribute("aria-hidden") ?? null, name: name.textContent, engine: engine.textContent,
    initialRight: avatar?.getBoundingClientRect().right ?? null, nameLeft: name.getBoundingClientRect().left,
    nameRight: name.getBoundingClientRect().right, nameBottom: name.getBoundingClientRect().bottom,
    nameCut: name.scrollWidth > name.clientWidth, engineTop: engine.getBoundingClientRect().top,
    engineHeight: engine.getBoundingClientRect().height, footHeight: footer.height, footRight: footer.right,
    retry: retry && { left: retry.left, right: retry.right, width: retry.width, height: retry.height },
  };
}

/** The words of the focused element. Output: the text. */
export function focused() {
  return document.activeElement?.textContent?.trim() ?? "";
}

/** Reads the focused link's ring against the scrolling list that would clip it. Output: the
 * link's words, the ring's style and width, and how far it reaches past each side of the
 * list, zero or less when it stays inside. */
export function ring() {
  const node = document.activeElement;
  const style = getComputedStyle(node);
  const reach = parseFloat(style.outlineWidth) + parseFloat(style.outlineOffset);
  const link = node.getBoundingClientRect();
  const list = node.closest(".nav-group").getBoundingClientRect();
  return {
    text: node.textContent.trim(), style: style.outlineStyle, width: parseFloat(style.outlineWidth),
    past: [list.left - (link.left - reach), link.right + reach - list.right, list.top - (link.top - reach), link.bottom + reach - list.bottom],
  };
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
