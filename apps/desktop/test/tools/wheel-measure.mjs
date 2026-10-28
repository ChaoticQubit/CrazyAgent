import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { bundle, withPage } from "../browser.mjs";

const PANE_LINES = 12000;
const HISTORY_LIMIT = 5000;
const SOCKET_PATH_MAX = 100;
const SHAPES = [["wheel 40 notches @50ms", 40, 50], ["trackpad 300 events @8ms", 300, 8], ["trackpad 600 events @2ms", 600, 2]];
const CONFIGS = [["before", false, "old"], ["modes-only", true, "old"], ["after", true, "new"]];
const MODE_CODES = ["\x1b[?1049h", "\x1b[?1000h", "\x1b[?1002h", "\x1b[?1003h", "\x1b[?1005h", "\x1b[?1006h"];

/** Runs tmux commands against one private server socket. Input: the socket path. Output: a
 * call taking tmux arguments and returning its output. */
function tmuxOn(socket) {
  assert.ok(socket.length < SOCKET_PATH_MAX, `the socket path ${socket} stays under ${SOCKET_PATH_MAX} characters`);
  return (...args) => execFileSync("tmux", ["-f", "/dev/null", "-S", socket, ...args], { encoding: "utf8", maxBuffer: 64 << 20, stdio: ["ignore", "pipe", "pipe"] });
}

/** Opens a tmux control-mode client, as the engine attaches, and runs commands through it
 * one at a time. Inputs: the socket and session. Output: ready, run (true on %end) and close. */
function control(socket, session) {
  const child = spawn("tmux", ["-f", "/dev/null", "-S", socket, "-C", "attach-session", "-t", session], { stdio: ["pipe", "pipe", "ignore"] });
  let buffer = "";
  let block = false;
  let first = null;
  const ready = new Promise((done) => { first = done; });
  const queue = [];
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (data) => {
    buffer += data;
    let end;
    while ((end = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 1);
      if (line.startsWith("%begin")) block = true;
      else if (block && (line.startsWith("%end") || line.startsWith("%error"))) {
        block = false;
        if (first) { first(); first = null; continue; }
        queue.shift()?.(line.startsWith("%end"));
      }
    }
  });
  return { ready, run: (line) => new Promise((done) => { queue.push(done); child.stdin.write(line + "\n"); }), close: () => child.kill() };
}

/** Measures wheel delivery to a full-screen app: `less --mouse` over a long file in a real
 * tmux pane, attached as the engine attaches and fed by the view's real input routing in
 * headless Chrome. "before" replays the screen without the pane's alternate screen and mouse
 * modes and uses the single input queue, as the engine and view did before the fix;
 * "modes-only" replays the modes with the old queue; "after" is the shipped path. Inputs: the
 * page, tmux, the socket and the long file. Output: one result per shape and setup. */
async function wheelDelivery(evaluate, tmux, socket, file) {
  const fixture = await bundle(new URL("./wheel-measure-fixture.mjs", import.meta.url).pathname);
  const topLine = () => Number(/^line (\d+)/.exec(tmux("capture-pane", "-p", "-t", "m").split("\n")[0])?.[1] ?? NaN);
  const results = [];
  for (const [shape, count, interval] of SHAPES) {
    for (const [setup, withModes, wiring] of CONFIGS) {
      try { tmux("kill-server"); } catch {}
      tmux("new-session", "-d", "-s", "m", "-x", "80", "-y", "24", `exec env -u LESS less --mouse --wheel-lines=3 ${file}`);
      for (let waited = 0; topLine() !== 1; waited += 50) {
        assert.ok(waited < 5000, "less shows line 1");
        await pause(50);
      }
      const flags = tmux("display", "-p", "-t", "m", "#{alternate_on} #{mouse_standard_flag} #{mouse_button_flag} #{mouse_all_flag} #{mouse_utf8_flag} #{mouse_sgr_flag}").trim().split(" ").map((flag) => flag === "1");
      const modes = withModes ? MODE_CODES.filter((_, index) => flags[index]).join("") : "";
      const screen = tmux("capture-pane", "-p", "-e", "-t", "m").replace(/\n$/, "").split("\n").join("\r\n");
      await evaluate(fixture);
      await evaluate(`Fixture.mount(24, 80, ${JSON.stringify(wiring)})`);
      await evaluate(`Fixture.write(${JSON.stringify(modes + "\x1b[0m\x1b[H\x1b[2J" + screen)})`);
      const client = control(socket, "m");
      await client.ready;
      const top = topLine();
      let running = true;
      const pump = (async () => {
        while (running) {
          const items = await evaluate("Fixture.take()");
          if (items.length === 0) { await pause(1); continue; }
          for (const item of items) {
            assert.ok(await client.run(`send-keys -t m -H ${item.chunk.map((byte) => byte.toString(16).padStart(2, "0")).join(" ")}`), "tmux took the send");
            await evaluate(`Fixture.ack([${item.id}])`);
          }
        }
      })();
      await evaluate(`Fixture.fire(${count}, ${interval})`);
      for (let waited = 0, quiet = 0; waited < 60000 && quiet < 300; waited += 20) {
        quiet = (await evaluate("Fixture.view()")).idle ? quiet + 20 : 0;
        await pause(20);
      }
      running = false;
      await pump;
      await pause(300);
      const view = await evaluate("Fixture.view()");
      const latency = await evaluate("Fixture.latency()");
      client.close();
      const notches = Math.round((topLine() - top) / 3);
      const result = { measure: "wheel", shape, setup, view: `${view.kind}/${view.mouse}`, events: count, notchesMoved: notches, lostPct: Math.round(100 * (count - notches) / count), sends: view.stats.sends, mostWaiting: view.stats.most, stopped: view.stats.stopped,
        latencyMs: Object.fromEntries(Object.entries(latency).map(([name, value]) => [name, value === null ? null : Math.round(value * 10) / 10])) };
      console.log(JSON.stringify(result));
      results.push(result);
    }
  }
  return results;
}

