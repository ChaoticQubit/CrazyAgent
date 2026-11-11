import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-harness-updates.ts");
const view = await import("../src/screens/settings/harnesses.tsx");

const entry = (id, name, status) => ({ id, name, status, status_label: "", executable: status.kind === "not_installed" ? null : `/opt/homebrew/bin/${id}`,
  headless: `${id} -p`, tools: { kind: "switchable" }, model_selection: true, account_kinds: ["subscription"], team: { kind: "lead" }, aliases: [] });
const ready = (id, name) => entry(id, name, { kind: "ready" });
const limited = (id, name) => entry(id, name, { kind: "limited", reason: "It has no switch for tools" });
const missing = (id, name) => entry(id, name, { kind: "not_installed" });
const quiet = () => undefined;
const markup = (element) => renderToStaticMarkup(element).replace(/&#x27;/g, "'");
const row = (props) => markup(createElement(view.HarnessRowView, { onCheck: quiet, onUpdate: quiet, onGuide: quiet, ...props }));
const confirm = ({ versions, name }) => markup(createElement(view.UpdateConfirm, { versions, name, open: true, pending: false, onConfirm: quiet, onCancel: quiet }));
const page = (settings) => markup(createElement(view.UpdateSettingsView, { settings, save: () => Promise.resolve({ kind: "ok", value: undefined }) }));

const claude = { id: "claude-code", method: { kind: "homebrew", cask: "claude-code@latest" }, installed: "2.1.309", latest: { kind: "newer", version: "2.1.322" }, command: "/opt/homebrew/bin/brew upgrade --cask claude-code@latest", install: null, refusal: null, guide: null, running: false, last: null };

test("the version line says what is installed and what is new", () => {
  assert.equal(api.versionLine(claude), "Version 2.1.309. Version 2.1.322 is available.");
  assert.equal(api.versionLine({ ...claude, latest: { kind: "up_to_date" } }), "Version 2.1.309. Up to date.");
  assert.equal(api.versionLine({ ...claude, latest: { kind: "no_check" } }), "Version 2.1.309. This install updates itself. Update asks it to look now.");
  assert.equal(api.versionLine({ ...claude, latest: { kind: "failed", code: "harness_update_lookup_failed" } }), "Version 2.1.309. Couldn't check for a newer version.");
  assert.equal(api.versionLine({ ...claude, latest: { kind: "failed", code: "harness_update_check_unreadable" } }), "Version 2.1.309. Couldn't check for a newer version.");
  assert.equal(api.versionLine({ ...claude, latest: { kind: "not_checked" } }), "Version 2.1.309.");
  assert.equal(api.versionLine({ ...claude, installed: null, latest: { kind: "not_checked" } }), "Version unknown.");
});

test("a self-updating install names the version its vendor's site has, and keeps Update when the check failed", () => {
  const native = { ...claude, method: { kind: "self_update" }, command: "/Users/me/.local/bin/claude update" };
  assert.match(row({ entry: ready("claude-code", "Claude Code"), versions: native, mode: "check_and_update" }), /Version 2\.1\.309\. Version 2\.1\.322 is available\./);
  const failed = row({ entry: ready("claude-code", "Claude Code"), versions: { ...native, latest: { kind: "failed", code: "harness_update_lookup_failed" } }, mode: "check_and_update" });
  assert.match(failed, /Couldn't check for a newer version\./);
  assert.match(failed, /<button[^>]*>Update<\/button>/);
});

test("with online lookups off a self-updating install says it updates itself", () => {
  const native = { ...claude, method: { kind: "self_update" }, command: "/Users/me/.local/bin/claude update", latest: { kind: "no_check" } };
  assert.match(row({ entry: ready("claude-code", "Claude Code"), versions: native, mode: "check_and_update" }), /This install updates itself\. Update asks it to look now\./);
});

test("an installed harness row has Check for updates and Update", () => {
  const html = row({ entry: ready("claude-code", "Claude Code"), versions: claude, mode: "check_and_update" });
  assert.match(html, /<button[^>]*>Check for updates<\/button>/);
  assert.match(html, /<button[^>]*>Update<\/button>/);
});

test("the Update confirm shows the exact command, how it was installed and what running agents do", () => {
  const html = confirm({ versions: claude, name: "Claude Code" });
  assert.match(html, /<pre[^>]*>\/opt\/homebrew\/bin\/brew upgrade --cask claude-code@latest<\/pre>/);
  assert.match(html, /Installed with Homebrew \(claude-code@latest\)\./);
  assert.match(html, /Agents that are running keep their version until they start again\./);
});

test("an unknown install has no Update button, says why and links the harness's guide", () => {
  const html = row({ entry: ready("claude-code", "Claude Code"), versions: { ...claude, method: { kind: "unknown" }, command: null, refusal: "harness_update_unknown_install", guide: "https://code.claude.com/docs/en/setup" }, mode: "check_and_update" });
  assert.doesNotMatch(html, />Update</);
  assert.match(html, /CrazyAgent can't tell how this was installed, so it won't update it\./);
  assert.match(html, /href="https:\/\/code\.claude\.com\/docs\/en\/setup"/);
});

test("RCLI says it comes with CrazyAgent and has no buttons", () => {
  const html = row({ entry: limited("rcli", "RCLI"), versions: { ...claude, id: "rcli", method: { kind: "with_app" }, command: null, refusal: "harness_update_with_app", latest: { kind: "not_checked" } }, mode: "check_and_update" });
  assert.match(html, /Comes with CrazyAgent and updates with it\./);
  assert.doesNotMatch(html, />Update</);
  assert.doesNotMatch(html, />Check for updates</);
});

test("a harness that is not installed has no update buttons", () => {
  assert.doesNotMatch(row({ entry: missing("codex", "Codex"), versions: null, mode: "check_and_update" }), />Check for updates</);
});

test("a running update disables both buttons and says Updating", () => {
  const html = row({ entry: ready("claude-code", "Claude Code"), versions: { ...claude, running: true }, mode: "check_and_update" });
  assert.match(html, /Updating/);
  assert.match(html, /<button[^>]*disabled=""[^>]*>Update<\/button>/);
  assert.match(html, /<button[^>]*disabled=""[^>]*>Check for updates<\/button>/);
});

test("a finished update says from and to, and a failed one shows its last lines", () => {
  const ok = row({ entry: ready("claude-code", "Claude Code"), versions: { ...claude, last: { ok: true, code: null, output: "", from: "2.1.309", to: "2.1.322", ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(ok, /Updated from 2\.1\.309 to 2\.1\.322\. New agents use it now; no restart needed\./);
  const bad = row({ entry: ready("claude-code", "Claude Code"), versions: { ...claude, last: { ok: false, code: "harness_update_failed", output: "Error: cask is pinned", from: "2.1.309", to: "2.1.309", ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(bad, /The update didn't finish\./);
  assert.match(bad, /Error: cask is pinned/);
  assert.match(bad, /<summary[^>]*>What the updater said<\/summary>/);
});

test("Check only hides Update, and Off hides both buttons", () => {
  assert.doesNotMatch(row({ entry: ready("claude-code", "Claude Code"), versions: claude, mode: "check_only" }), />Update</);
  const off = row({ entry: ready("claude-code", "Claude Code"), versions: claude, mode: "off" });
  assert.doesNotMatch(off, />Update</);
  assert.doesNotMatch(off, />Check for updates</);
});

test("the page lists all three update settings with every option", () => {
  const html = page({ mode: "check_and_update", when: "on_button", online: "on" });
  for (const word of ["Check and update", "Check only", "Off", "Only when I choose Check for updates", "Each time this page opens", "Look them up on the vendor's site", "Don't look them up"]) assert.match(html, new RegExp(word));
});

test("the online lookup setting says which two sites are read and that nothing about the owner is sent", () => {
  const html = page({ mode: "check_and_update", when: "on_button", online: "on" });
  assert.match(html, /downloads\.claude\.ai/);
  assert.match(html, /releases\.openai\.com/);
  assert.match(html, /It sends no account, project or file details\./);
});

test("changing the online lookup keeps the other two settings", async () => {
  const sent = [];
  await view.chooseOnline({ mode: "check_only", when: "on_open", online: "on" }, "off", (settings) => { sent.push(settings); return Promise.resolve({ kind: "ok", value: undefined }); });
  assert.deepEqual(sent, [{ mode: "check_only", when: "on_open", online: "off" }]);
});

test("every update refusal has its own plain sentence", () => {
  const codes = ["harness_update_unknown_install", "harness_update_with_app", "harness_update_tool_missing", "harness_update_tool_unsafe", "harness_update_needs_admin", "harness_update_as_root", "harness_update_off", "harness_update_check_only", "harness_update_busy", "harness_update_not_installed", "harness_update_timeout", "harness_update_failed", "harness_update_unsupported_here", "harness_update_lookup_failed", "harness_update_check_unreadable"];
  assert.equal(new Set(codes.map(api.updateCopy)).size, codes.length);
  for (const code of codes) assert.doesNotMatch(api.updateCopy(code), /harness_|\u2014/, code);
});

test("polling stops when the update ends and is bounded", async () => {
  let reads = 0;
  const done = await api.pollWhileRunning("claude-code", (id, look) => {
    assert.equal(look, false);
    reads += 1;
    return Promise.resolve({ kind: "ok", value: { ...claude, id, running: reads < 3 } });
  }, () => Promise.resolve());
  assert.equal(reads, 3);
  assert.equal(done.kind, "ok");
  assert.equal(done.value.running, false);
  assert(api.UPDATE_POLLS_MAX * api.UPDATE_POLL_MS <= 20 * 60 * 1000);
});
