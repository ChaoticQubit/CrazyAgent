import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-app-plugins.ts");
const screen = await import("../src/screens/settings/app-plugins.tsx");

const SHA = "5e".repeat(32);
const OLD_SHA = "4d".repeat(32);
const HTTPS = { kind: "http", url: "https://api.openai.com/v1/embeddings", hosts: ["api.openai.com"], local_unencrypted: false, dimensions: 256 };
const PROBLEM = { code: "app_plugin_url_not_https", line: 7, field: "http.url", message: "The address must start with https://." };
const WORDS = [
  { name: "settings.save_quit", words: "Save what the agents were doing when the app quits" },
  { name: "net.request", words: "Send requests to the internet addresses it lists" },
];
const plugin = (patch = {}) => ({
  name: "openai", title: "OpenAI embeddings", version: "1", description: "Embeds with OpenAI.", kind: "embedding", origin: "bundled",
  status: { kind: "ready" }, enabled: true, runs: HTTPS, secrets: [], sha256: SHA, ...patch,
});
const trust = (patch = {}) => ({
  name: "harbor-embed", title: "Harbor embeddings", version: "2", description: "Embeds with the harbor API.", kind: "embedding",
  runs: { ...HTTPS, hosts: ["api.openai.com", "harbor.example"] }, secrets: ["api_key"], sha256: SHA, replaces: false, changed: false, grew_hosts: [], ...patch,
});
const PLUGINS = [
  plugin({ secrets: [{ name: "api_key", binding: { kind: "none" } }], status: { kind: "needs_key" }, enabled: false }),
  plugin({ name: "harbor-embed", title: "Harbor embeddings", origin: "user", secrets: [{ name: "api_key", binding: { kind: "key" } }] }),
  plugin({ name: "dock-embed", title: "Dock embeddings", origin: "user", status: { kind: "changed", trusted_sha256: OLD_SHA }, enabled: false }),
  plugin({ name: "fresh-embed", title: "Fresh embeddings", origin: "user", status: { kind: "needs_review" }, enabled: false }),
  plugin({ name: "broken", title: "broken", origin: "user", runs: null, status: { kind: "refused", problem: PROBLEM }, enabled: false }),
];
const GRANTS = [
  { plugin: "agent-quit", tier: "t2", origin: "bundled", state: "on", essential: true, capabilities: [{ name: "settings.save_quit", tier: "t2" }] },
  { plugin: "notes", tier: "t2", origin: "bundled", state: "off", essential: false, capabilities: [{ name: "net.request", tier: "t2" }, { name: "settings.save_quit", tier: "t2" }] },
];
const page = (patch = {}) => ({
  listing: { plugins: PLUGINS, more: false }, words: WORDS, notices: [], when_turned_off: { value: "refuse_new_calls", project_override: false }, ...patch,
});
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const actions = new Proxy({}, { get: () => forbidden });
const render = (patch = {}) => renderToStaticMarkup(createElement(screen.AppPluginsView, {
  page: { kind: "ok", value: page() }, grants: GRANTS, dialog: { kind: "none" }, accounts: [], busy: false, failure: "", actions, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const switchOf = (markup, name) => markup.match(new RegExp(`<button id="app-plugin-${name}"[^>]*>`, "u"))?.[0] ?? "";

test("App plugins: every status reads as one label, and a trusted plugin whose files changed reads Changed - review with Review", () => {
  const word = (patch) => api.pluginState(plugin(patch)).word;
  assert.equal(word({ status: { kind: "refused", problem: PROBLEM } }), "Refused");
  assert.equal(word({ status: { kind: "changed", trusted_sha256: OLD_SHA } }), "Changed - review");
  assert.equal(word({ status: { kind: "needs_review" } }), "Needs review");
  assert.equal(word({ status: { kind: "needs_key" } }), "Needs a key");
  assert.equal(word({ enabled: true }), "Enabled");
  assert.equal(word({ enabled: false }), "Off");
  const markup = render();
  const reviews = [...markup.matchAll(/aria-label="Review ([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(reviews, ["Dock embeddings", "Fresh embeddings"]);
  assert.match(switchOf(markup, "dock-embed"), /aria-disabled="true"/u);
  assert.ok(text(markup).includes("Changed - review"));
});

test("App plugins: an essential built-in code plugin has a locked switch with its note, and each code plugin lists what it may do in plain words", () => {
  const markup = render();
  assert.match(switchOf(markup, "agent-quit"), /aria-checked="true"/u);
  assert.match(switchOf(markup, "agent-quit"), /aria-disabled="true"/u);
  assert.ok(text(markup).includes(api.ESSENTIAL_NOTE));
  assert.doesNotMatch(switchOf(markup, "notes"), /aria-disabled/u);
  const shown = text(markup);
  assert.ok(shown.includes("1 permission") && shown.includes("2 permissions"));
  assert.ok(shown.includes("Save what the agents were doing when the app quits"));
  assert.ok(shown.includes("Send requests to the internet addresses it lists"));
  assert.equal(api.permissionWords(WORDS, "service.status"), "service.status", "a name the table lacks shows itself");
});

test("App plugins: an embedding plugin's summary counts the hosts it reaches and the keys it needs", () => {
  assert.equal(api.grantSummary(null, plugin({ secrets: [{ name: "api_key", binding: { kind: "none" } }] })), "reaches 1 host, needs 1 key");
  assert.equal(api.grantSummary(null, plugin({ runs: { ...HTTPS, hosts: ["a.example", "b.example"] } })), "reaches 2 hosts");
  assert.equal(api.grantSummary(null, plugin({ runs: { kind: "local", runtime: "r", download_bytes: 1 } })), "runs on this computer only");
});

test("App plugins: the review sheet lists the hosts that grew first and says the files changed", () => {
  const grown = trust({ changed: true, grew_hosts: ["harbor.example"] });
  assert.deepEqual(api.reviewHosts(grown), ["harbor.example", "api.openai.com"]);
  const shown = text(render({ dialog: { kind: "trust", folder: null, trust: grown, review: true } }));
  assert.ok(shown.includes("Review Harbor embeddings"));
  assert.ok(shown.includes("Its files changed since you trusted it."));
  assert.ok(shown.indexOf("New host: harbor.example") < shown.indexOf("Host: api.openai.com"), "the grown host comes first");
  assert.ok(shown.includes(`SHA-256 ${SHA}`) && shown.includes("Approve"));
  const added = text(render({ dialog: { kind: "trust", folder: "/Users/owner/harbor", trust: trust({ replaces: true }), review: false } }));
  assert.ok(added.includes("Trust Harbor embeddings?") && added.includes("Replaces your plugin named harbor-embed."));
});

test("App plugins: a refused plugin shows its sentence, code, line and field and cannot be approved", () => {
  const shown = text(render({ dialog: { kind: "refused", problem: PROBLEM } }));
  assert.ok(shown.includes("The address must start with https://. (app_plugin_url_not_https, line 7, http.url)"));
  assert.ok(!shown.includes("Approve"));
});

test("App plugins: the turn-off confirm lists what uses the plugin and picks the choice for this one time", () => {
  const users = [{ run_id: "run-1", label: "Fix the login bug" }];
  const shown = text(render({ dialog: { kind: "off", plugin: "notes", title: "notes", users, choice: "stop_runs" } }));
  assert.ok(shown.includes("Turn off notes?") && shown.includes("1 run uses it now:") && shown.includes("Fix the login bug"));
  assert.ok(shown.includes("This time") && shown.includes("Refuse new calls") && shown.includes("Stop runs using it"));
  const markup = render({ dialog: { kind: "off", plugin: "notes", title: "notes", users: [], choice: "stop_runs" } });
  assert.ok(text(markup).includes("Nothing is using it now."), "the confirm shows even when nothing uses it");
  assert.match(markup, /name="app-plugin-when-off-once" value="stop_runs" checked=""/u);
});

test("App plugins: the turn-off setting shows the choice in force and says when a project file sets it", () => {
  const plain = render();
  assert.match(plain, /name="app-plugin-when-off" value="refuse_new_calls" checked=""/u);
  assert.ok(!text(plain).includes(api.PROJECT_OVERRIDE_LINE));
  const project = render({ page: { kind: "ok", value: page({ when_turned_off: { value: "stop_runs", project_override: true } }) } });
  assert.match(project, /name="app-plugin-when-off" value="stop_runs" checked=""/u);
  assert.ok(text(project).includes(api.PROJECT_OVERRIDE_LINE));
});

test("App plugins: removing a plugin with an entered key says the key goes with it", () => {
  const keyed = text(render({ dialog: { kind: "remove", plugin: "harbor-embed", title: "Harbor embeddings", keyed: true } }));
  assert.ok(keyed.includes("Removing it also deletes the key you entered for it."));
  const plain = text(render({ dialog: { kind: "remove", plugin: "dock-embed", title: "Dock embeddings", keyed: false } }));
  assert.ok(!plain.includes("deletes the key"));
  const removes = [...render().matchAll(/aria-label="Remove ([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(removes, ["Harbor embeddings", "Dock embeddings", "Fresh embeddings", "broken"], "only the owner's plugins can be removed");
});

test("App plugins: the key dialog names the hosts the key is sent to and holds no key value", () => {
  const accounts = [{ id: "acct-work", name: "Work OpenAI", method: "api_key" }, { id: "acct-sub", name: "Claude Max", method: "subscription" }];
  assert.deepEqual(api.keyAccounts(accounts).map((account) => account.id), ["acct-work"]);
  const markup = render({ dialog: { kind: "bind", plugin: "openai", title: "OpenAI embeddings", secret: "api_key", hosts: ["api.openai.com"], choice: "key" }, accounts: api.keyAccounts(accounts) });
  assert.ok(text(markup).includes("The key is sent only to api.openai.com."));
  assert.deepEqual(api.keyHosts(plugin()), ["api.openai.com"]);
  const field = markup.match(/<input id="app-plugin-key"[^>]*>/u)?.[0] ?? "";
  assert.match(field, /type="password"/u);
  assert.doesNotMatch(field, /value=/u, "React never holds the key");
});

test("App plugins: a change notice becomes one app notification naming the plugin", () => {
  assert.equal(api.changedLine(page()), null);
  const one = api.changedLine(page({ notices: [{ plugin: "dock-embed", trusted_sha256: OLD_SHA, current_sha256: SHA }] }));
  assert.equal(one, "Dock embeddings changed since you trusted it. It stays off until you review it in App plugins.");
  const two = api.changedLine(page({ notices: [
    { plugin: "dock-embed", trusted_sha256: OLD_SHA, current_sha256: SHA },
    { plugin: "gone", trusted_sha256: OLD_SHA, current_sha256: SHA },
  ] }));
  assert.match(two, /^2 plugins changed since you trusted them: Dock embeddings, gone\./u);
  const state = source("../src/state.ts");
  assert.match(state, /changedLine\(/u, "the app reads the notices once it connects, not only on this page");
});

test("App plugins: loading, not ready and a refused read each keep the title", () => {
  assert.ok(text(render({ page: { kind: "loading" } })).includes("App plugins"));
  assert.ok(text(render({ page: { kind: "not_ready" } })).includes("isn't ready"));
  const refused = text(render({ page: { kind: "error", code: "app_plugins_unavailable", message: "x" } }));
  assert.ok(refused.includes("App plugins") && refused.includes("Try again"));
});

test("App plugins: every app plugin code the engine can answer has its own sentence", () => {
  const engine = source("../../../crates/crazyagent/src/engine/owner/transport/app_plugins.rs");
  const block = engine.match(/pub const APP_PLUGINS_CODES: &\[&str\] = &\[([\s\S]*?)\];/u)?.[1] ?? "";
  const codes = [...block.matchAll(/"(app_plugins?_[a-z_]+)"/gu)].map((match) => match[1]);
  assert.ok(codes.includes("app_plugin_setting_unsafe") && codes.length >= 20, `the engine's codes were read: ${codes.length}`);
  for (const code of codes) assert.ok(Object.hasOwn(api.APP_PLUGIN_REFUSALS, code), `${code} has a sentence`);
  assert.match(api.appPluginRefusal({ code: "app_plugin_folder_unavailable" }), /can't use a plugins folder on this computer yet/u);
});

test("App plugins: each action is a direct bridge call with its own arguments, and an entered key travels only in its own call", () => {
  const file = source("../src/api-app-plugins.ts");
  for (const [command, args] of [
    ["desktop_app_plugins", null], ["desktop_app_plugin_grants", null], ["desktop_app_plugin_preview", "{ folder }"], ["desktop_app_plugin_review", "{ name }"],
    ["desktop_app_plugin_add", "{ folder, sha256, replace }"], ["desktop_app_plugin_trust", "{ name, sha256 }"],
    ["desktop_app_plugin_set_enabled", "{ name, enabled, whenOff }"], ["desktop_app_plugin_users", "{ name }"], ["desktop_app_plugin_remove", "{ name }"],
    ["desktop_app_plugin_bind_account", "{ name, secret, account }"], ["desktop_app_plugin_set_key", "{ name, secret, key }"],
    ["desktop_app_plugin_unbind", "{ name, secret }"], ["desktop_app_plugin_set_when_off", "{ whenOff }"],
  ]) {
    const shape = args === null ? `\\("${command}"\\)` : `\\("${command}", ${args.replace(/[{}]/gu, "\\$&")}\\)`;
    assert.match(file, new RegExp(`call<[^>]+>${shape}`, "u"), `${command} is called with ${args ?? "nothing"}`);
  }
  const view = source("../src/screens/settings/app-plugins.tsx");
  assert.match(view, /pickFolder\("app_plugin"\)/u, "Add picks a folder; the bridge never reads plugin files");
  assert.match(view, /keyRef\.current\.value = ""/u, "the key field is cleared once its value is sent");
});

test("Settings lists App plugins under App next to the other app pages and keeps Plugins and skills as it was", async () => {
  const settings = await import("../src/screens/settings/index.tsx");
  const app = settings.SETTINGS_GROUPS.find((group) => group.title === "App");
  assert.ok(app.items.some((item) => item.id === "app-plugins" && item.label === "App plugins"));
  const index = source("../src/screens/settings/index.tsx");
  assert.match(index, /tab === "app-plugins" \? <AppPluginsSettings \/>/u);
  assert.match(index, /tab === "plugins" \? <PluginsAndSkills skill=\{skill\} \/>/u);
});