/** Measures how much of a classic pane's history the view holds: `PANE_LINES` lines printed
 * into a tmux pane with `HISTORY_LIMIT` of history, the history and screen written into the
 * view as the engine sends them on attach, the oldest line the view's buffer keeps, and 200
 * wheel notches up with the time from each notch to a drawn frame. Inputs: the page, its CDP
 * send, tmux and the long file. Output: the result. */
async function scrollback(evaluate, send, tmux, file) {
  const fixture = await bundle(new URL("./wheel-measure-fixture.mjs", import.meta.url).pathname);
  try { tmux("kill-server"); } catch {}
  tmux("new-session", "-d", "-s", "h", "-x", "80", "-y", "24", "sleep 600");
  tmux("set-option", "-g", "history-limit", String(HISTORY_LIMIT));
  tmux("new-window", "-d", "-t", "h:", "-n", "c", `cat ${file}; exec sleep 600`);
  for (let waited = 0; !tmux("capture-pane", "-p", "-t", "h:c").includes(`line ${PANE_LINES}`); waited += 50) {
    assert.ok(waited < 5000, "the pane printed every line");
    await pause(50);
  }
  const history = tmux("capture-pane", "-p", "-e", "-S", "-", "-E", "-1", "-t", "h:c").replace(/\n$/, "").split("\n");
  const screen = tmux("capture-pane", "-p", "-e", "-t", "h:c").replace(/\n$/, "").split("\n");
  await evaluate(fixture);
  await evaluate("Fixture.mount(24, 80, 'new')");
  await evaluate(`Fixture.write(${JSON.stringify(history.join("\r\n") + "\r\n\x1b[0m\x1b[H\x1b[2J" + screen.join("\r\n"))})`);
  const box = await evaluate("(() => { const b = document.querySelector('#screen .xterm-screen').getBoundingClientRect(); return { x: Math.round(b.left + b.width / 2), y: Math.round(b.top + b.height / 2) }; })()");
  const notch = async (deltaY) => {
    const begun = performance.now();
    await send("Input.dispatchMouseEvent", { type: "mouseWheel", x: box.x, y: box.y, deltaX: 0, deltaY });
    await evaluate("Fixture.frames()");
    return performance.now() - begun;
  };
  const times = [];
  for (let index = 0; index < 200; index += 1) times.push(await notch(-120));
  const after200 = await evaluate("Fixture.topText()");
  times.sort((a, b) => a - b);
  const pick = (q) => Math.round(times[Math.min(times.length - 1, Math.floor(q * times.length))] * 10) / 10;
  const result = { measure: "scrollback", paneLines: PANE_LINES, historyLimit: HISTORY_LIMIT, tmuxHistoryRows: history.length, oldestKept: history[0], after200NotchesTop: after200, oldestInView: await evaluate("Fixture.oldestText()"), notchToFrameMs: { p50: pick(0.5), p95: pick(0.95), max: pick(1) } };
  console.log(JSON.stringify(result));
  return result;
}

/** Reruns the agent terminal scroll measurements behind DEC-101 slice 1 and prints one JSON
 * line per result. Needs tmux 3.2 or later, less and the Chrome that the desktop tests use.
 * Input: argv[2], an optional folder for the tmux socket and the long file (default: a new
 * temporary folder, removed afterwards). Output: the results on stdout. */
async function measure() {
  const given = process.argv[2];
  const folder = given ? resolve(given) : mkdtempSync(join(tmpdir(), "wm-"));
  mkdirSync(folder, { recursive: true });
  const file = join(folder, "lines.txt");
  writeFileSync(file, Array.from({ length: PANE_LINES }, (_, index) => `line ${index + 1} of the measured output`).join("\n") + "\n");
  const socket = join(folder, "s");
  const tmux = tmuxOn(socket);
  try {
    await withPage(async ({ evaluate, send }) => {
      await send("Emulation.setDeviceMetricsOverride", { width: 1200, height: 800, deviceScaleFactor: 1, mobile: false });
      await wheelDelivery(evaluate, tmux, socket, file);
      await scrollback(evaluate, send, tmux, file);
    });
  } finally {
    try { tmux("kill-server"); } catch {}
    if (given) rmSync(file, { force: true });
    else rmSync(folder, { recursive: true, force: true });
  }
}

await measure();
