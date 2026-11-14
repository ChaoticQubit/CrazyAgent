import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const signIn = await import("../src/screens/settings/sign-in.tsx");
const view = await import("../src/screens/settings/harnesses.tsx");

const CODE = "native_auth_harness_too_old";
const quiet = () => undefined;
const markup = (element) => renderToStaticMarkup(element).replace(/&#x27;/g, "'");
const claude = { id: "claude-code", method: { kind: "homebrew", cask: "claude-code@latest" }, installed: "2.1.200", latest: { kind: "not_checked" },
  command: "/opt/homebrew/bin/brew upgrade --cask claude-code@latest", install: null, refusal: null, guide: null, running: false, last: null, sign_in_min: "2.1.268" };
const codex = { ...claude, id: "codex", method: { kind: "homebrew", cask: "codex" }, installed: "0.50.0", command: "/opt/homebrew/bin/brew upgrade --cask codex", sign_in_min: "0.56.0" };
const ready = (id, name) => ({ id, name, status: { kind: "ready" }, status_label: "", executable: `/opt/homebrew/bin/${id}`, headless: `${id} -p`,
  tools: { kind: "switchable" }, model_selection: true, account_kinds: ["subscription"], team: { kind: "lead" }, aliases: [] });
const tooOld = (props) => markup(createElement(signIn.TooOldView, { name: "Claude Code", line: "line", versions: claude, mode: "check_and_update", pending: false, onUpdate: quiet, onCheck: quiet, ...props }));

test("DEC-166: a sign-in refused as too old names the harness, its version and the minimum it needs", () => {
  assert.deepEqual(signIn.signInFailure(CODE, "claude", undefined, claude),
    { text: "Claude Code 2.1.200 is too old to sign in from CrazyAgent; it needs 2.1.268 or newer.", hint: "too_old" });
  assert.deepEqual(signIn.signInFailure(CODE, "chatgpt", undefined, codex),
    { text: "Codex 0.50.0 is too old to sign in from CrazyAgent; it needs 0.56.0 or newer.", hint: "too_old" });
});

test("DEC-166: without a version below the minimum the line names only the minimum, and without a row it says to update", () => {
  const line = "Claude Code is too old to sign in from CrazyAgent; it needs 2.1.268 or newer.";
  assert.equal(signIn.signInFailure(CODE, "claude", undefined, { ...claude, installed: null }).text, line);
  assert.equal(signIn.signInFailure(CODE, "claude", undefined, { ...claude, installed: "2.1.300" }).text, line, "a version at or past the minimum is never called too old");
  assert.equal(signIn.signInFailure(CODE, "claude", undefined, { ...claude, installed: "garbage" }).text, line);
  assert.deepEqual(signIn.signInFailure(CODE, "claude"),
    { text: "Claude Code is too old to sign in from CrazyAgent. Update Claude Code, then choose Check again.", hint: "too_old" });
  assert.deepEqual(signIn.signInFailure(CODE, "chatgpt", undefined, { ...codex, sign_in_min: null }),
    { text: "Codex is too old to sign in from CrazyAgent. Update Codex, then choose Check again.", hint: "too_old" });
});

test("DEC-166: the too-old panel offers Update with the harness's own command and Check again", () => {
  const html = tooOld({ line: "Claude Code 2.1.200 is too old to sign in from CrazyAgent; it needs 2.1.268 or newer." });
  assert.match(html, /Claude Code 2\.1\.200 is too old to sign in from CrazyAgent; it needs 2\.1\.268 or newer\./);
  assert.match(html, /<button[^>]*>Update Claude Code<\/button>/);
  assert.match(html, /<button[^>]*>Check again<\/button>/);
  assert.doesNotMatch(html, /Try again/);
});

test("DEC-166: with updates on Check only, or no update command, the panel shows how to update instead of an Update button", () => {
  const checkOnly = tooOld({ mode: "check_only" });
  assert.doesNotMatch(checkOnly, /<button[^>]*>Update Claude Code<\/button>/);
  assert.match(checkOnly, /To update Claude Code, run this in Terminal:/);
  assert.match(checkOnly, /<pre[^>]*>\/opt\/homebrew\/bin\/brew upgrade --cask claude-code@latest<\/pre>/);
  assert.match(checkOnly, /<button[^>]*>Check again<\/button>/);
  const unknown = tooOld({ versions: { ...claude, method: { kind: "unknown" }, command: null, refusal: "harness_update_unknown_install" } });
  assert.doesNotMatch(unknown, /<button[^>]*>Update Claude Code<\/button>/);
  assert.match(unknown, /Update it with its own installer\./);
  assert.match(unknown, /<button[^>]*>Check again<\/button>/);
});

test("DEC-166: while its update runs the panel says so and offers no second Update", () => {
  const html = tooOld({ versions: { ...claude, running: true } });
  assert.match(html, /Updating Claude Code\.\.\./);
  assert.match(html, /<button[^>]*disabled=""[^>]*>Update Claude Code<\/button>/);
});

test("DEC-166: Settings, Harnesses says which version sign-in needs, for a harness that signs in only", () => {
  const signs = markup(createElement(view.HarnessRowView, { entry: ready("claude-code", "Claude Code"), versions: { ...claude, installed: "2.1.325" }, mode: "check_and_update",
    onCheck: quiet, onUpdate: quiet, onInstall: quiet, onGuide: quiet }));
  assert.match(signs, /Sign-in needs 2\.1\.268 or newer\./);
  const other = markup(createElement(view.HarnessRowView, { entry: ready("gemini-cli", "Gemini CLI"), versions: { ...claude, id: "gemini-cli", sign_in_min: undefined }, mode: "check_and_update",
    onCheck: quiet, onUpdate: quiet, onInstall: quiet, onGuide: quiet }));
  assert.doesNotMatch(other, /Sign-in needs/);
});
