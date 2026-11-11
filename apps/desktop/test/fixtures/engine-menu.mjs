import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";

export const events = [];
let root = null;

/** Mounts the real shell and sidebar with the real stylesheet and a restart control that
 * records each restart. Input: whether an update install holds the restart off, whether a
 * restart runs, the last failure's words and the engine state. Output: nothing. */
export function mount(options = {}) {
  root?.unmount();
  events.length = 0;
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = shell;
  document.head.append(style);
  document.documentElement.style.height = "100%";
  document.body.style.height = "100%";
  document.body.style.margin = "0";
  document.body.innerHTML = '<div id="root" style="height:100%"></div>';
  root = createRoot(document.getElementById("root"));
  update(options);
}

/** Draws the mounted sidebar again with a new restart state, keeping what the owner opened.
 * Input: the same state the mount takes. Output: nothing. */
export function update({ held = false, busy = false, failure = "", engine = "running" } = {}) {
  const restart = { held, busy, failure, onRestart: () => events.push("restart") };
  const sidebar = h(Sidebar, { route: { name: "home" }, projects: [], profileName: "Mira Okafor", engine, onRetry: () => events.push("retry"), restart });
  flushSync(() => root.render(h(AppShell, { sidebar }, h("p", null, "Body"))));
}

/** The screen centre of the first element a selector finds. Input: the selector. Output: the
 * point, or null when nothing matches. */
export function centerOf(selector) {
  const found = document.querySelector(selector);
  if (!found) return null;
  const box = found.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}

/** Reads the footer's menu, its confirm dialog and where focus is. Output: the reading. */
export function shown() {
  const menu = document.querySelector(".sidebar-foot [role=menu]");
  const item = menu?.querySelector("[role=menuitem]") ?? null;
  const dialog = document.querySelector("dialog[open]");
  const active = document.activeElement;
  return {
    menu: menu !== null,
    items: menu ? [...menu.querySelectorAll("[role=menuitem]")].map((entry) => entry.textContent.trim()) : [],
    disabled: item ? item.disabled : null,
    note: menu?.querySelector("p")?.textContent ?? null,
    dialog: dialog ? { title: dialog.querySelector("h2").textContent, words: dialog.querySelector(".dialog-body").textContent, buttons: [...dialog.querySelectorAll(".dialog-foot button")].map((button) => button.textContent.trim()) } : null,
    focus: active === document.body ? "body" : active.getAttribute("role") ?? active.className,
    status: document.querySelector(".sidebar-foot .engine-state").textContent.trim(),
    alert: document.querySelector(".sidebar-foot [role=alert]")?.textContent ?? null,
    events: [...events],
  };
}
