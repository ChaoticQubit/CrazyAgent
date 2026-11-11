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
let places = [];
let heldPlaces = null;
let out = false;
let leftOver = null;
let held = {};
let windowAsks = [];

function answer(command) {
  if (command === "desktop_ide_window_open") out = true;
  if (command === "desktop_ide_window_out") return out;
  if (command === "desktop_ide_open") return { ownWindow: out, left: leftOver };
  return command === "desktop_ide_state" ? READY : null;
}

window.__TAURI_INTERNALS__ = {
  async invoke(command, args) {
    calls.push(command);
    if (command === "desktop_ide_place") places.push(args.bounds);
    if (command === "desktop_ide_window_open") windowAsks.push(args);
    if (absent.includes(command)) throw `Command ${command} not found`;
    if (refused.includes(command)) throw { code: "ide_webview_failed" };
    if (command === "desktop_ide_place" && heldPlaces) return new Promise((answer, refuse) => heldPlaces.push({ answer, refuse }));
    if (held[command]) return new Promise((done) => held[command].push(() => done(answer(command))));
    return answer(command);
  },
};

function Page() {
  useCloseIde("project-one", (line) => reported.push(`close: ${line}`));
  return h(IdeTab, { projectId: "project-one", onView: () => {}, onHideProblem: (line) => reported.push(`hide: ${line}`) });
}

/** Opens the real IDE tab with its stylesheet and the native answers stubbed. Inputs: the
 * commands that refuse, the commands this build does not have, whether each place waits
 * for [`answerPlace`], whether the IDE already sits in its own window and what the open says
 * the last IDE left running. */
export function mount(refusing, missing = [], holdPlaces = false, startsOut = false, left = null) {
  refused = refusing;
  leftOver = left;
  absent = missing;
  calls = [];
  reported = [];
  places = [];
  heldPlaces = holdPlaces ? [] : null;
  out = startsOut;
  held = {};
  windowAsks = [];
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

/** Moves the IDE panel down as a banner above it does. Input: the pixels. Output: once the
 * tab has measured it. */
export function moveTo(offset) {
  document.querySelector(".ide-panel").style.transform = `translateY(${offset}px)`;
  return frames();
}

/** Waits for a native command to be asked for a number of times. Inputs: the command and the
 * count. Output: true when it got there, false when it never did. */
export async function calledTimes(command, times) {
  for (const deadline = Date.now() + CALL_WAIT_MS; Date.now() < deadline; await new Promise((done) => setTimeout(done, POLL_MS))) {
    if (count(command) >= times) return true;
  }
  return false;
}

/** How many times a native command was asked for. Input: the command. Output: the count. */
export function count(command) {
  return calls.filter((name) => name === command).length;
}

/** Holds a native command's answers until [`release`]. Input: the command. */
export function hold(command) {
  held[command] = [];
}

/** Answers everything held for a native command. Input: the command. Output: once the tab has
 * had the answers. */
export function release(command) {
  const waiting = held[command];
  delete held[command];
  for (const send of waiting) send();
  return frames();
}

/** Presses the button with these words, as many times as asked, in one go. Inputs: the words
 * and the presses. Output: once the tab has drawn. */
export function press(words, times = 1) {
  const button = [...document.querySelectorAll("button")].find((node) => node.textContent.trim() === words);
  if (!button) throw new Error(`no button says ${words}: ${document.body.textContent}`);
  for (let at = 0; at < times; at += 1) button.click();
  return frames();
}

/** Closes the IDE's own window as the owner does. */
export function closeWindow() {
  out = false;
}

/** What the window button sent. Output: its arguments, oldest first. */
export function asks() {
  return windowAsks;
}

/** What the tab shows. Output: its text, its button words and whether the panel is hidden. */
export function tab() {
  return {
    text: document.querySelector(".ide-tab").textContent,
    buttons: [...document.querySelectorAll("button")].map((node) => node.textContent.trim()),
    panelHidden: document.querySelector(".ide-panel").hidden,
  };
}

/** The boxes the tab asked the window to place. Output: them, oldest first. */
export function placed() {
  return places;
}

/** Answers the oldest held place. Output: once the tab has had the answer. */
export function answerPlace() {
  heldPlaces.shift()?.answer(null);
  return frames();
}

/** Refuses the oldest held place. Output: once the tab has had the refusal. */
export function refusePlace() {
  heldPlaces.shift()?.refuse({ code: "ide_webview_failed" });
  return frames();
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
