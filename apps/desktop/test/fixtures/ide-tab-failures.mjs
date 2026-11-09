import { createElement as h } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import css from "../../src/screens/project/ide.css?inline";
import { IdeTab, useCloseIde } from "../../src/screens/project/ide.tsx";

const READY = { state: { build: { state: "ready", version: "1.0.0", root: "/ide" }, unqualified: null }, platform: "macos" };
const CALL_WAIT_MS = 5_000;
const POLL_MS = 20;

let root = null;
let refused = [];
let absent = [];
let calls = [];
let reported = [];

window.__TAURI_INTERNALS__ = {
  async invoke(command) {
    calls.push(command);
    if (absent.includes(command)) throw `Command ${command} not found`;
    if (refused.includes(command)) throw { code: "ide_webview_failed" };
    return command === "desktop_ide_state" ? READY : null;
  },
};

function Page() {
  useCloseIde("project-one", (line) => reported.push(`close: ${line}`));
  return h(IdeTab, { projectId: "project-one", onView: () => {}, onHideProblem: (line) => reported.push(`hide: ${line}`) });
}

/** Opens the real IDE tab with its stylesheet and the native answers stubbed. Inputs: the
 * commands that refuse and the commands this build does not have. */
export function mount(refusing, missing = []) {
  refused = refusing;
  absent = missing;
  calls = [];
  reported = [];
  const style = document.createElement("style");
  style.textContent = css;
  document.head.append(style);
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(h(Page)));
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Waits for a native command to be asked for. Input: the command. Output: true when it
 * was, false when it never came. */
export async function called(command) {
  for (const deadline = Date.now() + CALL_WAIT_MS; Date.now() < deadline; await new Promise((done) => setTimeout(done, POLL_MS))) {
    if (calls.includes(command)) return true;
  }
  return false;
}

/** Closes the project page as leaving it does. Output: once the calls it makes are answered. */
export async function leave() {
  flushSync(() => root.unmount());
  await called("desktop_ide_hide");
  await called("desktop_ide_close");
}

/** What the owner is told. Output: the tab's alert text or null, and what reached the page. */
export function seen() {
  return { alert: document.querySelector("[role=alert]")?.textContent ?? null, reported };
}
