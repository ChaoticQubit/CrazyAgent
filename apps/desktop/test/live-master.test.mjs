import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import "./load-source.mjs";

const io = await import("../src/screens/team/terminal-io.ts");
const live = await import("../src/screens/project/live-master.tsx");

const key = (type, extra = {}) => ({ type, key: "Enter", shiftKey: false, altKey: false, ctrlKey: false, metaKey: false, ...extra });

test("Shift+Enter types a new line once and never reaches xterm as a second Enter", () => {
  assert.deepEqual(io.keyOverride(key("keydown", { shiftKey: true })), { typed: "\n", xterm: false });
  assert.deepEqual(io.keyOverride(key("keypress", { shiftKey: true })), { typed: null, xterm: false });
  assert.deepEqual(io.keyOverride(key("keyup", { shiftKey: true })), { typed: null, xterm: false });
  assert.deepEqual(io.keyOverride(key("keydown")), { typed: null, xterm: true }, "Enter alone still sends");
  for (const modifier of ["altKey", "ctrlKey", "metaKey"]) {
    assert.deepEqual(io.keyOverride(key("keydown", { shiftKey: true, [modifier]: true })), { typed: null, xterm: true }, modifier);
  }
  assert.deepEqual(io.keyOverride({ ...key("keydown", { shiftKey: true }), key: "a" }), { typed: null, xterm: true });
  assert.deepEqual([...io.encodeTyped(io.NEW_LINE)], [0x0a]);
});

test("only Codex and Claude Code masters run live, and the master is found by kind", () => {
  assert.equal(live.runsLive("codex"), true);
  assert.equal(live.runsLive("claude-code"), true);
  for (const other of ["rcli", "opencode", "", undefined]) assert.equal(live.runsLive(other), false, String(other));
  const node = (id, kind, terminal = null) => ({ id, kind, parent: null, label: id, harness: "codex", status: null, status_label: null, account: null, agents: null, terminal });
  assert.equal(live.masterNode([node("g", "harness_group"), node("a", "agent", "team-2")]), null);
  assert.equal(live.masterNode([node("a", "agent", "team-2"), node("m", "master", "team-1")]).id, "m");
});

test("the terminal hands Shift+Enter to the key override before xterm reads it", () => {
  const panel = readFileSync(new URL("../src/screens/team/terminal-panel.tsx", import.meta.url), "utf8");
  assert.match(panel, /attachCustomKeyEventHandler\(\(event\) => \{\s*const override = keyOverride\(event\);/);
  const page = readFileSync(new URL("../src/screens/project/index.tsx", import.meta.url), "utf8");
  assert.match(page, /active === "conversation" && <MasterArea data=\{data\} \/>/);
});
