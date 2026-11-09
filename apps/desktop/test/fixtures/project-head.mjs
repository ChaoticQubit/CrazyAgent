import { createElement as h, useRef } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import { ProjectHead, useProjectHead } from "../../src/screens/project/head.tsx";
import { TABS } from "../../src/screens/project/index.tsx";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

const STILL = `
.pj-head, .pj-head * { transition: none !important; }
.pj-head .page-header, .pj-head.scrolled .page-header { padding: 20px 0 16px !important; }
.pj-head .page-header h1, .pj-head.scrolled .page-header h1 { font-size: 22px !important; line-height: 28px !important; }
`;

const ONE_TAB = [{ id: "overview", label: "Overview", href: "#/projects/p1/overview" }];

let root = null;

const ABOUT = "A booking system for a small flower shop, with online orders, delivery slots and a calendar the staff can edit from the counter. ".repeat(4);

/** A project page as the app draws one: the real head and its scroll hook over a body.
 * Inputs: the head's meta line, or null, its tabs, its description and the body's height in
 * px. Output: the page element. */
function Page({ meta, tabs, about = "", body = 2000 }) {
  const page = useRef(null);
  const scrolled = useProjectHead(page, "p1");
  return h("div", { className: "pj pj-project", ref: page },
    h(ProjectHead, { title: "Larkspur Booking", about, actions: null, meta, tabs, current: "overview", scrolled }),
    h("div", { id: "page-body", style: { height: `${body}px` } }, "Overview"));
}

function start(sheets, page) {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = sheets.join("\n");
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  const sidebar = h(Sidebar, { route: { name: "home" }, projects: [{ id: "p1", name: "Project 1" }], profileName: "Mira Okafor", engine: "running", onRetry: () => {} });
  flushSync(() => root.render(h(AppShell, { sidebar }, page)));
}

/** Mounts the real app shell with the real stylesheets around a project page whose head
 * takes its scrolled form with no transition and no change of size. Inputs: the head's meta
 * line, or none, and its tabs, one by default. */
export function mountStill(meta = null, tabs = ONE_TAB) {
  start([shell, projects, STILL], h(Page, { meta, tabs }));
}

/** Mounts the project page with its real styles and a description that wraps, on a page only a
 * little taller than the window. Input: how many px taller. Output: how far the page scrolls. */
export async function mountShort(over) {
  start([shell, projects], h(Page, { meta: null, tabs: ONE_TAB, about: ABOUT }));
  await frames(3);
  const main = document.querySelector(".main");
  const body = document.getElementById("page-body");
  body.style.height = `${body.offsetHeight + over - (main.scrollHeight - main.clientHeight)}px`;
  await frames(2);
  return main.scrollHeight - main.clientHeight;
}

/** Mounts the project page with every tab the real page lists. */
export function mountAllTabs() {
  mountStill(null, TABS.map((tab) => ({ ...tab, href: `#/projects/p1/${tab.id}` })));
}

export function frames(count = 2) {
  return new Promise((done) => {
    const step = (left) => (left === 0 ? done() : requestAnimationFrame(() => step(left - 1)));
    step(count);
  });
}

/** Reads the head's form and height, the room under the page and whether the meta line is
 * drawn. Output: the values. */
export function state() {
  const head = document.querySelector(".pj-head");
  const page = document.querySelector(".pj-project");
  return {
    head: head.className,
    height: head.offsetHeight,
    room: parseFloat(getComputedStyle(page).getPropertyValue("--pj-head-room")) || 0,
    top: document.querySelector(".main").scrollTop,
    meta: document.querySelector(".pj-head .pj-meta")?.getClientRects().length > 0,
  };
}

/** Reads the tab strip: each tab's word, icon and top, how wide the strip is and how much of
 * it the tabs use, and whether the strip, the page or the window scrolls sideways. Output: the
 * values in whole pixels. */
export function tabRow() {
  const strip = document.querySelector(".pj-head .tabs");
  const main = document.querySelector(".main");
  const tabs = [...strip.querySelectorAll(".tab")];
  return {
    words: tabs.map((tab) => tab.textContent),
    icons: tabs.filter((tab) => tab.querySelector(".icon")).length,
    tops: tabs.map((tab) => Math.round(tab.getBoundingClientRect().top)),
    strip: strip.clientWidth,
    used: Math.round(tabs.at(-1).getBoundingClientRect().right - tabs[0].getBoundingClientRect().left),
    sideways: strip.scrollWidth > strip.clientWidth || main.scrollWidth > main.clientWidth || document.documentElement.scrollWidth > window.innerWidth,
  };
}
