import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { RESET_WATCH_MS, useResetNotice } from "../../src/screens/metrics/extras.ts";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const ACCOUNTS = [{ id: "acct-a", name: "Work" }, { id: "acct-b", name: "Home" }];
let reply = null;
let tick = null;
let root = null;
const asked = [];
const toasts = [];
const realTimeout = window.setTimeout.bind(window);

window.setTimeout = (work, ms, ...rest) => {
  if (ms !== RESET_WATCH_MS) return realTimeout(work, ms, ...rest);
  tick = work;
  return 1;
};

window.__TAURI_INTERNALS__ = {
  invoke: async (command, args) => {
    asked.push(command === "desktop_usage_extras" ? `${command}:${args.detail}` : command);
    if (command === "desktop_usage_extras" && reply !== null) return reply;
    if (command === "desktop_list_accounts") return { accounts: ACCOUNTS, next: null };
    throw { code: "unknown", message: command };
  },
  transformCallback: () => 0,
};

/** One stored extras row as the engine answers it. Inputs: the account, its count and how
 * long ago it rose, null for never. Output: the row. */
function row([account, count, agoMs]) {
  return {
    account,
    extras: {
      harness: "Codex",
      adapter: "codex",
      may_break: true,
      resets: { count, kinds: [], next_expiry_ms: null },
      credits: null,
      spend: { kind: "in_app" },
      not_reported: "not reported by Codex",
      resets_checked_ms: Date.now(),
      credits_checked_ms: null,
      resets_gained_ms: agoMs === null ? null : Date.now() - agoMs,
    },
  };
}

function Probe({ running }) {
  useResetNotice(running, (message, open) => toasts.push({ message, open: open ?? null }));
  return createElement("p", null, "probe");
}

const settle = () => new Promise((done) => realTimeout(done, 20));
const state = () => ({ asked: [...asked], toasts: [...toasts], armed: tick !== null });

/** Starts the real new-reset watch against a window bridge that answers its reads. Input:
 * the rows as [account, count, ms since the count rose or null]. Output: the reads made, the
 * toasts shown and whether the next look is armed. */
export async function start(rows) {
  reply = rows.map(row);
  document.body.innerHTML = "<div id='root'></div>";
  root = createRoot(document.getElementById("root"));
  await act(async () => { root.render(createElement(Probe, { running: true })); await settle(); });
  return state();
}

/** Runs the watch's next timed read. Input: the rows, null to fail the read. Output: the
 * reads made, the toasts shown and whether the next look is armed. */
export async function look(rows) {
  reply = rows === null ? null : rows.map(row);
  const work = tick;
  tick = null;
  await act(async () => { work(); await settle(); });
  return state();
}

/** Tells the watch the engine stopped, then runs any look it left armed. Output: the reads
 * made, the toasts shown and whether a look is armed. */
export async function engineStops() {
  await act(async () => { root.render(createElement(Probe, { running: false })); await settle(); });
  const work = tick;
  tick = null;
  if (work) await act(async () => { work(); await settle(); });
  return state();
}
