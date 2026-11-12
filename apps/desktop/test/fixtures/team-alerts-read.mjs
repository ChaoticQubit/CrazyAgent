import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { ALERT_READ_MS, useTeamAlerts } from "../../src/app.tsx";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let reply = null;
let tick = null;
let shown = null;
let root = null;
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
    if (command === "desktop_question_counts") return [];
    if (command === "desktop_team_attention") return null;
    if (command === "desktop_team_events" && reply !== null) return reply;
    throw { code: "unknown", message: command };
  },
  transformCallback: () => 0,
};

function Probe({ running }) {
  const { alerts, working } = useTeamAlerts(running, () => {});
  shown = { alerts: [...alerts], working: [...working] };
  return createElement("p", null, "probe");
}

const settle = () => new Promise((done) => realTimeout(done, 20));
const state = () => ({ asked: [...asked], shown, armed: tick !== null });

/** Starts the real alert hook against a window bridge that answers its reads. Input: the app
 * events the engine answers with. Output: the reads made and the counts shown. */
export async function start(events) {
  reply = events;
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  await act(async () => { root.render(createElement(Probe, { running: true })); await settle(); });
  return state();
}

/** Runs the hook's next timed read. Input: the app events to answer with, null to fail the
 * read. Output: the reads made and the counts shown. */
export async function look(events) {
  reply = events;
  const work = tick;
  tick = null;
  await act(async () => { work(); await settle(); });
  return state();
}

/** Tells the hook the engine stopped. Output: the reads made and the counts shown. */
export async function engineStops() {
  await act(async () => { root.render(createElement(Probe, { running: false })); await settle(); });
  return state();
}
