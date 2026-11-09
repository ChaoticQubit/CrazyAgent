import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";
import frame from "../../src/screens/settings/settings.css?inline";
import { Settings } from "../../src/screens/settings/index.tsx";
import { SessionContext } from "../../src/state.ts";

export { contrast } from "./contrast.mjs";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const PAGE = '<div class="shell"><nav class="sidebar"></nav><main class="main" id="main" tabindex="-1"><div class="page" id="mount"></div></main></div>';
const FAILURE = { code: "engine_failed" };
const member = (index, kind = "chatgpt") => ({ id: `acct-${index}`, kind, provider: "chatgpt", status: "signed_in", status_label: "Signed in" });
const account = (index) => ({ id: `acct-${index}`, name: `Account ${index}`, provider: "chatgpt", provider_name: "ChatGPT", method: "subscription", status: "signed_in", status_label: "Signed in" });
const MAIN = { id: "pool-main", name: "Main pool", summary: "3 ChatGPT", members: [member(0, "api_key"), member(1), member(2)], project_count: 2 };
const SPARE = { id: "pool-spare", name: "Spare", summary: "1 ChatGPT", members: [member(3)], project_count: 0 };
const FULL = { ...MAIN, members: Array.from({ length: 15 }, (_, index) => member(index)) };
const LATE = { status: "not_signed_in", status_label: "Not signed in" };
const WAITING = { ...MAIN, members: [member(0, "api_key"), { ...member(1), ...LATE }, member(2)] };
const POOLS = { ok: [MAIN, SPARE], full: [FULL], waiting: [WAITING], empty: [] };
const SIGNED_IN = Array.from({ length: 18 }, (_, index) => account(index));
const ACCOUNTS = { ok: SIGNED_IN, waiting: SIGNED_IN.map((item, index) => (index === 1 ? { ...item, ...LATE } : item)), empty: [] };
let root = null;
let sent = [];

/** Lets the engine's answers and the renders they cause land. Input: the wait in
 * milliseconds. Output: nothing. */
export async function settle(ms) {
  await act(async () => { await new Promise((done) => setTimeout(done, ms)); });
}

/** Answers one engine command for a scene. Inputs: the scene and the command. Output: the
 * answer, a failure thrown, or a read that never lands. */
function answer(scene, command) {
  const failing = (state, list) => {
    if (state === "error") throw FAILURE;
    if (state === "missing") throw `Command ${command} not found`;
    return state === "pending" ? new Promise(() => {}) : list[state];
  };
  if (command === "desktop_pool_summaries") return failing(scene.pools ?? "ok", POOLS);
  if (command === "desktop_list_accounts") {
    const accounts = failing(scene.accounts ?? "ok", ACCOUNTS);
    return accounts instanceof Promise ? accounts : { accounts, next: null };
  }
  if (command === "desktop_inspect_model_access" || command === "desktop_model_access_page") throw FAILURE;
  throw `Command ${command} not found`;
}

/** Mounts the real Settings page on Pools against a fake engine. Inputs: the scene, naming the
 * pools and accounts reads as ok, full, waiting, empty, error, missing or pending, and the theme.
 * Output: nothing. */
export async function mount(scene = {}, theme = "light") {
  sent = [];
  window.__TAURI_INTERNALS__ = {
    plugins: {},
    transformCallback: () => 0,
    async invoke(command) { sent.push(command); return answer(scene, command); },
  };
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects, frame].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.style.margin = "0";
  document.body.innerHTML = PAGE;
  root = createRoot(document.getElementById("mount"));
  const session = { noteResult: (result) => result, showToast: () => {} };
  await act(async () => {
    root.render(createElement(SessionContext.Provider, { value: session }, createElement(Settings, { tab: "pools" })));
  });
  await settle(60);
}

const words = (node) => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
const nameOf = (node) => node.getAttribute("aria-label") ?? words(node);
const firstText = (node) => [...(node?.childNodes ?? [])].find((part) => part.nodeType === Node.TEXT_NODE)?.textContent ?? words(node);

/** Reads the tab's top line. Output: its heading, purpose and action names, or null. */
export function top() {
  const node = document.querySelector(".settings-top");
  if (!node) return null;
  return { heading: words(node.querySelector("h2")), purpose: words(node.querySelector("p")), actions: [...node.querySelectorAll("button, a")].map(words) };
}

/** Reads the tab's sections. Output: each one's heading, changed mark, purpose line, box and
 * rows with their words, edges and button names. */
export function sections() {
  return [...document.querySelectorAll(".set-group")].map((group) => {
    const box = group.getBoundingClientRect();
    return {
      heading: firstText(group.querySelector("h3")).trim(), mark: words(group.querySelector("h3 .set-changed")) || null, about: words(group.querySelector(".set-head p")),
      left: box.left, right: box.right, top: box.top,
      rows: [...group.querySelectorAll(".set-row")].map((row) => ({
        label: firstText(row.querySelector(".set-label")).trim(), mark: words(row.querySelector(".set-changed")) || null, help: words(row.querySelector(".set-help")),
        labelRight: row.querySelector(".set-label").getBoundingClientRect().right, controlLeft: row.querySelector(".set-ctl").getBoundingClientRect().left,
        buttons: [...row.querySelectorAll("button")].map(nameOf), visible: [...row.querySelectorAll("button")].map(words),
      })),
    };
  });
}

/** Reads what the page says. Output: its words on one line. */
export function text() {
  return document.querySelector("#mount").innerText.replace(/\s+/g, " ").trim();
}

/** The commands the page has sent. Output: their names. */
export function commands() {
  return [...sent];
}

/** Counts what a selector finds. Input: the selector. Output: the count. */
export function count(selector) {
  return document.querySelectorAll(selector).length;
}

/** Reads the first account line. Output: its words and the edges of its name and checkbox. */
export function pick() {
  const row = document.querySelector(".pick");
  return { words: words(row), nameRight: row.querySelector("span").getBoundingClientRect().right, boxLeft: row.querySelector("input").getBoundingClientRect().left };
}

/** Reads the tab's alert. Output: its words and whether it sits below the top line. */
export function alert() {
  const node = document.querySelector('#mount [role="alert"]');
  const head = document.querySelector(".settings-top");
  return { words: words(node), belowTop: Boolean(node && head && head.getBoundingClientRect().bottom <= node.getBoundingClientRect().top) };
}

/** Reads one control. Input: the selector. Output: whether it is marked invalid, or null when
 * it is not there. */
export function invalid(selector) {
  return document.querySelector(selector)?.getAttribute("aria-invalid") ?? null;
}

/** The focused element's accessible name and id. Output: both. */
export function focused() {
  const node = document.activeElement;
  return { name: node ? nameOf(node) : "", id: node?.id ?? "" };
}

/** The screen centre of the element a selector finds with the given words, scrolled into
 * view. Inputs: the selector and the words, or null for the first match. Output: the point,
 * or null when nothing matches. */
export function boxOf(selector, wanted) {
  const found = [...document.querySelectorAll(selector)].find((element) => wanted === null || words(element) === wanted);
  if (!found) return null;
  found.scrollIntoView({ block: "center" });
  const box = found.getBoundingClientRect();
  return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
}
