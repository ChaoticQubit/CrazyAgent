import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import intake from "../../src/screens/intake/intake.css?inline";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

let root = null;

const header = h("div", { className: "page-header" }, h("div", null, h("h1", null, "Projects"), h("p", null, "Every project the engine knows.")));

const screens = {
  home: () => header,
  intake: () => h("div", { className: "pj in-page" }, header, h("section", { className: "in-panel" }, "Found project")),
  new: () => h("div", { className: "pj pj-new" }, header, h("div", { className: "pj-new-body" },
    h("nav", { className: "pj-step-rail", "aria-label": "Steps" }, h("ol", null, h("li", null, "Basics"))),
    h("div", { id: "second" }, "Name and folder"))),
  settings: () => h("div", { className: "pj-settings" },
    h("nav", { className: "pj-subnav", "aria-label": "Project settings" }, h("button", { type: "button" }, "General")),
    h("div", { className: "pj-settings-body", id: "second" }, "General settings")),
};

/** Mounts the real app shell and sidebar with the real stylesheets around one screen body
 * shaped as that screen draws it. Input: the screen name. */
export function mountScreen(name) {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, intake].join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const sidebar = h(Sidebar, { route: { name: "home" }, projects: [{ id: "p1", name: "Project 1" }], profileName: "Mira Okafor", engine: "running", onRetry: () => {} });
  flushSync(() => root.render(h(AppShell, { sidebar }, screens[name]())));
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Measures the main area, its page, the screen body and a second column, rounded to whole
 * pixels. Output: the right edges, the page padding and the widths. */
export function boxes() {
  const main = document.querySelector(".main");
  const page = main.querySelector(".page");
  const body = page.firstElementChild;
  const second = document.getElementById("second");
  const left = main.getBoundingClientRect().left;
  return {
    windowWidth: window.innerWidth,
    mainRight: Math.round(left + main.clientWidth),
    mainWidth: Math.round(main.clientWidth),
    pageRight: Math.round(page.getBoundingClientRect().right),
    pagePadding: Math.round(parseFloat(getComputedStyle(page).paddingRight)),
    bodyRight: Math.round(body.getBoundingClientRect().right),
    second: second ? Math.round(second.getBoundingClientRect().width) : null,
  };
}
