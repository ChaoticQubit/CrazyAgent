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
const row = (props) => markup(createElement(view.HarnessRowView, { onCheck: quiet, onUpdate: quiet, onInstall: quiet, onGuide: quiet, ...props }));
const confirm = ({ versions, name }) => markup(createElement(view.UpdateConfirm, { versions, name, open: true, pending: false, onConfirm: quiet, onCancel: quiet }));
const installConfirm = ({ versions, name }) => markup(createElement(view.InstallConfirm, { versions, name, open: true, pending: false, onConfirm: quiet, onCancel: quiet }));
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
  const codes = ["harness_update_unknown_install", "harness_update_with_app", "harness_update_tool_missing", "harness_update_tool_unsafe", "harness_update_needs_admin", "harness_update_as_root", "harness_update_off", "harness_update_check_only", "harness_update_busy", "harness_update_not_installed", "harness_update_timeout", "harness_update_failed", "harness_update_unsupported_here", "harness_update_lookup_failed", "harness_update_check_unreadable", "harness_update_check_failed", "harness_update_start_failed", "harness_update_settings_unreadable", "harness_update_unavailable"];
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

const aider = { id: "aider", method: { kind: "unknown" }, installed: null, latest: { kind: "not_checked" }, command: null, install: "/opt/homebrew/bin/brew install --formula aider", refusal: "harness_update_not_installed", guide: "https://aider.chat/docs/install.html", running: false, last: null };

test("a harness that is not installed has an Install button right in its row, and no update buttons", () => {
  const html = row({ entry: missing("aider", "Aider"), versions: aider, mode: "check_and_update" });
  assert.match(html, /<button[^>]*aria-label="Install Aider"[^>]*>Install<\/button>/);
  assert.doesNotMatch(html, />Check for updates</);
  assert.doesNotMatch(html, />Update</);
});

test("every catalog harness gets the button, also one CrazyAgent cannot start yet", () => {
  const html = row({ entry: missing("goose", "Goose"), versions: { ...aider, id: "goose", install: "/opt/homebrew/bin/brew install --formula block-goose-cli" }, mode: "check_and_update" });
  assert.match(html, /<button[^>]*>Install<\/button>/);
});

test("the Install confirm shows the exact command and what CrazyAgent will not do", () => {
  const html = installConfirm({ versions: aider, name: "Aider" });
  assert.match(html, /<pre[^>]*>\/opt\/homebrew\/bin\/brew install --formula aider<\/pre>/);
  assert.match(html, /CrazyAgent runs exactly this line\. It never uses sudo and never runs a script from a website\./);
  assert.match(html, /When it finishes, new agents can use Aider with no engine restart\./);
  const npm = installConfirm({ versions: { ...aider, id: "continue", install: "/opt/homebrew/bin/npm install -g @continuedev/cli" }, name: "Continue CLI" });
  assert.match(npm, /<pre[^>]*>\/opt\/homebrew\/bin\/npm install -g @continuedev\/cli<\/pre>/);
});

test("with neither Homebrew nor npm the row has no Install button, says why and links the vendor's guide", () => {
  const html = row({ entry: missing("aider", "Aider"), versions: { ...aider, install: null, refusal: "harness_install_no_tool" }, mode: "check_and_update" });
  assert.doesNotMatch(html, />Install</);
  assert.match(html, /CrazyAgent installs with Homebrew or npm and found no way to install this one\./);
  assert.match(html, /<a[^>]*href="https:\/\/aider\.chat\/docs\/install\.html"[^>]*>How to install it yourself<\/a>/);
});

test("with Install turned off in settings the row keeps only the guide link", () => {
  const html = row({ entry: missing("aider", "Aider"), versions: { ...aider, install: null, refusal: "harness_install_off" }, mode: "check_and_update" });
  assert.doesNotMatch(html, />Install</);
  assert.match(html, />How to install it yourself</);
});

test("a running install disables the button and says Installing", () => {
  const html = row({ entry: missing("aider", "Aider"), versions: { ...aider, running: true }, mode: "check_and_update" });
  assert.match(html, /Installing/);
  assert.match(html, /<button[^>]*disabled=""[^>]*>Install<\/button>/);
});

