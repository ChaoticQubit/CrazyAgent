import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { useLoad } from "../../src/state.ts";

export const renders = [];
let root = null;
let answer = null;

function Probe({ id, cache }) {
  const [loaded] = useLoad(() => new Promise((done) => { answer = done; }), [], cache);
  const shown = loaded.kind === "ok" ? loaded.value : loaded.kind;
  renders.push(`${id}:${shown}`);
  return createElement("p", null, shown);
}

export function mount(id, cache) {
  root?.unmount();
  renders.length = 0;
  answer = null;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(Probe, { id, cache })));
  return [...renders];
}

export async function reply(result) {
  await new Promise((done) => setTimeout(done, 0));
  if (!answer) throw new Error("the probe never started its read");
  answer(result);
  for (let turn = 0; turn < 5; turn += 1) await new Promise((done) => setTimeout(done, 0));
  return { renders: [...renders], text: document.querySelector("p")?.textContent ?? null };
}

let twinReads = [];
let reloaders = {};

function Twin({ id, cache }) {
  const [loaded, reload] = useLoad(() => new Promise((done) => { twinReads.push(done); }), [], cache);
  reloaders[id] = reload;
  return createElement("p", { id }, loaded.kind === "ok" ? loaded.value : loaded.kind);
}

async function settle() {
  for (let turn = 0; turn < 5; turn += 1) await new Promise((done) => setTimeout(done, 0));
}

function twinTexts() {
  return { reads: twinReads.length, x: document.getElementById("x")?.textContent ?? null, y: document.getElementById("y")?.textContent ?? null };
}

export async function mountTwins(cache) {
  root?.unmount();
  twinReads = [];
  reloaders = {};
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement("div", null, createElement(Twin, { id: "x", cache }), createElement(Twin, { id: "y", cache }))));
  await settle();
  return twinTexts();
}

export async function answerTwin(index, value) {
  twinReads[index]({ kind: "ok", value });
  await settle();
  return twinTexts();
}

export async function reloadTwin(id) {
  flushSync(() => reloaders[id]());
  await settle();
  return twinTexts();
}
