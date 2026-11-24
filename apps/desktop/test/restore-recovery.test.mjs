import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const recovery = await import("../src/screens/restore-recovery.tsx");
const restore = await import("../src/screens/settings/restore.tsx");
const { plainError, knownError } = await import("../src/api.ts");
const { AppView } = await import("../src/app.tsx");

const FOLDER = "/Users/mira/Library/Application Support/CrazyAgent/backups";
const ok = (value) => ({ kind: "ok", value });
const quiet = { busy: false, problem: null, onPutBack() {}, onContinue() {}, onOpenFolder() {}, onRetry() {} };
const view = (offer, props = {}) => renderToStaticMarkup(createElement(recovery.RestoreRecoveryView,
  { ...quiet, read: ok({ offer, folder: FOLDER }), ...props }));
const button = (label) => new RegExp(`<button[^>]*>(?:(?!</button>).)*${label}</button>`);
const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

test("a restore whose named copy passes its check offers put-back and the backups folder", () => {
  const markup = view({ kind: "put_back", copy: "replaced-20261117T093000000Z" });
  assert.ok(markup.includes(recovery.RECOVERY_BROKEN), markup);
  assert.match(markup, button(recovery.PUT_BACK));
  assert.match(markup, button(recovery.OPEN_FOLDER));
  assert.ok(markup.includes(FOLDER), "the folder path is shown");
  assert.doesNotMatch(markup, button(recovery.CONTINUE));
  assert.doesNotMatch(markup, /Restart engine/);
});

test("a restore that stopped before it changed anything offers to continue with the 2.5 data", () => {
  const markup = view({ kind: "continue" });
  assert.ok(markup.includes(recovery.RECOVERY_CONTINUE), markup);
  assert.match(markup, button(recovery.CONTINUE));
  assert.doesNotMatch(markup, button(recovery.PUT_BACK));
});

test("a restore with no usable copy offers no button but the folder path and the code", () => {
  const markup = view({ kind: "stuck", code: "storage_putback_copy_missing" });
  assert.ok(markup.includes(recovery.RECOVERY_BROKEN), markup);
  assert.ok(markup.includes(FOLDER), markup);
  assert.ok(markup.includes("storage_putback_copy_missing"), markup);
  assert.doesNotMatch(markup, button(recovery.PUT_BACK));
  assert.doesNotMatch(markup, button(recovery.CONTINUE));
  assert.match(markup, button(recovery.OPEN_FOLDER));
});

test("a restore that wrote the 2.4 data is finished, not offered back, and a refused action shows its code", () => {
  const finished = view({ kind: "finished", copy: "replaced-20261117T093000000Z" });
  assert.ok(finished.includes(restore.RESTORE_RUNNING.replaceAll("'", "&#x27;")), finished);
  assert.doesNotMatch(finished, button(recovery.PUT_BACK));
  assert.doesNotMatch(finished, button(recovery.CONTINUE));
  const refused = view({ kind: "put_back", copy: "replaced-20261117T093000000Z" }, { problem: "storage_putback_busy" });
  assert.ok(refused.includes("storage_putback_busy"), refused);
  const busy = view({ kind: "put_back", copy: "replaced-20261117T093000000Z" }, { busy: true });
  assert.match(busy, /disabled=""/);
});

test("the engine-down state for a broken restore is the recovery screen, with no Restart engine", () => {
  const forbidden = () => { throw new Error("static rendering must not call the engine"); };
  const session = { engine: "down", engineProblem: plainError("engine_restore_broken"), engineCode: "engine_restore_broken",
    unconfirmed: false, profile: { kind: "loading" }, projects: { kind: "loading" }, toast: "", revision: 1, intake: [],
    found: { kind: "loading" }, retry: forbidden, noteResult: (result) => result };
  const markup = renderToStaticMarkup(createElement(AppView, { session, route: { name: "home" } }));
  assert.match(markup, /<h1>Restore 2\.4<\/h1>/);
  assert.doesNotMatch(markup, /Restart engine/);
  assert.doesNotMatch(markup, /engine isn&#x27;t running/);
});

test("Settings does not offer a new restore over one that did not finish", () => {
  for (const last of [{ kind: "restoring", copy: null }, { kind: "broken", copy: "replaced-20261117T093000000Z" }]) {
    const shown = restore.restoreShown({ until_ms: Date.UTC(2026, 11, 17), last });
    assert.equal(shown.offer, false, last.kind);
    assert.equal(shown.line, plainError("engine_restore_broken"), last.kind);
  }
});

test("a restore that ends not connected reaches the owner as its own sentence", () => {
  assert.equal(typeof knownError("not_connected"), "string");
  assert.doesNotMatch(plainError("not_connected"), /\(/);
});

test("the TUI says the recovery words and labels the desktop says", () => {
  const tui = read("../../../crates/crazyagent/src/tui/screens/restore_recovery.rs");
  for (const line of [recovery.RECOVERY_BROKEN, recovery.RECOVERY_CONTINUE, recovery.PUT_BACK, recovery.CONTINUE]) {
    assert.ok(tui.includes(`"${line}"`), `the TUI says ${line}`);
  }
});

test("a refused finish clears its running flag and offers Try again, as the TUI reads again on r", async () => {
  const finishing = { current: false };
  const refused = await recovery.finishOnce(finishing, async () => ({ kind: "error", code: "storage_putback_busy" }));
  assert.equal(refused, "storage_putback_busy");
  assert.equal(finishing.current, false, "a refused finish can run again");
  assert.equal(await recovery.finishOnce(finishing, async () => ({ kind: "ok" })), null);
  assert.equal(finishing.current, true, "a finish that ran is not run twice");
  const copy = "replaced-20261117T093000000Z";
  assert.doesNotMatch(view({ kind: "finished", copy }), button("Try again"), "a running finish offers nothing");
  const shown = view({ kind: "finished", copy }, { problem: "storage_putback_busy" });
  assert.match(shown, button("Try again"));
  assert.ok(shown.includes("storage_putback_busy"), shown);
});
