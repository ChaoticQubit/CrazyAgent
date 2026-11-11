import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { LiveMasterView } from "../../src/screens/project/live-master.tsx";
import { ConversationTab } from "../../src/screens/project/conversation.tsx";

let root = null;
let props = null;
const engine = { count: 1, turns: [], reads: 0, failing: 0 };

/** The live session as the engine lists it, with the message count it holds now. */
const listed = () => ({
  session_id: "live", phase: "configured", created: "1", last_activity: "2", started_ms: null, active_ms: null, turns: engine.count,
  first_line: null, last_line: null, agents: [], results_waiting: 0,
});

/** Answers the bridge calls the master area makes: the project open, the master and its
 * live session, its turns and the Sessions list, which counts the reads it served and
 * refuses as many of them as `failing` says, as an engine that is not reachable does. A
 * New session is refused as one the engine's settings no longer admit. Every other call is
 * refused, as an engine without it would. */
function bridge() {
  window.__TAURI_INTERNALS__ = {
    invoke: async (command) => {
      if (command === "desktop_open_project") return { project_id: "alpha", ticket: "t-1" };
      if (command === "desktop_master_state") return { ticket: "t-1", session: { session_id: "live", phase: "configured" } };
      if (command === "desktop_master_turns") return { page: { turns: engine.turns, next: null } };
      if (command === "desktop_master_sessions") {
        engine.reads += 1;
        if (engine.failing > 0) {
          engine.failing -= 1;
          throw { code: "not_connected", message: "engine not reachable" };
        }
        return { sessions: [listed()] };
      }
      if (command === "desktop_new_master_session") return { pending: false, outcome: "rejected", rejection: "master_source_conflict" };
      throw { code: "unknown", message: command };
    },
    transformCallback: () => 0,
  };
}

function page() {
  root?.unmount();
  engine.count = 1;
  engine.turns = [];
  engine.reads = 0;
  engine.failing = 0;
  bridge();
  document.body.innerHTML = '<div id="root"></div>';
  root = createRoot(document.getElementById("root"));
}

const data = {
  id: "alpha", name: "Alpha", setup: undefined, research: undefined, context: undefined, pools: undefined, harnesses: undefined,
  assigned: null, now: 0, navigate: () => {}, notify: () => {}, reload: () => {}, reloadPools: () => {},
  team: { result: undefined, at: 0 }, reloadTeam: () => {},
};

const sessions = { ticket: "t-1", liveId: "live", reread: async () => {}, opened: async () => ({ kind: "ok", value: "t-1" }) };

/** The live master as the area's graph read shows it, in the given team status. */
const running = (status) => ({ kind: "running", node: { id: "master", kind: "master", parent: null, label: "Master", harness: "codex", status, status_label: null, account: null, terminal: null } });

/** Mounts the live master area on the live session with the master in the given status, as
 * the area's graph read last showed it. Inputs: the status and how many of the first list
 * reads the engine refuses. */
export function mountLive(status, failing = 0) {
  page();
  engine.failing = failing;
  props = { data, harness: "codex", live: running(status), onStart: async () => {}, sessions };
  flushSync(() => root.render(createElement(LiveMasterView, props)));
}

/** Has the engine count one more message on the live session and the area's next graph read
 * show the master in the given status, as a turn starting or ending does. Input: the status. */
export function turnLive(status) {
  engine.count += 1;
  props = { ...props, live: running(status) };
  flushSync(() => root.render(createElement(LiveMasterView, props)));
}

/** Mounts the conversation of a master that runs no live terminal, its live session holding
 * one turn that is still replying. */
export function mountConversation() {
  page();
  engine.turns = [{ id: "turn-1", ordinal: 1, phase: "running", reason: null, reply: null }];
  flushSync(() => root.render(createElement(ConversationTab, { data })));
}

/** Has the engine settle the first turn and run a second one sent from another window, as
 * the conversation's next read finds them. */
export function turnConversation() {
  engine.count += 1;
  engine.turns = [
    { id: "turn-1", ordinal: 1, phase: "succeeded", reason: null, reply: { text: "Planned." } },
    { id: "turn-2", ordinal: 2, phase: "running", reason: null, reply: null },
  ];
}

/** Waits up to a bound for the live session line to read the given text. Inputs: the text
 * and the bound in ms. Output: the line as last shown and how many list reads the engine
 * served. */
export async function liveLine(text, ms) {
  const line = () => document.querySelector('[data-testid="master-live-session"]')?.textContent ?? null;
  const end = Date.now() + ms;
  while (Date.now() < end && !line()?.endsWith(text)) await new Promise((done) => setTimeout(done, 25));
  return { line: line(), reads: engine.reads };
}

/** The problem the Sessions bar shows, if any. Output: its text, or null when none shows. */
export function problem() {
  return document.querySelector('[data-testid="master-sessions"] .pj-error')?.textContent ?? null;
}

/** Clicks the button with this exact text and lets React render. Input: the text. Output:
 * false when no such button shows. */
export function click(text) {
  const button = [...document.querySelectorAll("button")].find((item) => item.textContent.trim() === text);
  if (!button) return false;
  flushSync(() => button.click());
  return true;
}
