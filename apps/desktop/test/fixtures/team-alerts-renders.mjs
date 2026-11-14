import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { ALERT_READ_MS, useTeamAlerts } from "../../src/app.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const LOOKS_MAX = 64;
let events = null;
let tick = null;
let root = null;
let renders = 0;
const asked = [];
const realTimeout = window.setTimeout.bind(window);

window.setTimeout = (work, ms, ...rest) => {
  if (ms !== ALERT_READ_MS) return realTimeout(work, ms, ...rest);
  tick = work;
  return 1;
};

window.__TAURI_INTERNALS__ = {
  invoke: async (command) => {
    asked.push(command);
    if (command === "desktop_team_events") return events;
    if (command === "desktop_question_counts") return [];
    if (command === "desktop_system_notifications") return "available";
    if (command === "desktop_team_attention") return null;
    throw { code: "unknown", message: command };
  },
  transformCallback: () => 0,
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
const state = () => ({ renders, asked: [...asked], armed: tick !== null });

/** Starts the real team alert watch on a window bridge that answers its reads. Input: the
 * projects as `{ project: [unseen, agents] }`. Output: the renders, the reads and whether the
 * next look is armed. */
export async function start(projects) {
  events = teamEvents(projects);
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  await act(async () => { root.render(createElement(Probe)); await settle(); });
  return state();
}

/** Runs the watch's next timed looks, each reading the given projects. Inputs: the projects
 * and how many looks. Output: the renders, the reads and whether a next look is armed. */
export async function looks(projects, count) {
  if (!Number.isInteger(count) || count < 1 || count > LOOKS_MAX) throw new Error(`1 to ${LOOKS_MAX} looks`);
  events = teamEvents(projects);
  for (let index = 0; index < count; index += 1) {
    const work = tick;
    if (work === null) throw new Error("no look armed");
    tick = null;
    await act(async () => { work(); await settle(); });
  }
  return state();
}
