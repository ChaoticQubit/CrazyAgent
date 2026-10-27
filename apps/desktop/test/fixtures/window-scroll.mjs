import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import styles from "../../src/style.css?inline";
import { AppView, EngineOutdatedView } from "../../src/app.tsx";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

let root = null;

function session(patch) {
  const forbidden = () => { throw new Error("the layout check must not call the engine"); };
  return {
    engine: "running", unconfirmed: false, profile: { kind: "ok", value: { name: "Mira Okafor" } }, projects: { kind: "ok", value: [] },
    toast: "", revision: 1, retry: forbidden, checkUnconfirmed: forbidden, refreshProfile: forbidden, refreshProjects: forbidden,
    showToast: forbidden, noteResult: (result) => result, ...patch,
  };
}

function mount(element) {
  root?.unmount();
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = styles;
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(element));
}

const projects = Array.from({ length: 60 }, (_, index) => ({ id: `p${index}`, name: `Project ${index}` }));
const rows = Array.from({ length: 80 }, (_, index) => createElement("p", { key: index, id: index === 79 ? "last" : undefined }, `Row ${index}`));

const screens = {
  shell: () => createElement(AppShell, {
    sidebar: createElement(Sidebar, { route: { name: "home" }, projects, profileName: "Mira Okafor", engine: "running", onRetry: () => {} }),
  }, rows),
  launching: () => createElement(AppView, { session: session({ engine: "connecting" }), route: { name: "home" } }),
  down: () => createElement(AppView, { session: session({ engine: "down" }), route: { name: "home" } }),
  outdated: () => createElement(EngineOutdatedView, {
    versions: { kind: "ok", value: { engine: "0.1.0", app: "0.2.0", message: "Engine is version 0.1.0, this app is 0.2.0; restart the engine" } },
    restart: { kind: "idle" }, onRestart: () => {}, onRetry: () => {},
  }),
  updates: () => createElement(AppView, { session: session({ engine: "down" }), route: { name: "settings", tab: "updates" } }),
  welcome: () => createElement(AppView, { session: session({ profile: { kind: "ok", value: null } }), route: { name: "home" } }),
};

export function mountScreen(name) {
  mount(screens[name]());
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

export function pageScroll() {
  const rootElement = document.getElementById("root");
  return [document.documentElement.scrollTop, document.body.scrollTop, rootElement.scrollTop, window.scrollY];
}

export function rootStyles() {
  return [document.documentElement, document.body, document.getElementById("root")].map((element) => {
    const style = getComputedStyle(element);
    return { overflow: style.overflowY, overscroll: style.overscrollBehaviorY, height: Math.round(element.getBoundingClientRect().height) };
  });
}

export function lastOf(selector) {
  const all = document.querySelectorAll(selector);
  const element = all[all.length - 1];
  if (!element) return null;
  element.setAttribute("data-scroll-target", "");
  return element.tagName;
}

export function reach(scrollerSelector) {
  const scroller = document.querySelector(scrollerSelector);
  const target = document.querySelector("[data-scroll-target]");
  if (!scroller || !target) return { missing: true };
  const box = scroller.getBoundingClientRect();
  const top = Math.max(box.top, 0);
  const bottom = Math.min(box.bottom, window.innerHeight);
  const item = target.getBoundingClientRect();
  return {
    x: Math.round(box.left + box.width / 2), y: Math.round((top + bottom) / 2),
    visible: item.top >= top - 1 && item.bottom <= bottom + 1 && item.bottom <= window.innerHeight + 1,
    overflows: scroller.scrollHeight > scroller.clientHeight,
  };
}

export function placement(selector) {
  const box = document.querySelector(selector).getBoundingClientRect();
  return { left: Math.round(box.left), right: Math.round(window.innerWidth - box.right), top: Math.round(box.top), bottom: Math.round(window.innerHeight - box.bottom) };
}
