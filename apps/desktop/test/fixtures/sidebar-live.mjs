import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";
import { contrast, markContrast } from "./contrast.mjs";

export { contrast, markContrast };

let root = null;

/** Mounts the real shell and sidebar with the real stylesheet. Inputs: the project rows, the
 * theme and the route. */
export function mount(projects, theme, route = { name: "home" }) {
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
  const sidebar = h(Sidebar, { route, projects, profileName: "Mira Okafor", engine: "running", onRetry: () => {} });
  flushSync(() => root.render(h(AppShell, { sidebar }, h("p", null, "Body"))));
}

/** Reads where each project row's parts sit. Output: per row, the name's left edge and width,
 * the row's right edge, the count's and unseen count's right edges and the dot's size, null
 * for a part the row does not draw. */
export function rows() {
  return [...document.querySelectorAll(".nav-group .nav-item")].map((link) => {
    const name = link.querySelector(".name").getBoundingClientRect();
    const count = link.querySelector(".nav-live")?.getBoundingClientRect() ?? null;
    const flag = link.querySelector(".nav-flag")?.getBoundingClientRect() ?? null;
    const dot = link.querySelector(".nav-live-dot")?.getBoundingClientRect() ?? null;
    return {
      nameLeft: name.left, nameWidth: name.width, rowRight: link.getBoundingClientRect().right,
      countRight: count?.right ?? null, flagRight: flag?.right ?? null, dotWidth: dot?.width ?? null, dotHeight: dot?.height ?? null,
    };
  });
}

/** How far the project list reaches past its own width. Output: the pixels, zero when it fits. */
export function listOverflow() {
  const list = document.querySelector(".nav-group");
  return list.scrollWidth - list.clientWidth;
}

/** Paints the first project row as the pointer paints it. Input: none. Output: none. */
export function hoverFirstRow() {
  document.querySelector(".nav-group .nav-item").style.background = "var(--surface-2)";
}

/** The first row's dot and count fill, for a check that the row draws them at all. Output: the
 * dot's background and whether the count is present. */
export function firstRowParts() {
  const dot = document.querySelector(".nav-group .nav-live-dot");
  return { dot: dot ? getComputedStyle(dot).backgroundColor : null, count: document.querySelector(".nav-group .nav-live") !== null };
}
