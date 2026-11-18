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

test("a team pane whose lead's teammates' server ended says so when its attach is refused", () => {
  const copy = "The terminal server this lead's teammates ran on ended, so its team panes are closed.";
  assert.equal(io.endCopy("terminal_teammates_server_ended"), copy);
  assert.deepEqual(io.terminalEvent({ kind: "rejected", code: "terminal_teammates_server_ended" }),
    { kind: "ended", copy });
});

test("a native view's end codes each say why in plain words, never the generic line, and name the cause only, since a following view reconnects by itself (V243-NATIVE-INPUT E5, #509 F2)", () => {
  const copy = {
    native_terminal_input_overflow: "More typing was waiting than this terminal could take, so the view closed.",
    native_terminal_gpu: "This Mac couldn't draw the terminal, so the view closed.",
    terminal_closed: "The connection to this terminal closed.",
    owner_grant_stale_or_revoked: "This app's access to the engine changed, so the view closed.",
  };
  assert.equal(io.endCopy("not_a_code"), "This terminal closed.");
  for (const [code, line] of Object.entries(copy)) assert.equal(io.endCopy(code), line, code);
});
