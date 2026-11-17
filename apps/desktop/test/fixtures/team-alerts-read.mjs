import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { useTeamAlerts } from "../../src/app.tsx";
import { COALESCE_MS } from "../../src/engine-changes.ts";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let reply = null;
let shown = null;
let root = null;
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
    if (command === "desktop_question_counts") return [];
    if (command === "desktop_team_attention") return null;
    if (command === "desktop_team_events" && reply !== null) return reply;
    throw { code: "unknown", message: command };
  },
  transformCallback: (heard) => {
    if (watch !== null) throw new Error("one channel, the page's watch");
    watch = heard;
    return 1;
  },
};

function Probe({ running }) {
  const { alerts, working } = useTeamAlerts(running, () => {});
  shown = { alerts: [...alerts], working: [...working] };
  return createElement("p", null, "probe");
}

const settle = () => new Promise((done) => realTimeout(done, 20));
const state = () => ({ asked: [...asked], shown });

/** Starts the real alert hook against a window bridge that answers its reads. Input: the app
 * events the engine answers with. Output: the reads made and the counts shown. */
export async function start(events) {
  reply = events;
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  await act(async () => { root.render(createElement(Probe, { running: true })); await settle(); });
  return state();
}

/** Sends one engine change on the page's watch channel, as the app's feed does. Inputs: the
 * topics, and the app events to answer with, null to fail the read. Output: the reads made
 * and the counts shown. */
export async function change(topics, events) {
  if (watch === null) throw new Error("the page never watched");
  reply = events;
  const index = sent;
  sent += 1;
  await act(async () => { watch({ index, message: { topics } }); release(); await settle(); });
  return state();
}

/** Lets the page sit with no change. Output: the reads made and the counts shown. */
export async function idle() {
  await act(async () => { await settle(); await settle(); });
  return state();
}

/** Runs the hook's read after a team change. Input: the app events to answer with, null to
 * fail the read. Output: the reads made and the counts shown. */
export function look(events) {
  return change(["team"], events);
}

/** Tells the hook the engine stopped. Output: the reads made and the counts shown. */
export async function engineStops() {
  await act(async () => { root.render(createElement(Probe, { running: false })); await settle(); });
  return state();
}
