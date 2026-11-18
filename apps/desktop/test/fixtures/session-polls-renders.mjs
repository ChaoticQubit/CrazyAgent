import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { useSession } from "../../src/state.ts";
import * as intake from "../../src/screens/intake/intake.ts";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const LOOKS_MAX = 64;
const PENDING_MAX = 8;
const POLL_PACES = new Set([intake.FOUND_POLL_MS, intake.IDLE_POLL_MS]);
const FIRST_POLL_ID = 1_000_000_000;
let found = [];
let job = null;
let pending = new Map();
let nextId = FIRST_POLL_ID;
let root = null;
let renders = 0;
const asked = [];
const realTimeout = window.setTimeout.bind(window);
const realClear = window.clearTimeout.bind(window);

window.setTimeout = (work, ms, ...rest) => {
  if (!POLL_PACES.has(ms)) return realTimeout(work, ms, ...rest);
  if (pending.size >= PENDING_MAX) throw new Error(`over ${PENDING_MAX} polls armed at once`);
  nextId += 1;
  pending.set(nextId, { work, ms });
  return nextId;
};

window.clearTimeout = (id) => {
  if (!pending.delete(id)) realClear(id);
};

window.__TAURI_INTERNALS__ = {
  invoke: async (command) => {
    asked.push(command);
    if (command === "desktop_connect") return { connected: true, pending: false, stuck_sign_in: null };
    if (command === "desktop_found_projects") return found.map((item) => ({ ...item }));
    if (command === "desktop_intake_status") return job === null ? [] : [JSON.parse(JSON.stringify(job))];
    throw { code: "unknown", message: command };
  },
  transformCallback: () => 0,
};

/** One folder the engine found in the projects folder and has not set up. Inputs: its name and
 * state. Output: the row as the bridge answers it. */
function folder([name, state]) {
  return { name, path: `/Users/owner/CrazyAgent/${name}`, git: true, known: false, state, project_id: null };
}

/** An intake job that waits for the owner's answer, as the bridge answers it. Input: its id,
 * null for none. Output: the job or null. */
function waiting(id) {
  return id === null ? null : { id, source: "/Users/owner/Downloads/app", mode: "copy", overrides: [], stopping: false, state: { state: "needs_answer", code: "folder_exists" } };
}

/** Stands for the app's whole tree under the session: counts each time it renders. */
function Tree({ session }) {
  renders += 1;
  return createElement("p", null, `${session.engine} ${session.found.kind} ${session.intake[0]?.id ?? "none"}`);
}

/** Holds the real session as the app does. Input: whether Home is shown. Output: the tree. */
function Probe({ home }) {
  return createElement(Tree, { session: useSession(home) });
}

const settle = () => new Promise((done) => realTimeout(done, 20));
const state = () => ({
  renders,
  asked: [...asked],
  armed: pending.size,
  paces: [...pending.values()].map((poll) => poll.ms).sort((a, b) => a - b),
});

/** Starts the real session on a window bridge whose engine runs, with the found folders and
 * the intake job given, on Home or another page. Inputs: the folders as [name, state], the job
 * id, null for none, and whether Home is shown. Output: the renders, the reads, how many polls
 * are armed and their waits. */
export async function start(folders, jobId, home = true) {
  found = folders.map(folder);
  job = waiting(jobId);
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  await act(async () => { root.render(createElement(Probe, { home })); await settle(); });
  return state();
}

/** Moves to Home or away from it, as the route does. Input: whether Home is shown. Output: the
 * renders, the reads, how many polls are armed and their waits. */
export async function show(home) {
  if (root === null) throw new Error("start first");
  await act(async () => { root.render(createElement(Probe, { home })); await settle(); });
  return state();
}

/** The app window takes the focus back, as when the owner returns from the TUI or Finder.
 * Output: the renders, the reads, how many polls are armed and their waits. */
export async function focus() {
  await act(async () => { window.dispatchEvent(new Event("focus")); await settle(); });
  return state();
}

/** Runs every armed found-folder and intake poll, as many rounds as asked, each reading the
 * folders and job given. Inputs: the folders, the job id and the rounds. Output: the renders,
 * the reads, how many polls are armed and their waits. */
export async function looks(folders, jobId, rounds) {
  if (!Number.isInteger(rounds) || rounds < 1 || rounds > LOOKS_MAX) throw new Error(`1 to ${LOOKS_MAX} rounds`);
  found = folders.map(folder);
  job = waiting(jobId);
  for (let round = 0; round < rounds; round += 1) {
    const due = [...pending.values()];
    if (due.length === 0) throw new Error("no poll armed");
    pending = new Map();
    await act(async () => { for (const poll of due) poll.work(); await settle(); });
  }
  return state();
}
