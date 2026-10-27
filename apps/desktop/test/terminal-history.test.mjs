import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const io = await import("../src/screens/team/terminal-io.ts");
const { Terminal } = await import("../node_modules/@xterm/xterm/lib/xterm.mjs");

const PANE_ROWS = 10;
const VIEW_ROWS = 30;
const CHUNK_BYTES = 8192;
const HISTORY_ROWS = 2000;

const row = (index) => `history row ${String(index).padStart(5, "0")} ${"x".repeat(24)}`;
const historyRows = Array.from({ length: HISTORY_ROWS }, (_, index) => row(index + 1));
const screenRows = Array.from({ length: PANE_ROWS }, (_, index) => `screen row ${index + 1}`);
const attach = `\x1b[0m\x1b[H\x1b[2J${historyRows.map((text) => `${text}\r\n`).join("")}\x1b[0m${"\n".repeat(PANE_ROWS - 1)}`
  + `\x1b[0m\x1b[H\x1b[2J${screenRows.join("\r\n")}\x1b[0m\x1b[${PANE_ROWS};1H`;
const chunks = Array.from({ length: Math.ceil(attach.length / CHUNK_BYTES) }, (_, index) => attach.slice(index * CHUNK_BYTES, (index + 1) * CHUNK_BYTES));
const nextTask = (run) => { const id = setTimeout(run, 0); return () => clearTimeout(id); };

function allRows(terminal) {
  const buffer = terminal.buffer.active;
  return Array.from({ length: buffer.length }, (_, index) => buffer.getLine(index).translateToString(true)).filter(Boolean);
}

async function attachThenFit(onWrite) {
  const terminal = new Terminal({ cols: 80, rows: PANE_ROWS, scrollback: 5000 });
  let resolveFit;
  const fitted = new Promise((resolve) => { resolveFit = resolve; });
  const fit = () => { terminal.resize(80, VIEW_ROWS); resolveFit(); };
  const done = onWrite(fit);
  for (const chunk of chunks) terminal.write(chunk, done());
  await fitted;
  await new Promise((resolve) => terminal.write("", resolve));
  const rows = allRows(terminal);
  terminal.dispose();
  return rows;
}

test("a team terminal fits to its panel only once the attach's history and screen are drawn, so no history row is lost", async () => {
  assert.ok(chunks.length > 4, "the attach spans several engine chunks");
  const settled = await attachThenFit((fit) => io.settleAfterWrites(nextTask, fit));
  assert.deepEqual(settled, [...historyRows, ...screenRows]);

  let first = true;
  const early = await attachThenFit((fit) => () => () => { if (first) { first = false; fit(); } });
  const lost = historyRows.filter((text) => !early.includes(text));
  assert.ok(lost.length > 0, "fitting after the first chunk, as the panel did, drops history rows");
});

test("the settle waits for every queued write, runs once, and a write after it changes nothing", async () => {
  const frames = [];
  let settles = 0;
  const wrote = io.settleAfterWrites((run) => { frames.push(run); return () => { const at = frames.indexOf(run); if (at >= 0) frames.splice(at, 1); }; }, () => { settles += 1; });
  const first = wrote();
  const second = wrote();
  first();
  assert.equal(frames.length, 0, "no frame while a write is still parsing");
  second();
  assert.equal(frames.length, 1);
  const third = wrote();
  assert.equal(frames.length, 0, "a new write cancels the waiting frame");
  third();
  frames.shift()();
  assert.equal(settles, 1);
  const late = wrote();
  late();
  for (const run of frames.splice(0)) run();
  assert.equal(settles, 1, "the settle runs once");
  assert.throws(() => late(), /every write is done once/);
});
