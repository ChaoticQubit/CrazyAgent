import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const restore = await import("../src/screens/settings/restore.tsx");
const { plainError, knownError } = await import("../src/api.ts");
const { SETTINGS_GROUPS } = await import("../src/screens/settings/index.tsx");

const UNTIL = Date.UTC(2026, 11, 17, 9, 30);
const ok = (value) => ({ kind: "ok", value });
const quiet = { asking: false, sending: false, problem: "", onAsk() {}, onCancel() {}, onConfirm() {}, onRetry() {} };
const view = (props) => renderToStaticMarkup(createElement(restore.RestoreSettingsView, { ...quiet, ...props }));
const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

test("the button is offered only while the pinned 2.4 backup lasts and no restore runs or is done", () => {
  const offered = restore.restoreShown({ until_ms: UNTIL, last: null });
  assert.equal(offered.offer, true);
  assert.match(offered.line, /^You can bring back your 2\.4 data until \S/);
  assert.deepEqual(restore.restoreShown({ until_ms: null, last: null }), { offer: false, line: restore.RESTORE_NONE, failure: null });
  assert.deepEqual(restore.restoreShown({ until_ms: UNTIL, last: { kind: "pending" } }), { offer: false, line: restore.RESTORE_RUNNING, failure: null });
  const restored = { kind: "restored", copy: "replaced-20261117T093000000Z" };
  assert.deepEqual(restore.restoreShown({ until_ms: UNTIL, last: restored }), { offer: false, line: plainError("engine_restored"), failure: null });
  const failed = restore.restoreShown({ until_ms: UNTIL, last: { kind: "failed", code: "storage_restore_busy" } });
  assert.equal(failed.offer, true);
  assert.equal(failed.failure, plainError("storage_restore_busy"));
  const late = restore.restoreShown({ until_ms: null, last: { kind: "failed", code: "storage_restore_failed" } });
  assert.deepEqual(late, { offer: false, line: restore.RESTORE_NONE, failure: plainError("storage_restore_failed") });
});

test("the confirm names the three things a restore does before anything runs, and only while it is offered", () => {
  const closed = view({ status: ok({ until_ms: UNTIL, last: null }) });
  assert.match(closed, /Bring back 2\.4 data<\/button>/);
  assert.ok(!closed.includes("<dialog"), "nothing asks before the button");
  const asked = view({ status: ok({ until_ms: UNTIL, last: null }), asking: true });
  assert.equal(restore.RESTORE_CONFIRM.length, 3);
  for (const line of restore.RESTORE_CONFIRM) assert.ok(asked.includes(`<li>${line}</li>`), line);
  const gone = view({ status: ok({ until_ms: null, last: null }), asking: true });
  assert.ok(!gone.includes("<dialog"), "no confirm once the backup is gone");
  assert.ok(!gone.includes("Bring back 2.4 data</button>"));
});

test("every code a restore ends with reaches the owner as its own sentence", () => {
  for (const code of ["engine_restoring", "engine_restored", "storage_restore_unavailable", "storage_restore_agents_running", "storage_restore_busy", "storage_restore_failed"]) {
    assert.equal(typeof knownError(code), "string", code);
    assert.doesNotMatch(plainError(code), /\(/, code);
  }
  const bridge = read("../src-tauri/src/bridge/owner_setup.rs");
  const listed = bridge.slice(bridge.indexOf("const RESTORE_CODES"), bridge.indexOf("];", bridge.indexOf("const RESTORE_CODES")));
  for (const [, code] of listed.matchAll(/"([a-z_]+)"/g)) assert.equal(typeof knownError(code), "string", code);
});

test("Restore 2.4 is listed under App, and the TUI says the same confirm and lines", () => {
  const app = SETTINGS_GROUPS.find((group) => group.title === "App");
  assert.ok(app.items.some((item) => item.id === "restore" && item.label === "Restore 2.4"));
  const tui = read("../../../crates/crazyagent/src/tui/screens/settings/restore.rs");
  for (const line of [...restore.RESTORE_CONFIRM, restore.RESTORE_NONE, restore.RESTORE_RUNNING]) {
    assert.ok(tui.includes(`"${line}"`), `the TUI says ${line}`);
  }
});
