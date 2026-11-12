import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import "./load-source.mjs";

const client = await import("../src/api-harness-settings.ts");
const view = await import("../src/screens/settings/harness-page.tsx");
const list = await import("../src/screens/settings/harnesses.tsx");

const REASON = "CrazyAgent runs agents in its sandbox and sets the permission mode itself.";
const setting = (extra = {}) => ({
  key: "autoCompactEnabled", label: "Auto-compact", help: "Compacts the conversation when it nears the context limit.", type: "bool",
  options: [], default: true, min: null, max: null, max_bytes: null, here: null, here_valid: true, listed: true, value: true, source: "default", version: 0,
  access: { kind: "editable" }, ...extra,
});
const locked = (extra = {}) => setting({
  key: "permissions.defaultMode", label: "Permission mode", help: "How the harness asks before it acts.", type: "enum",
  options: ["default", "acceptEdits", "bypassPermissions"], default: "default", value: "bypassPermissions", source: "engine",
  access: { kind: "locked", reason: REASON }, ...extra,
});
const readOnly = (reason, extra = {}) => setting({ key: "spinnerTips", label: "Spinner tips", type: "string", value: "on", source: "not_applied",
  access: { kind: "read_only", reason }, ...extra });
const page = (keys, extra = {}) => ({
  harness: "claude-code", name: "Claude Code", installed: "2.1.4", qualified: true, qualified_versions: ["2.1.4"], project_id: null, keys, ...extra,
});
const row = (item, extra = {}) => renderToStaticMarkup(createElement(view.SettingRow, {
  setting: item, scope: "app", draft: item.access.kind === "editable" ? client.draftOf(item) : null, busy: false, note: "",
  onDraft: () => {}, onSave: () => {}, onReset: () => {}, ...extra,
}));
const screen = (loaded, extra = {}) => renderToStaticMarkup(createElement(view.HarnessSettingsView, {
  page: loaded, scope: "app", query: "", drafts: {}, busy: null, notes: {},
  onQuery: () => {}, onDraft: () => {}, onSave: () => {}, onReset: () => {}, onRetry: () => {}, ...extra,
}));
const controls = /<input|<select|role="switch"|>Save<|>Use default<|>Clear project value</;

