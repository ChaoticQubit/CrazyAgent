import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { useTeamAlerts } from "../../src/app.tsx";
import { COALESCE_MS } from "../../src/engine-changes.ts";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const LOOKS_MAX = 64;
let events = null;
let root = null;
let renders = 0;
let watch = null;
let sent = 0;
const asked = [];
const realTimeout = window.setTimeout.bind(window);
let hold = null;

window.setTimeout = (work, ms, ...rest) => {
  if (ms !== COALESCE_MS) return realTimeout(work, ms, ...rest);
  hold = work;
  return 1;
};

/** Ends the store's hold window at once, so each change the fixture sends reads on its own.
 * Output: none. */
function release() {
  const work = hold;
  hold = null;
  work?.();
}

window.__TAURI_INTERNALS__ = {
  invoke: async (command) => {
    asked.push(command);
    if (command === "desktop_watch") return null;
    if (command === "desktop_team_events") return events;
    if (command === "desktop_question_counts") return [];
    if (command === "desktop_system_notifications") return "available";
    if (command === "desktop_team_attention") return null;
    throw { code: "unknown", message: command };
  },
  transformCallback: (heard) => {
    if (watch !== null) throw new Error("one channel, the page's watch");
    watch = heard;
    return 1;
  },
};

/** The app's team events as the engine answers them: each project's unseen count and agents at
 * work, no notifications. Input: `{ project: [unseen, agents] }`. Output: the events. */
function teamEvents(projects) {
  const rows = Object.entries(projects);
  return {
    projects: rows.map(([project_id, [unseen]]) => ({ project_id, unseen, questions: 0 })),
    working: rows.map(([project_id, [, agents]]) => ({ project_id, agents })),
    notifications: [],
  };
}

/** Stands for the app's whole tree under the root hook: counts each time it renders. */
function Tree({ alerts, working }) {
  renders += 1;
  return createElement("p", null, `${alerts.size} ${working.size}`);
}

function Probe() {
  const { alerts, working } = useTeamAlerts(true, () => undefined);
  return createElement(Tree, { alerts, working });
}

const settle = () => new Promise((done) => realTimeout(done, 20));
const state = () => ({ renders, asked: [...asked], watching: watch !== null });

/** Starts the real team alert watch on a window bridge that answers its reads. Input: the
 * projects as `{ project: [unseen, agents] }`. Output: the renders, the reads and whether the
 * page watches the engine. */
export async function start(projects) {
  events = teamEvents(projects);
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  await act(async () => { root.render(createElement(Probe)); await settle(); });
  return state();
}

/** Sends team changes on the page's watch channel, one look each, each reading the given
 * projects. Inputs: the projects and how many looks. Output: the renders, the reads and
 * whether the page watches the engine. */
export async function looks(projects, count) {
  if (!Number.isInteger(count) || count < 1 || count > LOOKS_MAX) throw new Error(`1 to ${LOOKS_MAX} looks`);
  if (watch === null) throw new Error("the page never watched");
  events = teamEvents(projects);
  for (let index = 0; index < count; index += 1) {
    const message = { index: sent, message: { topics: ["team"] } };
    sent += 1;
    await act(async () => { watch(message); release(); await settle(); });
  }
  return state();
}
