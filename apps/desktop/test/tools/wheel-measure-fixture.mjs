import { Terminal } from "@xterm/xterm";
import { connectInput, terminalOptions, typeKeys } from "../../src/screens/team/terminal-panel.tsx";
import { encodeBinary, encodeTyped, jobQueue, keyQueue } from "../../src/screens/team/terminal-io.ts";

let terminal = null;
let pending = [];
let nextId = 0;
const waiters = new Map();
export const stats = { stopped: 0, waiting: 0, most: 0, sends: 0, reports: 0 };
export const fired = [];
export const acks = [];

/** Mounts one agent terminal whose sends wait in the page until the measuring script takes
 * them to tmux. `wiring` "new" is the panel's input routing with its wheel lane; "old" is the
 * routing before it, every chunk on one queue. */
export function mount(rows, cols, wiring) {
  document.body.innerHTML = '<div id="screen" style="width:900px;height:600px"></div>';
  terminal = new Terminal({ ...terminalOptions(), rows, cols });
  terminal.open(document.getElementById("screen"));
  const run = jobQueue(() => { stats.stopped += 1; });
  const counted = (job) => {
    const queued = run(async () => { try { return await job(); } finally { stats.waiting -= 1; } });
    if (queued) { stats.waiting += 1; stats.most = Math.max(stats.most, stats.waiting); }
    return queued;
  };
  const send = (chunk) => new Promise((done) => {
    nextId += 1;
    stats.sends += 1;
    pending.push({ id: nextId, chunk });
    waiters.set(nextId, { done, chunk });
  });
  if (wiring === "new") {
    connectInput(terminal, counted, send, () => true);
  } else {
    const type = keyQueue(counted, send);
    typeKeys(terminal, (data) => type(encodeTyped(data)));
    terminal.onBinary((data) => type(encodeBinary(data)));
  }
}

/** Writes bytes into the terminal and waits until xterm parsed them. */
export function write(text) {
  return new Promise((done) => terminal.write(text, done));
}

/** Hands the waiting sends to the measuring script. */
export function take() {
  const out = pending.map((item) => ({ id: item.id, chunk: item.chunk }));
  pending = [];
  return out;
}

/** Completes the sends tmux took, counting the wheel reports they carried. */
export function ack(ids) {
  for (const id of ids) {
    const waiter = waiters.get(id);
    waiters.delete(id);
    const text = String.fromCharCode(...waiter.chunk);
    stats.reports += (text.match(/\x1b\[<6[45];\d+;\d+M/g) ?? []).length;
    acks.push({ at: performance.now(), reports: stats.reports });
    waiter.done(true);
  }
}

/** Fires `count` wheel events on the terminal, `interval` ms apart, as a mouse or trackpad. */
export function fire(count, interval, deltaY = 120) {
  const screen = document.querySelector("#screen .xterm-screen");
  const box = screen.getBoundingClientRect();
  const at = { clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 };
  return new Promise((done) => {
    let left = count;
    const one = () => {
      fired.push(performance.now());
      screen.dispatchEvent(new WheelEvent("wheel", { ...at, deltaY, deltaMode: 0, bubbles: true, cancelable: true }));
      left -= 1;
      if (left > 0) setTimeout(one, interval); else done();
    };
    one();
  });
}

/** The terminal's buffer and mouse mode, the send counts and whether every send completed. */
export function view() {
  return { kind: terminal.buffer.active.type, mouse: terminal.modes.mouseTrackingMode, viewportY: terminal.buffer.active.viewportY, stats: { ...stats }, idle: pending.length === 0 && waiters.size === 0 };
}

/** Time from each wheel event to the tmux reply for the send carrying its report. */
export function latency() {
  const each = fired.map((at, index) => {
    const ack = acks.find((item) => item.reports >= index + 1);
    return ack ? ack.at - at : null;
  }).filter((value) => value !== null).sort((a, b) => a - b);
  const pick = (q) => each.length ? each[Math.min(each.length - 1, Math.floor(q * each.length))] : null;
  const last = fired.length && acks.length ? acks[acks.length - 1].at - fired[fired.length - 1] : null;
  return { delivered: each.length, mean: each.length ? each.reduce((a, b) => a + b, 0) / each.length : null, p50: pick(0.5), p95: pick(0.95), max: each.length ? each[each.length - 1] : null, lastWheelToLastAck: last };
}

/** The first line the view shows. */
export function topText() {
  const buffer = terminal.buffer.active;
  return buffer.getLine(buffer.viewportY)?.translateToString(true) ?? "";
}

/** The oldest line the view's buffer keeps, the furthest the wheel can reach. */
export function oldestText() {
  return terminal.buffer.active.getLine(0)?.translateToString(true) ?? "";
}

/** Waits two frames, so xterm has drawn. */
export function frames() {
  return new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
}
