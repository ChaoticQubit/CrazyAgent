import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-harness-path.ts");
const view = await import("../src/screens/settings/harnesses.tsx");

const quiet = () => undefined;
const markup = (element) => renderToStaticMarkup(element).replace(/&#x27;/g, "'");
const block = (props) => markup(createElement(view.HarnessFileView, { id: "codex", name: "Codex", pending: false, onLook: quiet, onChoose: quiet, onForget: quiet, ...props }));
const entry = (id, name, kind) => ({ id, name, status: { kind }, status_label: "", executable: kind === "not_installed" ? null : `/opt/homebrew/bin/${id}`,
  headless: `${id} -p`, tools: { kind: "switchable" }, model_selection: true, account_kinds: ["subscription"], team: { kind: "lead" }, aliases: [] });
const CODES = ["harness_path_unknown_harness", "harness_path_invalid", "harness_path_not_found", "harness_path_not_executable", "harness_path_agent_space",
  "harness_path_shim", "harness_path_changeable", "harness_path_unreadable", "native_auth_host_executable_unsafe", "native_auth_host_executable_changed",
  "native_auth_host_unavailable", "native_auth_host_registration_frozen", "owner_runtime_stale", "harness_path_refused"];

test("each refusal of a harness file has its own sentence, and an unknown code reads as a plain refusal", () => {
  const lines = CODES.map((code) => api.pathCopy(code));
  assert.equal(new Set(lines).size, CODES.length);
  for (const code of CODES) assert.equal(api.isPathCode(code), true, code);
  assert.equal(api.isPathCode("engine_unreachable"), false);
  assert.equal(api.pathCopy("something_new"), api.pathCopy("harness_path_refused"));
  assert.equal(api.pathCopy("harness_path_not_executable"), "That file isn't a program this Mac can run.");
});

test("only Codex and Claude Code have a file to choose, named as the owner finds it", () => {
  assert.deepEqual([...api.CHOOSABLE], ["codex", "claude-code"]);
  for (const id of ["codex", "claude-code"]) assert.equal(api.choosable(id), true, id);
  for (const id of ["claude", "aider", "rcli", ""]) assert.equal(api.choosable(id), false, id);
  assert.equal(api.harnessFileName("codex"), "codex");
  assert.equal(api.harnessFileName("claude-code"), "claude");
});

test("a harness found nowhere lists where CrazyAgent looked, says shims are skipped, and offers Check again and Choose the file", () => {
  const html = block({ found: { kind: "missing", looked: ["/opt/homebrew/bin", "~/.local/bin", "~/.nvm/versions/node/v22.11.0/bin", "~/.volta/bin"] } });
  assert.match(html, /CrazyAgent looked for codex in these folders and didn't find it:/);
  for (const folder of ["/opt/homebrew/bin", "~/.local/bin", "~/.nvm/versions/node/v22.11.0/bin", "~/.volta/bin"]) assert.match(html, new RegExp(`<li>${folder.replace(/[.]/g, "\\.")}</li>`));
  assert.match(html, /Volta, asdf and mise folders hold shims that start another program/);
  assert.match(html, /aria-label="Check again for Codex"/);
  assert.match(html, />Choose the codex file<\/button>/);
  assert.doesNotMatch(html, /Forget the chosen file/);
});

test("the looked list never shows more than the engine's bound", () => {
  const looked = Array.from({ length: api.LOOKED_MAX + 20 }, (_, index) => `/folder-${index}`);
  const html = block({ found: { kind: "missing", looked } });
  assert.equal(html.match(/<li>/g).length, api.LOOKED_MAX);
});

test("a refused file names the file with home as ~ and the reason, and offers to forget a chosen file", () => {
  const html = block({ found: { kind: "refused", path: "/Users/someone/tools/codex", code: "harness_path_not_executable" } });
  assert.match(html, /CrazyAgent found ~\/tools\/codex but won't use it\. That file isn't a program this Mac can run\./);
  assert.match(html, />Forget the chosen file<\/button>/);
  assert.match(html, />Choose the codex file<\/button>/);
});

test("a harness in use says where, with no Check again; the sign-in shows Try again in its place", () => {
  const settings = block({ found: { kind: "registered", path: "/opt/homebrew/bin/codex" } });
  assert.match(settings, /Codex is ready at \/opt\/homebrew\/bin\/codex\./);
  assert.doesNotMatch(settings, /Check again|Choose the codex file|Try again/);
  const signIn = block({ id: "claude-code", name: "Claude Code", found: { kind: "ready", path: "/Users/someone/.local/bin/claude" }, onReady: quiet, readyLabel: "Try again" });
  assert.match(signIn, /Claude Code is ready at ~\/\.local\/bin\/claude\./);
  assert.match(signIn, />Try again<\/button>/);
});

test("a failure to look shows its sentence and keeps both actions", () => {
  const html = block({ found: null, problem: api.pathCopy("harness_path_unreadable") });
  assert.match(html, /CrazyAgent couldn't read or save the chosen file\. Try again\./);
  assert.match(html, /aria-label="Check again for Codex"/);
  assert.match(html, />Choose the codex file<\/button>/);
});

test("the Harnesses page offers the file choice under a missing Codex or Claude Code only", () => {
  const catalog = { kind: "ok", value: [entry("codex", "Codex", "not_installed"), entry("claude-code", "Claude Code", "ready"), entry("aider", "Aider", "not_installed")] };
  const html = markup(createElement(view.HarnessesView, { catalog, onCheck: quiet, settings: null }));
  assert.match(html, /data-harness-file="codex"/);
  assert.doesNotMatch(html, /data-harness-file="claude-code"/);
  assert.doesNotMatch(html, /data-harness-file="aider"/);
  assert.match(html, />Choose the codex file<\/button>/);
});