test("a finished install says the version and that no restart is needed, and a failed one shows its last lines", () => {
  const done = row({ entry: ready("aider", "Aider"), versions: { ...aider, installed: "0.86.2", install: null, refusal: null, method: { kind: "homebrew_formula", formula: "aider" }, command: "/opt/homebrew/bin/brew upgrade --formula aider", last: { ok: true, code: null, output: "", from: null, to: "0.86.2", ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(done, /Version 0\.86\.2 is installed now\. New agents use it; no restart needed\./);
  const failed = row({ entry: missing("aider", "Aider"), versions: { ...aider, last: { ok: false, code: "harness_install_failed", output: "Error: No available formula", from: null, to: null, ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(failed, /The install didn't finish\./);
  assert.match(failed, /Error: No available formula/);
  assert.match(failed, /<summary[^>]*>What the installer said<\/summary>/);
  const lost = row({ entry: missing("aider", "Aider"), versions: { ...aider, last: { ok: false, code: "harness_install_not_found", output: "", from: null, to: null, ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(lost, /The install finished, but CrazyAgent can't find Aider\. Choose Check again\./);
});

test("any harness installed with Homebrew or npm gets Check for updates and Update, named by how it was installed", () => {
  const formula = { ...aider, installed: "0.86.1", install: null, refusal: null, method: { kind: "homebrew_formula", formula: "aider" }, command: "/opt/homebrew/bin/brew upgrade --formula aider", latest: { kind: "newer", version: "0.86.2" } };
  const html = row({ entry: limited("aider", "Aider"), versions: formula, mode: "check_and_update" });
  assert.match(html, />Check for updates</);
  assert.match(html, /<button[^>]*>Update<\/button>/);
  assert.equal(api.installedWith(formula.method), "Installed with Homebrew (aider).");
  assert.equal(api.installedWith({ kind: "homebrew", cask: "droid" }), "Installed with Homebrew (droid).");
  assert.equal(api.installedWith({ kind: "npm", package: "@continuedev/cli" }), "Installed with npm (@continuedev/cli).");
});

test("another harness installed some other way says to use its own installer and links the guide", () => {
  const html = row({ entry: limited("aider", "Aider"), versions: { ...aider, installed: "0.86.2", install: null, refusal: "harness_update_unknown_install" }, mode: "check_and_update" });
  assert.doesNotMatch(html, />Update</);
  assert.match(html, /CrazyAgent can't tell how this was installed, so it won't update it\. Update it with its own installer\./);
  assert.match(html, />How to update it yourself</);
});

test("the page lists the install setting with both options, and changing it keeps the other three", async () => {
  const html = page({ mode: "check_and_update", when: "on_button", online: "on", install: "on" });
  for (const word of ["Install from the app", "Offer Install for harnesses that are not installed", "Don't offer Install", "Install runs Homebrew when it is on this Mac, else npm\\. It never uses sudo or a script from a website\\."]) assert.match(html, new RegExp(word));
  const sent = [];
  await view.chooseInstall({ mode: "check_only", when: "on_open", online: "off", install: "on" }, "off", (settings) => { sent.push(settings); return Promise.resolve({ kind: "ok", value: undefined }); });
  assert.deepEqual(sent, [{ mode: "check_only", when: "on_open", online: "off", install: "off" }]);
});

test("every install refusal has its own plain sentence, none shared with an update code", () => {
  const codes = ["harness_install_no_tool", "harness_install_off", "harness_install_already", "harness_install_not_found", "harness_install_failed", "harness_install_needs_node", "harness_install_list_invalid"];
  const update = ["harness_update_unknown_install", "harness_update_with_app", "harness_update_not_installed", "harness_update_failed", "harness_update_outcome_unknown"];
  assert.equal(new Set([...codes, ...update].map(api.updateCopy)).size, codes.length + update.length);
  for (const code of codes) assert.doesNotMatch(api.updateCopy(code), /harness_|\u2014/, code);
});

test("an update whose old version could not be read is not called an install", () => {
  const html = row({ entry: ready("claude-code", "Claude Code"), versions: { ...claude, installed: "1.2.3", latest: { kind: "up_to_date" }, last: { ok: true, code: null, output: "", from: null, to: "1.2.3", ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(html, /Version 1\.2\.3 is installed now\. New agents use it; no restart needed\./);
  assert.doesNotMatch(html, /Installed version/);
  const stopped = row({ entry: missing("aider", "Aider"), versions: { ...aider, last: { ok: false, code: "harness_update_timeout", output: "still downloading", from: null, to: null, ended_ms: 1 } }, mode: "check_and_update" });
  assert.match(stopped, /<summary[^>]*>What the installer said<\/summary>/);
});

test("a second job says another update or install is running, not this harness", () => {
  assert.equal(api.updateCopy("harness_update_busy"), "Another harness update or install is running.");
});

test("the confirm opens on a fresh read, so it shows the line the engine has now", async () => {
  const npm = { ...claude, method: { kind: "npm", package: "@anthropic-ai/claude-code" }, command: "/opt/homebrew/bin/npm install -g @anthropic-ai/claude-code" };
  const reads = [];
  const fresh = await api.readForConfirm("claude-code", "update", (id, look) => { reads.push([id, look]); return Promise.resolve({ kind: "ok", value: npm }); });
  assert.deepEqual(reads, [["claude-code", false]]);
  assert.equal(fresh.open, true);
  const html = confirm({ versions: fresh.read.value, name: "Claude Code" });
  assert.match(html, /<pre[^>]*>\/opt\/homebrew\/bin\/npm install -g @anthropic-ai\/claude-code<\/pre>/);
  assert.doesNotMatch(html, /brew upgrade/);
  assert.match(html, /Installed with npm \(@anthropic-ai\/claude-code\)\./);
});

test("no confirm opens when the fresh read has no line, says a job runs, or fails", async () => {
  const answer = (value) => () => Promise.resolve(value);
  const gone = await api.readForConfirm("claude-code", "update", answer({ kind: "ok", value: { ...claude, command: null, refusal: "harness_update_tool_missing" } }));
  assert.equal(gone.open, false);
  assert.equal(gone.read.value.refusal, "harness_update_tool_missing");
  assert.equal((await api.readForConfirm("claude-code", "update", answer({ kind: "ok", value: { ...claude, running: true } }))).open, false);
  assert.equal((await api.readForConfirm("aider", "install", answer({ kind: "error", code: "harness_update_unavailable", message: "x" }))).open, false);
  assert.equal((await api.readForConfirm("aider", "install", answer({ kind: "ok", value: aider }))).open, true);
  assert.equal((await api.readForConfirm("aider", "update", answer({ kind: "ok", value: aider }))).open, false);
  assert.equal(api.jobLine(aider, "install"), aider.install);
  assert.equal(api.jobLine(claude, "update"), claude.command);
});

test("a row that opens while a job runs is read until the job ends, then says how it ended with its buttons on", async () => {
  const ended = { ...claude, installed: "2.1.322", latest: { kind: "up_to_date" }, last: { ok: true, code: null, output: "", from: "2.1.309", to: "2.1.322", ended_ms: 1 } };
  let reads = 0;
  const last = await api.pollWhileRunning("claude-code", (id) => {
    reads += 1;
    return Promise.resolve({ kind: "ok", value: reads < 3 ? { ...claude, id, running: true } : ended });
  }, () => Promise.resolve(), () => true);
  assert.equal(reads, 3);
  const html = row({ entry: ready("claude-code", "Claude Code"), versions: last.value, mode: "check_and_update" });
  assert.match(html, /Updated from 2\.1\.309 to 2\.1\.322\./);
  assert.doesNotMatch(html, /Updating/);
  assert.doesNotMatch(html, /disabled=""/);
});

test("a row that is gone stops reading", async () => {
  let reads = 0;
  let waits = 0;
  const last = await api.pollWhileRunning("claude-code", (id) => {
    reads += 1;
    return Promise.resolve({ kind: "ok", value: { ...claude, id, running: true } });
  }, () => { waits += 1; return Promise.resolve(); }, () => waits < 2);
  assert.equal(reads, 2);
  assert.equal(last.value.running, true);
});

test("a failed settings read says why above the list and offers to try again, with no update or install controls", () => {
  const catalog = { kind: "ok", value: [ready("claude-code", "Claude Code"), missing("aider", "Aider")] };
  const html = markup(createElement(view.HarnessesView, { catalog, onCheck: quiet, settings: null, settingsError: { message: api.updateCopy("harness_update_settings_unreadable"), onRetry: quiet } }));
  assert.match(html, /CrazyAgent couldn't read the harness update settings\./);
  assert.match(html, /<button[^>]*>.*?Try again<\/button>/);
  assert.doesNotMatch(html, />Check for updates</);
  assert.doesNotMatch(html, />Update</);
  assert.doesNotMatch(html, />Install</);
  assert.doesNotMatch(markup(createElement(view.HarnessesView, { catalog, onCheck: quiet, settings: null })), /Try again/);
});

const NEEDS_LINE = "codex needs node, which is missing. Install Node.js from nodejs.org, or run brew install node in Terminal. Then try again.";
const NODE_FAILED = "Node.js didn't download. Check the connection, then try again.";
const codexNeeds = { id: "codex", method: { kind: "npm", package: "@openai/codex" }, installed: null, latest: { kind: "not_checked" }, command: "/opt/homebrew/bin/npm install -g @openai/codex@latest", install: null, refusal: null, guide: null, running: false, last: null, needs: { program: "node", line: NEEDS_LINE } };
const setupWith = (state, running = false) => ({ dependencies: [{ name: "node", label: "Node.js", state }], pack: [], running, version: 1 });

test("a found harness the tool resolver can't run reads Can't run with the resolver's line and Try again, never Ready or a version", () => {
  const html = row({ entry: ready("codex", "Codex"), versions: codexNeeds, mode: "check_and_update", onRetry: quiet });
  assert.match(html, />Can't run</);
  assert.match(html, /<span class="[^"]*pj-error[^"]*">codex needs node, which is missing\. Install Node\.js from nodejs\.org, or run brew install node in Terminal\. Then try again\.<\/span>/);
  assert.match(html, /<button[^>]*aria-label="Try again: Codex"[^>]*>Try again<\/button>/);
  assert.doesNotMatch(html, />Ready</);
  assert.doesNotMatch(html, /Version/);
  assert.doesNotMatch(html, />Check for updates</);
  assert.doesNotMatch(html, />Update</);
  assert.doesNotMatch(html, />Install Node\.js</, "the Node.js offer is the page's, once, not each row's");
});

test("with harness updates off a harness that can't run still reads Can't run, and one that runs shows no version", () => {
  const html = row({ entry: ready("codex", "Codex"), versions: codexNeeds, mode: "off", onRetry: quiet });
  assert.match(html, />Can't run</);
  assert.match(html, /codex needs node, which is missing\./);
  assert.match(html, /<button[^>]*>Try again<\/button>/);
  const runs = row({ entry: ready("codex", "Codex"), versions: { ...codexNeeds, needs: null }, mode: "off", onRetry: quiet });
  assert.match(runs, />Ready</);
  assert.doesNotMatch(runs, /Version unknown\./);
  assert.doesNotMatch(runs, /Try again/);
});

test("a harness the engine lists as Can't run is grouped under Can't run, never Ready, and reads the engine's line and Try again before its versions arrive", () => {
  const blocked = entry("codex", "Codex", { kind: "cant_run", reason: NEEDS_LINE });
  const groups = view.harnessGroups([ready("claude-code", "Claude Code"), blocked, limited("cursor", "Cursor CLI"), missing("gemini-cli", "Gemini CLI")]);
  assert.deepEqual(groups.map((group) => [group.title, group.entries.map((item) => item.id)]), [
    ["Ready", ["claude-code"]],
    ["Limited control", ["cursor"]],
    ["Can't run", ["codex"]],
    ["Not installed", ["gemini-cli"]],
  ]);
  const html = row({ entry: blocked, versions: null, mode: "check_and_update", onRetry: quiet });
  assert.match(html, />Can't run</);
  assert.match(html, /<span class="[^"]*pj-error[^"]*">codex needs node, which is missing\./);
  assert.match(html, /<button[^>]*aria-label="Try again: Codex"[^>]*>Try again<\/button>/);
  assert.doesNotMatch(html, />Ready</);
  assert.doesNotMatch(html, /Limited control/);
  const runs = row({ entry: blocked, versions: { ...codexNeeds, needs: null, installed: "0.167.1" }, mode: "check_and_update", onRetry: quiet });
  assert.match(runs, />Ready</, "a fresh read that runs wins over the catalog's older word");
  assert.doesNotMatch(runs, /Can't run|Try again/);
});

test("a harness that is not installed never reads Can't run", () => {
  const html = row({ entry: missing("codex", "Codex"), versions: { ...codexNeeds, install: "/opt/homebrew/bin/npm install -g @openai/codex" }, mode: "check_and_update", onRetry: quiet });
  assert.match(html, />Not installed</);
  assert.doesNotMatch(html, /Can't run|Try again/);
});

test("the page reads every harness with updates off too, and looks for a newer version only for an installed one when the settings say so", () => {
  assert.equal(view.openLook(true, "check_and_update", true), true);
  assert.equal(view.openLook(true, "check_only", true), true);
  assert.equal(view.openLook(true, "off", true), false);
  assert.equal(view.openLook(true, "check_and_update", false), false);
  assert.equal(view.openLook(false, "check_and_update", true), false);
});

test("the page offers one Install Node.js while a harness needs Node, says it installs, and says why it failed", () => {
  const catalog = { kind: "ok", value: [ready("codex", "Codex"), ready("claude-code", "Claude Code")] };
  const shown = (node) => markup(createElement(view.HarnessesView, { catalog, onCheck: quiet, settings: api.UPDATE_SETTINGS_DEFAULT, onSettings: quiet, node, onInstallNode: quiet }));
  assert.equal(shown({ kind: "offer" }).match(/>Install Node\.js<\/button>/g)?.length, 1);
  const installing = shown({ kind: "installing" });
  assert.match(installing, /Installing Node\.js\./);
  assert.doesNotMatch(installing, />Install Node\.js<\/button>/);
  const failed = shown({ kind: "failed", reason: NODE_FAILED });
  assert.match(failed, /Node\.js didn't download\. Check the connection, then try again\./);
  assert.equal(failed.match(/>Install Node\.js<\/button>/g)?.length, 1);
  assert.doesNotMatch(shown({ kind: "none" }), /Node\.js/);
});

test("the page's Node.js offer follows the listed harnesses that need Node and setup's Node row", () => {
  const reads = { codex: codexNeeds };
  assert.deepEqual(view.pageNode(["codex"], reads, null), { kind: "offer" });
  assert.deepEqual(view.pageNode(["codex"], reads, setupWith({ state: "missing" })), { kind: "offer" });
  assert.deepEqual(view.pageNode([], reads, null), { kind: "none" }, "a harness gone from the list offers nothing");
  assert.deepEqual(view.pageNode(["codex"], { codex: { ...codexNeeds, needs: { program: "ca-missing-runtime", line: "x" } } }, null), { kind: "none" });
  assert.deepEqual(view.pageNode(["codex"], reads, setupWith({ state: "installing" })), { kind: "installing" });
  assert.deepEqual(view.pageNode(["codex"], reads, setupWith({ state: "missing" }, true)), { kind: "installing" });
  assert.deepEqual(view.pageNode(["codex"], reads, setupWith({ state: "failed", reason: NODE_FAILED })), { kind: "failed", reason: NODE_FAILED });
  assert.deepEqual(view.pageNode(["codex"], reads, setupWith({ state: "installed", version: "26.13.1" })), { kind: "none" });
});

test("the page reads setup again while a listed harness needs Node and the Node row is checking, installing, or settled while a read still needs Node, and stops on a settled offer", () => {
  const reads = { codex: codexNeeds };
  assert.equal(view.pageNodeReads(["codex"], reads, setupWith({ state: "checking" })), true, "a Node row still checking is read again");
  assert.equal(view.pageNodeReads(["codex"], reads, setupWith({ state: "installing" })), true);
  assert.equal(view.pageNodeReads(["codex"], reads, setupWith({ state: "missing" }, true)), true, "a setup run that goes on is read until it ends");
  assert.equal(view.pageNodeReads(["codex"], reads, setupWith({ state: "installed", version: "26.13.1" })), true, "Node is there but a read still needs it");
  assert.equal(view.pageNodeReads(["codex"], reads, setupWith({ state: "missing" })), false, "a settled Missing row shows the offer and is not read again");
  assert.equal(view.pageNodeReads(["codex"], reads, setupWith({ state: "failed", reason: NODE_FAILED })), false);
  assert.equal(view.pageNodeReads(["codex"], { codex: { ...codexNeeds, needs: null } }, setupWith({ state: "checking" })), false, "nothing listed needs Node");
  assert.equal(view.pageNodeReads([], reads, setupWith({ state: "checking" })), false, "a harness gone from the list needs nothing");
});
