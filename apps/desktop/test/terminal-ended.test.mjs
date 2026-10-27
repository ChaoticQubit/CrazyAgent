import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";

const io = await import("../src/screens/team/terminal-io.ts");
const { Terminal } = await import("../node_modules/@xterm/xterm/lib/xterm.mjs");

const SCREEN = "\x1b[0m\x1b[H\x1b[2Jline one\r\nline two\r\nline three\r\nfooter\x1b[0m\x1b[4;7H";
const LABEL = "\x1b[0m\x1b[1;1H\x1b[L\x1b[7m Finished at 10:42 \x1b[0m\x1b[?25l\x1b[1;1H";

test("a finished run's last screen shows its finished label on top and says the run finished", async () => {
  const terminal = new Terminal({ cols: 40, rows: 4, allowProposedApi: true });
  await new Promise((resolve) => terminal.write(SCREEN + LABEL, resolve));
  const rows = [0, 1, 2, 3].map((row) => terminal.buffer.active.getLine(row).translateToString(true));
  terminal.dispose();
  assert.deepEqual(rows, [" Finished at 10:42 ", "line one", "line two", "line three"]);
  assert.equal(io.terminalEvent({ kind: "closed", reason: "terminal_ended" }).copy,
    "This run finished. Its last screen stays above.");
  assert.equal(io.endCopy("terminal_agent_unknown"),
    "This agent's terminal isn't running. It may have finished or not started yet.");
});
