import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import { Dialog } from "../../src/ui/overlay.tsx";
import { AppShell, Sidebar } from "../../src/ui/shell.tsx";
import { USER_MENU_ITEMS } from "../../src/ui/sidebar-foot.tsx";

export const events = [];
let root = null;
let state = {};
let open = null;

/** Mounts the real shell and sidebar with the real stylesheet, a restart control that
 * records each restart and a user menu control whose items open a real dialog over the page.
 * Input: whether an update install holds the restart off, whether a restart runs, the last
 * failure's words, the engine state and whether the user menu is there. Output: nothing. */
export function mount(options = {}) {
  root?.unmount();
  events.length = 0;
  open = null;
  window.history.replaceState(null, "", "#/home");
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
export function update(options = {}) {
  state = options;
  draw();
}

/** Draws the shell from the kept state and the open user dialog. Output: nothing. */
function draw() {
  const { held = false, busy = false, failure = "", engine = "running", menu = true } = state;
  const restart = { held, busy, failure, onRestart: () => events.push("restart") };
  const control = menu ? { open, onOpen: (tab) => { events.push(`open:${tab}`); open = tab; draw(); } } : undefined;
  const sidebar = h(Sidebar, { route: { name: "home" }, projects: [], profileName: "Mira Okafor", engine, onRetry: () => events.push("retry"), restart, menu: control });
  const shut = () => { events.push(`close:${open}`); open = null; draw(); };
  const dialog = open === null ? null : h(Dialog, { open: true, title: USER_MENU_ITEMS[open].label, onClose: shut, wide: true }, h("p", null, `${USER_MENU_ITEMS[open].label} panel`));
  flushSync(() => root.render(h(AppShell, { sidebar }, h("p", { className: "page-body" }, "Body"), dialog)));
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
  const item = menu?.querySelector("[data-item=restart]") ?? null;
  const dialog = document.querySelector("dialog[open]");
  const active = document.activeElement;
  const name = document.querySelector(".sidebar-foot .me");
  return {
    menu: menu !== null,
    expanded: name.getAttribute("aria-expanded"),
    hash: window.location.hash,
    page: document.querySelector(".page-body")?.textContent ?? null,
    item: active?.dataset?.item ?? null,
    items: menu ? [...menu.querySelectorAll("[role=menuitem]")].map((entry) => entry.textContent.trim()) : [],
    disabled: item ? item.disabled : null,
    note: menu?.querySelector("p")?.textContent ?? null,
    dialog: dialog ? { title: dialog.querySelector("h2").textContent, words: dialog.querySelector(".dialog-body").textContent, buttons: [...dialog.querySelectorAll(".dialog-foot button")].map((button) => button.textContent.trim()) } : null,
    dialogs: document.querySelectorAll("dialog[open]").length,
    focus: active === document.body ? "body" : active.getAttribute("role") ?? active.className,
    status: document.querySelector(".sidebar-foot .engine-state").textContent.trim(),
    alert: document.querySelector(".sidebar-foot [role=alert]")?.textContent ?? null,
    events: [...events],
  };
}