test("a locked key shows its reason and no control", () => {
  const markup = row(locked());
  assert.match(markup, /aria-label="Locked"/);
  assert.match(markup, /<span class="mono">bypassPermissions<\/span>/);
  assert.match(markup, />CrazyAgent runs agents in its sandbox and sets the permission mode itself.</);
  assert.match(markup, />Permission mode</);
  assert.match(markup, />permissions.defaultMode</);
  assert.doesNotMatch(markup, controls, "a locked key has no switch, select, field, save or reset");
  const project = row(locked({ here: "default" }), { scope: "project" });
  assert.doesNotMatch(project, controls, "a value forged into the project scope still offers no control");
  assert.match(project, /<span class="mono">bypassPermissions<\/span>/, "the engine value shows, never the stored one");
  assert.throws(() => row(locked(), { draft: "default" }), /only an editable key has a draft/);
  assert.throws(() => row(setting(), { draft: null }), /only an editable key has a draft/);
  assert.throws(() => row(locked({ source: "app" })), /a locked key shows the engine's value/);
  assert.equal(client.canReset(locked({ here: "default" })), false);
});

test("an ordinary key is a switch, select or field with help, Save once changed and the scope's reset", () => {
  const plain = row(setting());
  assert.match(plain, /role="switch"[^>]*aria-checked="true"/);
  assert.match(plain, />Compacts the conversation when it nears the context limit.</);
  assert.match(plain, />Default</);
  assert.doesNotMatch(plain, />Save</, "nothing to save until the switch changes");
  assert.doesNotMatch(plain, />Use default</, "nothing stored, nothing to reset");
  const changed = row(setting(), { draft: false });
  assert.match(changed, /role="switch"[^>]*aria-checked="false"/);
  assert.match(changed, /<button[^>]*>Save<\/button>/);
  const stored = row(setting({ here: false, value: false, source: "app", version: 4 }));
  assert.match(stored, />Set for every project</);
  assert.match(stored, /<button[^>]*>Use default<\/button>/);
  const override = row(setting({ here: true, value: true, source: "project", version: 2 }), { scope: "project" });
  assert.match(override, />Set for this project</);
  assert.match(override, /<button[^>]*>Clear project value<\/button>/);
  const inherited = row(setting({ value: false, source: "app" }), { scope: "project" });
  assert.match(inherited, />From the app setting</);
  assert.doesNotMatch(inherited, />Clear project value</, "a project page resets only its own override");
  const choice = row(setting({ key: "theme", label: "Theme", type: "enum", options: ["dark", "light"], default: "dark", value: "dark" }));
  assert.match(choice, /<select[^>]*>/);
  assert.match(choice, /<option value="light">light<\/option>/);
  const field = row(setting({ key: "cleanupPeriodDays", label: "Keep transcripts", type: "integer", min: 1, max: 365, default: 30, value: 30 }));
  assert.match(field, /<input[^>]*inputMode="numeric"[^>]*value="30"/);
  assert.match(row(setting(), { busy: true, draft: false }), /<button[^>]*disabled=""[^>]*>.*Saving\.\.\.<\/button>/);
  assert.match(row(setting(), { note: "This value doesn't fit the setting." }), /role="status">This value doesn&#x27;t fit the setting.</);
});

test("read-only keys show their value and the engine's reason, never a control", () => {
  const markup = row(readOnly("Not qualified for Claude Code 2.2.0"));
  assert.match(markup, />Not qualified for Claude Code 2.2.0</);
  assert.match(markup, /<span class="mono">on<\/span>/);
  assert.doesNotMatch(markup, controls);
  assert.doesNotMatch(markup, /aria-label="Locked"/, "only a key CrazyAgent sets itself carries the lock");
  const appOnly = row(readOnly("Set once for the whole app in Settings > Harnesses", { here: null, value: 30, source: "app" }), { scope: "project" });
  assert.match(appOnly, />Set once for the whole app in Settings &gt; Harnesses</);
  assert.doesNotMatch(appOnly, controls);
  assert.match(row(readOnly("Not qualified", { value: null })), /<span class="mono">Not set<\/span>/);
  assert.equal(client.canReset(readOnly("r", { here: "on" })), false);
  assert.equal(client.versionLine(page([])), "Claude Code 2.1.4 is qualified. You can change the settings below.");
  assert.equal(client.versionLine(page([], { installed: "2.2.0", qualified: false })),
    "Claude Code 2.2.0 isn't qualified yet, so every setting is read-only and agents start with CrazyAgent's defaults. Qualified versions: 2.1.4.");
  assert.equal(client.versionLine(page([], { name: "Codex", installed: null, qualified: false, qualified_versions: [] })),
    "CrazyAgent couldn't read the installed Codex version, so every setting is read-only and agents start with CrazyAgent's defaults. Qualified versions: none yet.");
  assert.equal(client.sourceLine({ source: "not_applied" }, "app"), "Not applied: agents start with CrazyAgent's defaults");
});

test("a saved value this harness version no longer allows shows as not used, with a reset that clears it", () => {
  const outdated = setting({ key: "model_reasoning_effort", label: "Reasoning effort", type: "enum", options: ["low", "medium", "high"], default: "medium",
    here: "minimal", here_valid: false, value: "medium", source: "default", version: 3 });
  const markup = row(outdated);
  assert.match(markup, /<select[^>]*>/, "the owner can still pick an allowed value");
  assert.match(markup, />The saved value minimal isn&#x27;t allowed for this harness version, so it isn&#x27;t used.</);
  assert.match(markup, /<button[^>]*>Use default<\/button>/);
  assert.equal(client.canReset(outdated), true);
  assert.equal(client.storedLine(outdated), "The saved value minimal isn't allowed for this harness version, so it isn't used.");
  assert.equal(client.storedLine(setting({ here: false, value: false, source: "app" })), "");
  assert.doesNotMatch(row(setting({ here: false, value: false, source: "app" })), /isn&#x27;t used/);
  const retired = readOnly("Not qualified for Codex 0.51.0", { key: "hide_agent_reasoning", label: "hide_agent_reasoning", here: true, here_valid: false, value: null, listed: false });
  const gone = row(retired);
  assert.match(gone, />This harness version no longer lists this setting, so the saved value On isn&#x27;t used.</);
  assert.match(gone, /<button[^>]*>Clear saved value<\/button>/);
  assert.doesNotMatch(gone, /<input|<select|role="switch"|>Save<|aria-label="Locked"/);
  assert.equal(client.canReset(retired), true);
  assert.equal(client.resetLabel(retired, "app"), "Clear saved value");
  assert.equal(client.resetLabel(retired, "project"), "Clear saved value");
  const waiting = readOnly("Not qualified for Codex 0.49.0", { here: "on", here_valid: false });
  assert.equal(client.canReset(waiting), false, "the engine refuses a reset on a key of an unqualified version");
  assert.doesNotMatch(row(waiting), /<button/);
  assert.throws(() => client.storedLine(setting({ here: null, here_valid: false })), /only a saved value can be not valid/);
  assert.throws(() => row(readOnly("r", { listed: false, here: null, here_valid: true })), /an unlisted key shows only for its saved value/);
  assert.throws(() => row(setting({ listed: false, here: true, here_valid: false })), /an unlisted key is read-only/);
});

test("the page groups keys in catalog order, filters them and says when changes apply", () => {
  const keys = [
    setting({ key: "a", label: "Alpha" }), locked({ key: "b", label: "Bravo" }), setting({ key: "c", label: "Charlie", help: "Uses the model picker." }),
    readOnly("Not qualified for Claude Code 2.2.0", { key: "d", label: "Delta" }),
  ];
  assert.deepEqual(client.settingGroups(keys).map((group) => [group.title, group.keys.map((key) => key.key)]),
    [["Settings", ["a", "c"]], ["Set by CrazyAgent", ["b"]], ["Read-only", ["d"]]]);
  assert.deepEqual(client.settingGroups(keys, "MODEL").map((group) => group.keys.map((key) => key.key)), [["c"]], "the filter reads help too");
  assert.deepEqual(client.settingGroups(keys, "  "), client.settingGroups(keys));
  assert.deepEqual(client.settingGroups(keys, "zulu"), []);
  const markup = screen({ kind: "ok", value: page(keys) });
  assert.match(markup, />Claude Code 2.1.4 is qualified. You can change the settings below.</);
  assert.match(markup, />Changes apply the next time a master or agent starts with this harness. Running ones keep their settings.</);
  assert.match(markup, /aria-label="Settings"[\s\S]*aria-label="Set by CrazyAgent"[\s\S]*aria-label="Read-only"/);
  assert.match(screen({ kind: "ok", value: page(keys) }, { query: "zulu" }), />No setting matches the filter.</);
  assert.match(screen({ kind: "ok", value: page([]) }), />This harness lists no settings.</);
  const project = screen({ kind: "ok", value: page(keys, { project_id: "p-1" }) }, { scope: "project" });
  assert.match(project, /A value set here wins over the app value for this project only./);
  assert.throws(() => screen({ kind: "ok", value: page(keys) }, { scope: "project" }), /a project page names its project/);
  assert.throws(() => client.settingGroups(Array.from({ length: client.ROWS_MAX + 1 }, (_, at) => setting({ key: `k${at}` }))), /at most ROWS_MAX/);
});

test("a harness with no settings list, a missing build and a failed read each read plainly", () => {
  assert.match(screen({ kind: "error", code: "harness_settings_no_catalog", message: "" }), /CrazyAgent has no settings list for this harness yet/);
  assert.match(screen({ kind: "not_ready" }), /This page isn&#x27;t ready yet/);
  assert.match(screen({ kind: "error", code: "harness_settings_unavailable", message: "" }), /This page isn&#x27;t ready yet/);
  assert.match(screen({ kind: "error", code: "owner_busy", message: "" }), /role="alert"[\s\S]*The engine is busy with other owner requests/);
  assert.match(screen({ kind: "loading" }), /skeleton/);
});

test("a draft sends a value of the key's type inside its bounds, as the engine checks it again", () => {
  const number = { type: "integer", options: [], min: 1, max: 365, max_bytes: null };
  assert.deepEqual(client.draftValue(number, " 30 "), { value: 30 });
  assert.deepEqual(client.draftValue(number, "365"), { value: 365 });
  for (const text of ["0", "366", "3.5", "1e3", "", "thirty", "99999999999999999"]) {
    assert.deepEqual(client.draftValue(number, text), { error: "Use a whole number from 1 to 365." }, text);
  }
  assert.deepEqual(client.draftValue({ ...number, max: null }, "0"), { error: "Use a whole number of at least 1." });
  assert.deepEqual(client.draftValue({ ...number, min: null, max: null }, "-4"), { value: -4 });
  const text = { type: "string", options: [], min: null, max: null, max_bytes: 8 };
  assert.deepEqual(client.draftValue(text, "abcdefgh"), { value: "abcdefgh" });
  assert.deepEqual(client.draftValue(text, "\u00e9".repeat(5)), { error: "Use at most 8 bytes." }, "bytes, not characters");
  for (const broken of ["two\nlines", "tab\there", "bell\u0007"]) {
    assert.deepEqual(client.draftValue(text, broken), { error: "Use one line of plain text." }, JSON.stringify(broken));
  }
  assert.deepEqual(client.draftValue({ ...text, max_bytes: null }, "a".repeat(client.VALUE_BYTES_MAX)), { value: "a".repeat(client.VALUE_BYTES_MAX) });
  assert.deepEqual(client.draftValue({ ...text, max_bytes: null }, "a".repeat(client.VALUE_BYTES_MAX + 1)), { error: `Use at most ${client.VALUE_BYTES_MAX} bytes.` });
  const choice = { type: "enum", options: ["dark", "light"], min: null, max: null, max_bytes: null };
  assert.deepEqual(client.draftValue(choice, "light"), { value: "light" });
  assert.deepEqual(client.draftValue(choice, "Light"), { error: "Pick one of the options." });
  const toggle = { type: "bool", options: [], min: null, max: null, max_bytes: null };
  assert.deepEqual(client.draftValue(toggle, false), { value: false });
  assert.deepEqual(client.draftValue(toggle, "false"), { error: "Turn this setting on or off." });
  assert.equal(client.draftChanged(setting(), true), false);
  assert.equal(client.draftChanged(setting(), false), true);
  assert.throws(() => client.draftChanged(setting(), "true"), /a draft matches its control/);
  assert.equal(client.draftOf(setting({ type: "integer", value: 30 })), "30");
});

test("agent and catalog text shows through displayText, so hidden characters cannot hide or reorder it", async () => {
  const { displayText } = await import("../src/screens/conversation.tsx");
  const hidden = "Mode\u202e reversed\u200b";
  const markup = row(locked({ label: hidden, help: hidden, value: hidden, access: { kind: "locked", reason: hidden } }));
  assert.ok(!markup.includes("\u202e") && !markup.includes("\u200b"), "no raw bidi or zero-width character reaches the page");
  assert.ok(markup.includes(displayText(hidden)));
  assert.equal(client.valueText(""), "Empty");
  assert.equal(client.valueText(false), "Off");
  assert.equal(client.valueText(12), "12");
  assert.equal(client.valueText(null), "Not set");
  assert.throws(() => client.draftOf(setting({ value: null, source: "app" })), /only the harness default leaves a key without a value/);
});

test("a stale change reads the page again, and every harness settings code reads as a sentence", () => {
  const stale = { kind: "error", code: "harness_settings_stale", message: "" };
  assert.equal(client.isStale(stale), true);
  assert.equal(view.changeNote(stale), "This setting changed since you opened it. The page now shows the saved value; make your change again.");
  assert.equal(view.changeNote({ kind: "ok", value: undefined }), "");
  assert.equal(view.changeNote({ kind: "not_ready" }), "Not ready yet: changing harness settings isn't in this build.");
  assert.equal(view.changeNote({ kind: "error", code: "harness_settings_key_locked", message: "" }), "CrazyAgent sets this one itself, so it can't be changed.");
  assert.equal(client.harnessSettingsRefusal({ code: "made_up_code" }), "Something went wrong. Try again. (made_up_code)");
  for (const code of client.HARNESS_SETTINGS_REFUSALS) {
    const line = client.harnessSettingsRefusal({ code });
    assert.match(line, /^[A-Z].*\.$/, code);
    assert.doesNotMatch(line, /_/, `${code} reads as a sentence: ${line}`);
  }
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/harness_settings.rs", import.meta.url), "utf8");
  const start = engine.indexOf("pub const HARNESS_SETTINGS_CODES");
  assert.ok(start >= 0, "the engine lists its harness settings codes");
  const own = [...engine.slice(start, engine.indexOf("];", start)).matchAll(/"([a-z_]+)"/g)].map((match) => match[1]);
  assert.ok(own.length >= 3, `engine harness settings codes: ${own.length}`);
  const transport = own.filter((code) => /_(request|reply)_(bounds|invalid)$/.test(code));
  for (const code of own.filter((code) => !transport.includes(code))) {
    assert.ok(client.HARNESS_SETTINGS_REFUSALS.includes(code), `${code} has its own sentence`);
  }
  const bridge = readFileSync(new URL("../src-tauri/src/bridge/harness_settings.rs", import.meta.url), "utf8");
  for (const code of ["harness_settings_outcome_unknown", "harness_settings_value_invalid"]) {
    assert.ok(bridge.includes(`"${code}"`), `the bridge answers ${code}`);
    assert.ok(client.HARNESS_SETTINGS_REFUSALS.includes(code), `${code} has its own sentence`);
  }
});

test("a Ready or Limited harness row opens its page; a harness that isn't installed expands", async () => {
  const entry = (id, status) => ({ id, name: id, status, status_label: "", executable: null, headless: "x -p", tools: { kind: "switchable" },
    model_selection: true, account_kinds: [], team: { kind: "available" }, aliases: [] });
  assert.equal(list.harnessPageHref(entry("claude-code", { kind: "ready" })), "#/settings/harnesses?harness=claude-code");
  assert.equal(list.harnessPageHref(entry("rcli", { kind: "limited", reason: "no tool switch" })), "#/settings/harnesses?harness=rcli");
  assert.equal(list.harnessPageHref(entry("aider", { kind: "not_installed" })), null);
  const markup = renderToStaticMarkup(createElement(list.HarnessesView, { catalog: { kind: "ok", value: [entry("codex", { kind: "ready" }), entry("aider", { kind: "not_installed" })] }, onCheck: () => {} }));
  assert.match(markup, /<a class="harness-row" href="#\/settings\/harnesses\?harness=codex" aria-label="codex settings">/);
  assert.match(markup, /<button type="button" class="harness-row" aria-expanded="false">/);
  const missing = renderToStaticMarkup(createElement(list.HarnessScreenView, { catalog: { kind: "ok", value: [entry("codex", { kind: "ready" })] }, id: "kiro" }));
  assert.match(missing, />CrazyAgent doesn&#x27;t know the harness kiro.</);
  assert.match(missing, /href="#\/settings\/harnesses">.*All harnesses<\/a>/);
  const { projectHarnesses } = await import("../src/screens/project/settings.tsx");
  assert.deepEqual(projectHarnesses([entry("codex", { kind: "ready" }), entry("aider", { kind: "not_installed" }), entry("rcli", { kind: "limited", reason: "r" })]).map((item) => item.id), ["codex", "rcli"]);
});

test("the harness page has a route, and its commands go through the bridge with only the harness, project, key, value and version", async () => {
  const { parseRoute, formatRoute } = await import("../src/ui/router.ts");
  assert.deepEqual(parseRoute("#/settings/harnesses?harness=claude-code"), { name: "settings", tab: "harnesses", harness: "claude-code" });
  assert.equal(formatRoute({ name: "settings", tab: "harnesses", harness: "claude-code" }), "#/settings/harnesses?harness=claude-code");
  for (const hash of ["#/settings/harnesses?harness=Claude", "#/settings/harnesses?harness=../x", "#/settings/harnesses?harness=", "#/settings/harnesses?harness=" + "a".repeat(33), "#/settings/pools?harness=codex"]) {
    assert.equal(parseRoute(hash).harness, undefined, hash);
  }
  assert.equal(client.harnessIdValid("qwen-code"), true);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/harness_settings.rs", import.meta.url), "utf8");
  assert.match(engine, /pub const HARNESS_BYTES_MAX: usize = 32;/, "the window bound below is the engine's");
  assert.equal(client.harnessIdValid("a".repeat(32)), true);
  assert.equal(client.harnessIdValid("a".repeat(33)), false, "the engine takes at most HARNESS_BYTES_MAX = 32 bytes");
  assert.equal(parseRoute("#/settings/harnesses?harness=" + "a".repeat(32)).harness, "a".repeat(32));
  const source = readFileSync(new URL("../src/api-harness-settings.ts", import.meta.url), "utf8");
  assert.match(source, /call<HarnessSettingsPage>\("desktop_harness_settings", \{ harness, projectId \}\)/);
  assert.match(source, /call<void>\("desktop_set_harness_setting", \{ harness, projectId, key, value, version \}\)/);
  assert.match(source, /call<void>\("desktop_reset_harness_setting", \{ harness, projectId, key, version \}\)/);
  const project = readFileSync(new URL("../src/screens/project/settings.tsx", import.meta.url), "utf8");
  assert.match(project, /\{ id: "harnesses", label: "Harnesses" \}/);
  assert.match(project, /<HarnessPage key=\{current.id\} harness=\{current.id\} projectId=\{data.id\} \/>/);
});

test("a key left to the harness default reads Harness default and saves only a value the owner picks", () => {
  assert.equal(client.HARNESS_DEFAULT, "Harness default");
  const effort = setting({ key: "model_reasoning_effort", label: "Reasoning effort", type: "enum", options: ["low", "high"], default: null, value: null });
  assert.equal(client.shownValue(effort), "Harness default");
  assert.equal(client.shownValue({ ...effort, source: "not_applied" }), "Not set", "only the default source reads as the harness default");
  assert.equal(client.shownValue(setting()), "On");
  assert.equal(client.draftOf(effort), "");
  assert.equal(client.draftChanged(effort, ""), false);
  assert.equal(client.draftChanged(effort, "high"), true);
  assert.deepEqual(client.draftValue(effort, ""), { error: "Pick one of the options." });
  assert.throws(() => client.draftOf({ ...effort, source: "app" }), /only the harness default leaves a key without a value/);
  const choice = row(effort);
  assert.match(choice, /<option value="" selected="">Harness default<\/option>/);
  assert.match(choice, /<option value="high">high<\/option>/);
  assert.doesNotMatch(choice, />Save</, "nothing to save while the harness decides");
  assert.match(row(effort, { draft: "high" }), /<button[^>]*>Save<\/button>/);

  const toggle = setting({ key: "analytics.enabled", label: "Analytics", default: null, value: null });
  assert.equal(client.draftOf(toggle), "");
  assert.equal(client.draftChanged(toggle, ""), false);
  assert.equal(client.draftChanged(toggle, false), true);
  assert.deepEqual(client.draftValue(toggle, false), { value: false });
  assert.deepEqual(client.draftValue(toggle, ""), { error: "Turn this setting on or off." });
  const unset = row(toggle);
  assert.doesNotMatch(unset, /role="switch"/, "a switch cannot show that the harness decides");
  assert.match(unset, /<option value="" selected="">Harness default<\/option><option value="on">On<\/option><option value="off">Off<\/option>/);
  assert.match(row(toggle, { draft: false }), /<option value="off" selected="">Off<\/option>/);
  assert.match(row(toggle, { draft: false }), /<button[^>]*>Save<\/button>/);
  assert.match(row(setting({ value: false, source: "app", here: false })), /role="switch"[^>]*aria-checked="false"/, "a set value is a switch again");

  const number = row(setting({ key: "history.max_bytes", label: "History size", type: "integer", min: 0, max: 100, default: null, value: null }));
  assert.match(number, /placeholder="Harness default"/);
  assert.match(number, /value=""/);
  const shown = row(readOnly("Not qualified for Codex 0.49.0", { type: "enum", options: ["low"], default: null, value: null, source: "default" }));
  assert.match(shown, /<span class="mono">Harness default<\/span>/);
});
