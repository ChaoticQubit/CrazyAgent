import { Terminal } from "@xterm/xterm";
import xtermStyles from "@xterm/xterm/css/xterm.css?inline";
import styles from "../../src/style.css?inline";
import teamStyles from "../../src/screens/team/team-graph.css?inline";
import { followScroll, terminalOptions } from "../../src/screens/team/terminal-panel.tsx";

let terminal = null;
export const sent = [];
const behind = [];

export function mount(rows) {
  terminal?.dispose();
  sent.length = 0;
  behind.length = 0;
  document.head.innerHTML = "";
  for (const text of [styles, teamStyles, xtermStyles]) {
    const style = document.createElement("style");
    style.textContent = text;
    document.head.append(style);
  }
  document.body.innerHTML = '<div id="root"><main class="main"><div class="page"><section class="tg-terminal"><div class="tg-terminal-screen" id="screen"></div></section></div></main></div>';
  terminal = new Terminal({ ...terminalOptions(), rows, cols: 80 });
  terminal.onData((data) => sent.push(data));
  terminal.open(document.getElementById("screen"));
}

export function write(text) {
  return new Promise((done) => terminal.write(text, done));
}

export function view() {
  const buffer = terminal.buffer.active;
  const box = document.querySelector("#screen .xterm-screen").getBoundingClientRect();
  return {
    kind: buffer.type, viewportY: buffer.viewportY, baseY: buffer.baseY, length: buffer.length,
    top: buffer.getLine(buffer.viewportY)?.translateToString(true) ?? "",
    x: Math.round(box.left + box.width / 2), y: Math.round(box.top + box.height / 2),
    sent: [...sent],
  };
}

export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}

/** Follows the terminal's scroll as the panel does for Jump to latest. */
export function follow() {
  followScroll(terminal, (value) => behind.push(value));
}

/** The latest Jump to latest flag, or null before the first report. */
export function reading() {
  return behind.length ? behind[behind.length - 1] : null;
}
