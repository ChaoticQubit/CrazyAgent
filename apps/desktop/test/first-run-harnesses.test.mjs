import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const model = await import("../src/screens/first-run/harnesses.ts");
const view = await import("../src/screens/first-run/harnesses-step.tsx");
const steps = await import("../src/screens/first-run/index.tsx");
const api = await import("../src/api-harness-updates.ts");

const quiet = () => undefined;
const markup = (element) => renderToStaticMarkup(element).replace(/&#x27;/g, "'");
const entry = (id, executable = null) => ({ id, name: id, status: executable ? { kind: "ready" } : { kind: "not_installed" }, status_label: "",
  executable, headless: "", tools: { kind: "switchable" }, model_selection: true, account_kinds: [], team: { kind: "lead" }, aliases: [] });
const versions = (id, patch = {}) => ({ id, method: { kind: "unknown" }, installed: null, latest: { kind: "not_checked" }, command: null, install: null,
  refusal: "harness_update_not_installed", guide: `https://example.test/${id}`, running: false, last: null, ...patch });
const last = (ok, code, to = null) => ({ ok, code, output: "", from: null, to, ended_ms: 1 });
const LINE = "/opt/homebrew/bin/npm install -g @openai/codex";

test("first run asks for harnesses right after tools and before accounts, in the step's own words", () => {
  assert.deepEqual(steps.FIRST_RUN_STEPS.map((step) => step.id), ["profile", "home", "tools", "harnesses", "accounts"]);
  const step = steps.FIRST_RUN_STEPS.find((item) => item.id === "harnesses");
  assert.deepEqual({ label: step.label, title: step.title, intro: step.intro }, model.HARNESS_STEP);
});

test("the step offers RCLI, Codex and Claude Code, and only RCLI starts picked because it comes with CrazyAgent", () => {
  assert.deepEqual(model.OFFERED.map((harness) => [harness.id, harness.name]), [["rcli", "RCLI"], ["codex", "Codex"], ["claude-code", "Claude Code"]]);
  assert.deepEqual(model.harnessRow("rcli", undefined, null, []), { kind: "bundled" });
  assert.equal(model.tickable({ kind: "bundled" }), false);
  assert.deepEqual(model.installable(["rcli", "codex"]), ["codex"]);
});

test("each row says what installing it runs, or why it can't, from the engine's reads", () => {
  assert.deepEqual(model.harnessRow("codex", entry("codex"), versions("codex", { install: LINE }), []), { kind: "offer", line: LINE });
  assert.deepEqual(model.harnessRow("codex", entry("codex"), versions("codex", { refusal: "harness_install_needs_node" }), []),
    { kind: "refused", reason: api.updateCopy("harness_install_needs_node"), guide: "https://example.test/codex" });
  assert.deepEqual(model.harnessRow("codex", entry("codex"), versions("codex", { refusal: "harness_install_off" }), []),
    { kind: "refused", reason: api.updateCopy("harness_install_off"), guide: "https://example.test/codex" });
  assert.deepEqual(model.harnessRow("claude-code", entry("claude-code", "/usr/local/bin/claude"), versions("claude-code", { installed: "2.1.0", refusal: null }), []),
    { kind: "installed", version: "2.1.0" });
  assert.deepEqual(model.harnessRow("codex", entry("codex"), versions("codex", { install: LINE }), ["codex"]), { kind: "waiting" });
  assert.deepEqual(model.harnessRow("codex", entry("codex"), versions("codex", { running: true }), []), { kind: "installing" });
  assert.deepEqual(model.harnessRow("codex", entry("codex", "/x/codex"), versions("codex", { last: last(true, null, "0.52.0") }), []), { kind: "done", version: "0.52.0" });
  assert.deepEqual(model.harnessRow("codex", entry("codex"), versions("codex", { last: last(false, "harness_install_failed") }), []),
    { kind: "failed", reason: api.updateCopy("harness_install_failed") });
  assert.deepEqual(model.harnessRow("codex", entry("codex"), null, []), { kind: "reading" });
});

test("needs Node has its own plain sentence, apart from no installer", () => {
  const needs = api.updateCopy("harness_install_needs_node");
  assert.notEqual(needs, api.updateCopy("harness_install_no_tool"));
  assert.notEqual(needs, api.updateCopy("harness_update_outcome_unknown"));
  assert.match(needs, /Node\.js/);
  assert.notEqual(api.updateCopy("harness_install_list_invalid"), api.updateCopy("harness_update_outcome_unknown"));
});

test("only a row it can install is tickable, and a failed or refused row offers Try again", () => {
  assert.equal(model.tickable({ kind: "offer", line: LINE }), true);
  for (const row of [{ kind: "installed", version: null }, { kind: "refused", reason: "x", guide: null }, { kind: "waiting" }, { kind: "installing" },
    { kind: "done", version: null }, { kind: "failed", reason: "x" }, { kind: "reading" }]) {
    assert.equal(model.tickable(row), false, row.kind);
  }
  assert.equal(model.retry({ kind: "failed", reason: "x" }), "install");
  assert.equal(model.retry({ kind: "refused", reason: "x", guide: null }), "read");
  assert.equal(model.retry({ kind: "offer", line: LINE }), null);
});

test("progress counts the picks that ended and the ones that failed", () => {
  const rows = { codex: { kind: "done", version: null }, "claude-code": { kind: "failed", reason: "x" } };
  assert.deepEqual(model.progress(["codex", "claude-code"], rows), { done: 2, total: 2, failed: 1, running: false });
  assert.deepEqual(model.progress(["codex", "claude-code"], { codex: { kind: "installing" }, "claude-code": { kind: "waiting" } }), { done: 0, total: 2, failed: 0, running: true });
  assert.equal(model.progressLine({ done: 1, total: 2, failed: 0, running: true }), "Installing: 1 of 2 done. You can continue; the installs keep going.");
  assert.equal(model.progressLine({ done: 2, total: 2, failed: 1, running: false }), "Installs finished: 2 of 2 done, 1 failed. Each failure shows its reason above.");
  assert.equal(model.progressLine({ done: 2, total: 2, failed: 0, running: false }), "Installs finished: 2 of 2 done.");
});

test("the view shows RCLI picked and locked, an unpicked offer with its line, and the needs-Node reason with Try again and the guide", () => {
  const rows = { rcli: { kind: "bundled" }, codex: { kind: "offer", line: LINE }, "claude-code": { kind: "refused", reason: api.updateCopy("harness_install_needs_node"), guide: "https://example.test/claude-code" } };
  const html = markup(createElement(view.HarnessesStepView, { rows, picks: [], started: false, busy: false, problem: "", onPick: quiet, onInstall: quiet, onRetry: quiet, onContinue: quiet }));
  assert.match(html, /aria-label="RCLI"[^>]*checked=""[^>]*disabled=""|aria-label="RCLI"[^>]*disabled=""[^>]*checked=""/);
  assert.match(html, /Comes with CrazyAgent\./);
  assert.match(html, /aria-label="Codex"/);
  assert.doesNotMatch(html, /aria-label="Codex"[^>]*checked=""/);
  assert.ok(html.includes(`Installs with ${LINE}`));
  assert.ok(html.includes(api.updateCopy("harness_install_needs_node")));
  assert.match(html, /How to install it yourself/);
  assert.match(html, />Try again</);
  assert.match(html, />Continue</);
  assert.doesNotMatch(html, /Install picked harnesses/);
  const picked = markup(createElement(view.HarnessesStepView, { rows, picks: ["codex"], started: false, busy: false, problem: "", onPick: quiet, onInstall: quiet, onRetry: quiet, onContinue: quiet }));
  assert.match(picked, /aria-label="Codex"[^>]*checked=""|checked=""[^>]*aria-label="Codex"/);
  assert.match(picked, /Install picked harnesses/);
  assert.match(picked, /Skip for now/);
});
