import { act, createElement } from "react";
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
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

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

async function acted(work) {
  await act(async () => {
    work();
    await settle(0);
  });
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
  await acted(() => root.render(createElement(Probe, { id })));
  return state();
}

export async function move(id) {
  await acted(() => root.render(createElement(Probe, { id })));
  return state();
}

export async function wait(ms) {
  await settle(ms);
  return state();
}

export async function reply(ids) {
  const answer = replies.shift();
  if (!answer) throw new Error("no change list read is waiting for a reply");
  await acted(() => answer(ids));
  return state();
}

export async function again() {
  await acted(() => reread());
  return state();
}

export async function unmount(ms) {
  await acted(() => root?.unmount());
  root = null;
  await settle(ms);
  return state();
}
