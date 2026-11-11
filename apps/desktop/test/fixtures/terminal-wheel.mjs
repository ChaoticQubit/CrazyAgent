import { Terminal } from "@xterm/xterm";
import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import styles from "../../src/style.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { connectInput, terminalOptions } from "../../src/screens/team/terminal-panel.tsx";
import { jobQueue } from "../../src/screens/team/terminal-io.ts";

let terminal = null;
let input = null;
let held = [];
let hold = false;
export const sent = [];
export const wheeled = [];
export const stats = { queued: 0, finished: 0, most: 0, stopped: 0, wheels: 0 };

/** Mounts one agent terminal view wired as the team panel wires it, with a send that the
 * page holds while `hold` is on, as an engine still busy with earlier input does. */
export function mount(rows, cols, keys = true) {
  input?.dispose();
  terminal?.dispose();
  sent.length = 0;
  wheeled.length = 0;
  held = [];
  hold = false;
  Object.assign(stats, { queued: 0, finished: 0, most: 0, stopped: 0, wheels: 0 });
  document.head.innerHTML = "";
  for (const text of [styles, teamStyles, xtermStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="page"><section class="tg-terminal"><div class="tg-terminal-screen" id="screen"></div></section></div></main></div>';
  terminal = new Terminal({ ...terminalOptions(), rows, cols });
  terminal.open(document.getElementById("screen"));
  document.getElementById("screen").addEventListener("wheel", () => { stats.wheels += 1; }, { capture: true });
  const run = jobQueue(() => { stats.stopped += 1; });
  const counted = (job) => {
    const queued = run(async () => {
      try { return await job(); } finally { stats.finished += 1; }
    });
    if (!queued) return false;
    stats.queued += 1;
    stats.most = Math.max(stats.most, stats.queued - stats.finished);
    return true;
  };
  input = connectInput(terminal, counted, (chunk, wheel) => {
    sent.push(chunk);
    wheeled.push(wheel);
    if (!hold) return Promise.resolve(true);
    return new Promise((done) => held.push(done));
  }, () => true, () => keys);
}

export function write(text) {
  return new Promise((done) => terminal.write(text, done));
}

export function holdSends(on) {
  hold = on;
  if (on) return;
  for (const done of held.splice(0)) done(true);
}

export function focus() {
  terminal.focus();
}

export function settled() {
  return held.length === 0 && (stats.stopped > 0 || stats.queued === stats.finished);
}

export function view() {
  const box = document.querySelector("#screen .xterm-screen").getBoundingClientRect();
  return {
    kind: terminal.buffer.active.type,
    mouse: terminal.modes.mouseTrackingMode,
    x: Math.round(box.left + box.width / 2),
    y: Math.round(box.top + box.height / 2),
    sends: sent.length,
    text: sent.map((chunk) => String.fromCharCode(...chunk)).join(""),
    lanes: sent.map((chunk, index) => `${wheeled[index] ? "wheel" : "keys"}:${String.fromCharCode(...chunk)}`),
    stats: { ...stats },
  };
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}
