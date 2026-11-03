import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { useProjectFiles } from "../../src/screens/project/project-files.ts";

let root = null;
let project = "alpha";
let asked = [];
let replies = [];
let open = 0;
let most = 0;
let shown = null;

function read(id, query) {
  asked.push(`${id}:${query}`);
  open += 1;
  most = Math.max(most, open);
  return new Promise((done) => {
    replies.push((files) => {
      open -= 1;
      done({ kind: "ok", value: files });
    });
  });
}

function Probe({ id, query }) {
  const files = useProjectFiles(id, query, read);
  shown = files === null ? null : [...files];
  return createElement("p", null, files === null ? "none" : files.join(","));
}

async function settle(ms) {
  await new Promise((done) => setTimeout(done, ms));
}

function state() {
  return { asked: [...asked], open, most, shown };
}

export async function mount(id, query) {
  root?.unmount();
  project = id;
  asked = [];
  replies = [];
  open = 0;
  most = 0;
  shown = null;
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
  flushSync(() => root.render(createElement(Probe, { id, query })));
  await settle(0);
  return state();
}

export async function type(query, ms) {
  flushSync(() => root.render(createElement(Probe, { id: project, query })));
  await settle(ms);
  return state();
}

export async function reply(files, ms) {
  const answer = replies.shift();
  if (!answer) throw new Error("no file read is waiting for a reply");
  answer(files);
  await settle(ms);
  return state();
}
