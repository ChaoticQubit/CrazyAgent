import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { useChangeList } from "../../src/screens/changes/poll.ts";

export const EVERY = 300;
let root = null;
let asked = [];
let replies = [];
let open = 0;
let most = 0;
let shown = null;
let reread = null;

function read(projectId) {
  asked.push(projectId);
  open += 1;
  most = Math.max(most, open);
  return new Promise((done) => {
    replies.push((ids) => {
      open -= 1;
      done({ kind: "ok", value: ids.map((id) => ({ id })) });
    });
  });
}

function Probe({ id }) {
  const [list, again] = useChangeList(id, EVERY, read);
  reread = again;
  shown = list === undefined ? null : list.kind === "ok" ? list.value.map((set) => set.id).join(",") : list.kind;
  return createElement("p", null, shown ?? "loading");
}

async function settle(ms) {
  await new Promise((done) => setTimeout(done, ms));
}

function state() {
  return { asked: [...asked], open, most, shown };
}

export async function mount(id) {
  root?.unmount();
  asked = [];
  replies = [];
  open = 0;
  most = 0;
  shown = null;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(Probe, { id })));
  await settle(0);
  return state();
}

export async function move(id) {
  flushSync(() => root.render(createElement(Probe, { id })));
  await settle(0);
  return state();
}

export async function wait(ms) {
  await settle(ms);
  return state();
}

export async function reply(ids) {
  const answer = replies.shift();
  if (!answer) throw new Error("no change list read is waiting for a reply");
  answer(ids);
  await settle(0);
  return state();
}

export async function again() {
  reread();
  await settle(0);
  return state();
}

export async function unmount(ms) {
  root?.unmount();
  root = null;
  await settle(ms);
  return state();
}
