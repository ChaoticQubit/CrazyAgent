import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { AppView } from "../../src/app.tsx";
import { useSession } from "../../src/state.ts";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const PROJECTS = [{ id: "p-alpha", name: "Alpha", repository_count: 1 }];
const HELD_MAX = 8;
const asked = [];
let held = [];
let owner = null;
let route = { name: "home" };
let session = null;
let root = null;

window.__TAURI_INTERNALS__ = {
  plugins: {},
  transformCallback: () => 0,
  invoke: async (command) => {
    asked.push(command);
    if (command === "desktop_connect") return { connected: true, pending: false, stuck_sign_in: null };
    if (command === "desktop_owner_profile") return owner;
    if (command === "desktop_project_list") {
      if (held.length >= HELD_MAX) throw new Error(`over ${HELD_MAX} project lists held`);
      return new Promise((done) => held.push(() => done({ items: PROJECTS.map((project) => ({ ...project })), more: false })));
    }
    throw `Command ${command} not found`;
  },
};

/** The app's own view under the real session, as App renders it, without App's root watches. */
function Probe() {
  session = useSession();
  return createElement(AppView, { session, route });
}

const settle = () => new Promise((done) => setTimeout(done, 20));

/** What the page asked the bridge since a mark. Input: the mark. Output: the commands in order. */
function since(mark) {
  if (!(Number.isInteger(mark) && mark >= 0 && mark <= asked.length)) throw new Error(`a mark is a past count: ${mark}`);
  return { asked: asked.slice(mark), held: held.length };
}

/** Launches the app's view on a bridge whose engine runs and answers the profile at once, while
 * the project list waits until release. Inputs: the page, "home" or "updates", and whether the
 * owner has a profile. Output: the commands asked and how many project lists wait. */
export async function launch(page, named) {
  if (root !== null) throw new Error("one launch a page");
  if (page !== "home" && page !== "updates") throw new Error(`no page ${page}`);
  route = page === "updates" ? { name: "settings", tab: "updates" } : { name: "home" };
  owner = named ? { name: "Mira Okafor" } : null;
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  const mark = asked.length;
  await act(async () => { root.render(createElement(Probe)); await settle(); });
  return since(mark);
}

/** Answers every project list that waits. Output: the commands asked after it and how many
 * project lists still wait. */
export async function release() {
  if (held.length === 0) throw new Error("no project list waits");
  const due = held;
  held = [];
  const mark = asked.length;
  await act(async () => { for (const done of due) done(); await settle(); });
  return since(mark);
}

/** Connects again, as Try again and an engine restart do. Output: the commands asked after it
 * and how many project lists wait. */
export async function retry() {
  if (session === null) throw new Error("no session yet");
  const mark = asked.length;
  await act(async () => { session.retry(); await settle(); });
  return since(mark);
}
