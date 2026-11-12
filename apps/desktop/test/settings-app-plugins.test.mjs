import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-app-plugins.ts");
const screen = await import("../src/screens/settings/app-plugins.tsx");

const SHA = "5e".repeat(32);
const LOCAL = { kind: "local", runtime: "static-retrieval-mrl-en-v1", download_bytes: 125729604 };
const HTTPS = { kind: "http", url: "https://api.openai.com/v1/embeddings", hosts: ["api.openai.com"], local_unencrypted: false, dimensions: 256 };
const LOOPBACK = { kind: "http", url: "http://127.0.0.1:11434/api/embed", hosts: ["127.0.0.1"], local_unencrypted: true, dimensions: 768 };
const PROBLEM = { code: "app_plugin_url_not_https", line: 7, field: "http.url", message: "The address must start with https://." };
const plugin = (patch = {}) => ({
  name: "local", title: "Built-in model", version: "1", description: "Runs on this computer.", kind: "embedding", origin: "bundled",
  status: { kind: "ready" }, enabled: true, runs: LOCAL, secrets: [], sha256: SHA, ...patch,
});
const trust = (patch = {}) => ({
  name: "harbor-embed", title: "Harbor embeddings", version: "2", description: "Embeds with the harbor API.", kind: "embedding",
  runs: HTTPS, secrets: ["api_key"], sha256: SHA, replaces: false, ...patch,
});
const PLUGINS = [
  plugin(),
  plugin({ name: "openai", title: "OpenAI embeddings", runs: HTTPS, enabled: false, status: { kind: "needs_key" }, secrets: [{ name: "api_key", binding: { kind: "none" } }] }),
  plugin({ name: "harbor-embed", title: "Harbor embeddings", origin: "user", runs: HTTPS, status: { kind: "ready" }, enabled: true,
    secrets: [{ name: "api_key", binding: { kind: "account", account: "acct-work", name: "Work OpenAI" } }, { name: "org", binding: { kind: "key" } }] }),
  plugin({ name: "dock-embed", title: "Dock embeddings", origin: "user", runs: LOOPBACK, status: { kind: "needs_review" }, enabled: true }),
  plugin({ name: "broken", title: "broken", origin: "user", runs: null, status: { kind: "refused", problem: PROBLEM }, enabled: false }),
  plugin({ name: "spare", title: "Spare embeddings", origin: "user", runs: LOOPBACK, status: { kind: "ready" }, enabled: false }),
];
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const actions = new Proxy({}, { get: () => forbidden });
const render = (patch = {}) => renderToStaticMarkup(createElement(screen.AppPluginsView, {
  listing: { kind: "ok", value: { plugins: PLUGINS, more: false } }, dialog: { kind: "none" }, accounts: [], busy: false, failure: "", actions, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

test("App plugins (A1): every status reads as one label, Refused before Needs review before Needs a key before Enabled or Off", () => {
  const word = (patch) => api.pluginState(plugin(patch)).word;
  assert.equal(word({ status: { kind: "refused", problem: PROBLEM }, enabled: true }), "Refused");
  assert.equal(word({ status: { kind: "needs_review" }, enabled: true }), "Needs review");
  assert.equal(word({ status: { kind: "needs_key" }, enabled: false }), "Needs a key");
  assert.equal(word({ status: { kind: "ready" }, enabled: true }), "Enabled");
  assert.equal(word({ status: { kind: "ready" }, enabled: false }), "Off");
  assert.match(api.pluginState(plugin({ status: { kind: "refused", problem: PROBLEM } })).detail, /The address must start with https:\/\/\. \(app_plugin_url_not_https, line 7, http\.url\)/);
  const shown = text(render());
  for (const words of ["Built-in model", "Enabled", "OpenAI embeddings", "Needs a key", "Dock embeddings", "Needs review", "Refused", "app_plugin_url_not_https", "Spare embeddings", "Off"]) {
    assert.ok(shown.includes(words), `the list shows ${words}`);
  }
  assert.ok(shown.includes("Built in") && shown.includes("Added by you"), "each row says where the plugin came from");
});

test("App plugins (A6, A7): a usable plugin has its own switch; a refused or unreviewed one is off with the reason", () => {
  const markup = render();
  const switchOf = (title) => markup.match(new RegExp(`<button id="app-plugin-${title}"[^>]*>`, "u"))?.[0] ?? "";
  assert.match(switchOf("local"), /aria-checked="true"/u);
  assert.match(switchOf("spare"), /aria-checked="false"/u);
  assert.doesNotMatch(switchOf("spare"), /aria-disabled/u);
  assert.match(switchOf("dock-embed"), /aria-disabled="true"/u);
  assert.match(switchOf("broken"), /aria-disabled="true"/u);
  assert.ok(text(markup).includes("Review this plugin before it runs."), "the unreviewed switch says why");
});

test("App plugins (A8, A9): only the owner's plugins have Remove, and an unreviewed one offers Review", () => {
  const markup = render();
  const removes = [...markup.matchAll(/aria-label="Remove ([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(removes, ["Harbor embeddings", "Dock embeddings", "broken", "Spare embeddings"]);
  const reviews = [...markup.matchAll(/aria-label="Review ([^"]+)"/gu)].map((match) => match[1]);
  assert.deepEqual(reviews, ["Dock embeddings"]);
});

test("App plugins (A10, A11, A12): each secret says where its key comes from and never shows a key", () => {
  const shown = text(render());
  assert.ok(shown.includes("api_key: Not set"), "an unbound secret");
  assert.ok(shown.includes("api_key: Key from Work OpenAI"), "a secret bound to an account names the account");
  assert.ok(shown.includes("org: Key you entered"), "an entered key is named, never shown");
  const markup = render();
  assert.match(markup, /aria-label="Set key for api_key of OpenAI embeddings"/u);
  assert.match(markup, /aria-label="Unbind api_key of Harbor embeddings"/u);
  assert.match(markup, /aria-label="Unbind org of Harbor embeddings"/u);
});

test("App plugins (A2, A3, A4): Add opens a paste box with Pick a .yaml file, Check and Cancel", () => {
  const empty = render({ dialog: { kind: "add", yaml: "", checking: false } });
  assert.ok(text(empty).includes("Add an app plugin"));
  assert.match(empty, /<label for="app-plugin-yaml">Plugin manifest \(YAML\)<\/label>/u);
  for (const label of ["Pick a .yaml file", "Check", "Cancel"]) assert.ok(text(empty).includes(label), `the dialog offers ${label}`);
  assert.match(empty, /<button[^>]*disabled=""[^>]*>[^<]*(<[^>]*>)*Check/u, "Check waits for text");
  const typed = render({ dialog: { kind: "add", yaml: "schema: 1\n", checking: false } });
  assert.doesNotMatch(typed, /<button[^>]*disabled=""[^>]*>[^<]*(<[^>]*>)*Check/u);
});

test("App plugins (A2, A13): the trust screen names kind, hosts, secrets, how it runs and the SHA-256, with Confirm and Cancel", () => {
  const https = text(render({ dialog: { kind: "trust", yaml: "schema: 1\n", trust: trust(), review: false } }));
  for (const words of ["Trust Harbor embeddings?", "Embedding", "Version 2", "Embeds with the harbor API.", "Reaches api.openai.com", "https://api.openai.com/v1/embeddings", "Secrets it needs: api_key", `SHA-256 ${SHA}`, "Confirm", "Cancel"]) {
    assert.ok(https.includes(words), `the trust screen shows ${words}`);
  }
  assert.ok(!https.includes("local, not encrypted"), "an https plugin is not called unencrypted");
  const loopback = text(render({ dialog: { kind: "trust", yaml: "schema: 1\n", trust: trust({ runs: LOOPBACK, secrets: [] }), review: false } }));
  assert.ok(loopback.includes("Reaches 127.0.0.1 (local, not encrypted)"), "plain http to this computer says so");
  assert.ok(loopback.includes("Secrets it needs: none"));
  const local = text(render({ dialog: { kind: "trust", yaml: "schema: 1\n", trust: trust({ runs: LOCAL, secrets: [] }), review: false } }));
  assert.ok(local.includes("Runs on this computer with the built-in model, a 126 MB download"), "the local runtime names its download");
});

test("App plugins (A14, A9): a manifest naming the owner's plugin says it replaces it, and a review uses the review title", () => {
  assert.ok(text(render({ dialog: { kind: "trust", yaml: "x", trust: trust({ replaces: true }), review: false } })).includes("Replaces your plugin named harbor-embed."));
  assert.ok(!text(render({ dialog: { kind: "trust", yaml: "x", trust: trust(), review: false } })).includes("Replaces"));
  const review = text(render({ dialog: { kind: "trust", yaml: null, trust: trust({ name: "dock-embed", title: "Dock embeddings", runs: LOOPBACK, secrets: [] }), review: true } }));
  assert.ok(review.includes("Review Dock embeddings"));
  assert.ok(review.includes("Confirm"));
});

test("App plugins (A5): a refused manifest shows its sentence, code, line and field, and no Confirm", () => {
  const shown = text(render({ dialog: { kind: "refused", yaml: "schema: 1\n", problem: PROBLEM } }));
  assert.ok(shown.includes("This manifest can't be added"));
  assert.ok(shown.includes("The address must start with https://. (app_plugin_url_not_https, line 7, http.url)"));
  assert.ok(shown.includes("Edit manifest") && shown.includes("Cancel"));
  assert.ok(!shown.includes("Confirm"), "a refused manifest cannot be confirmed");
  assert.equal(api.problemText({ code: "app_plugin_too_large", line: null, field: null, message: "The file is too large." }), "The file is too large. (app_plugin_too_large)");
});

test("App plugins (A10, A11): the key dialog lists only API-key accounts and a masked key field that holds no value", () => {
  const accounts = [
    { id: "acct-work", name: "Work OpenAI", method: "api_key" },
    { id: "acct-sub", name: "Claude Max", method: "subscription" },
    { id: "acct-local", name: "Local server", method: "url" },
  ];
  assert.deepEqual(api.keyAccounts(accounts).map((account) => account.id), ["acct-work"]);
  const markup = render({ dialog: { kind: "bind", plugin: "openai", title: "OpenAI embeddings", secret: "api_key", choice: "key" }, accounts: api.keyAccounts(accounts) });
  assert.ok(text(markup).includes("Key for api_key"));
  assert.ok(text(markup).includes("Key from Work OpenAI"));
  assert.ok(!text(markup).includes("Claude Max"), "a subscription account has no key to share");
  assert.ok(text(markup).includes("Enter a key for this plugin"));
  const field = markup.match(/<input id="app-plugin-key"[^>]*>/u)?.[0] ?? "";
  assert.match(field, /type="password"/u);
  assert.match(field, /autoComplete="off"|autocomplete="off"/u);
  assert.doesNotMatch(field, /value=/u, "React never holds the key");
  assert.ok(text(markup).includes("Sent to the engine once and never shown again."));
});

test("App plugins (A15): a listing with more plugins than it carries says so", () => {
  assert.ok(text(render({ listing: { kind: "ok", value: { plugins: PLUGINS.slice(0, 2), more: true } } })).includes("Showing the first 2 plugins."));
  assert.ok(!text(render()).includes("Showing the first"));
});

test("App plugins: loading, not ready and a refused read each keep the title", () => {
  assert.ok(text(render({ listing: { kind: "loading" } })).includes("App plugins"));
  assert.ok(text(render({ listing: { kind: "not_ready" } })).includes("isn't ready"));
  const refused = text(render({ listing: { kind: "error", code: "app_plugins_unavailable", message: "x" } }));
  assert.ok(refused.includes("App plugins") && refused.includes("Try again"));
});

test("App plugins: every app_plugin and search code the engine can answer has its own sentence", () => {
  const engine = source("../../../crates/crazyagent/src/engine/owner/transport/app_plugins.rs");
  const block = engine.match(/pub const APP_PLUGINS_CODES: &\[&str\] = &\[([\s\S]*?)\];/u)?.[1] ?? "";
  const codes = [...block.matchAll(/"((?:app_plugins?|search)_[a-z_]+)"/gu)].map((match) => match[1]);
  for (const code of ["search_confirm_required", "search_model_confirm", "search_model_downloading", "search_choice_unavailable"]) assert.ok(codes.includes(code), `${code} is read from the engine`);
  assert.ok(codes.length >= 10, `the engine's codes were read: ${codes.length}`);
  for (const code of codes) assert.ok(Object.hasOwn(api.APP_PLUGIN_REFUSALS, code), `${code} has a sentence`);
  assert.match(api.appPluginRefusal({ code: "app_plugin_name_taken" }), /^[A-Z]/u);
});

test("App plugins: each action is a direct bridge call, and an entered key travels only in its own call", () => {
  const file = source("../src/api-app-plugins.ts");
  for (const [command, args] of [
    ["desktop_app_plugins", null], ["desktop_app_plugin_preview", "{ yaml }"], ["desktop_app_plugin_review", "{ name }"],
    ["desktop_app_plugin_add", "{ yaml, sha256, replace }"], ["desktop_app_plugin_trust", "{ name, sha256 }"],
    ["desktop_app_plugin_set_enabled", "{ name, enabled }"], ["desktop_app_plugin_remove", "{ name }"],
    ["desktop_app_plugin_bind_account", "{ name, secret, account }"], ["desktop_app_plugin_set_key", "{ name, secret, key }"],
    ["desktop_app_plugin_unbind", "{ name, secret }"], ["desktop_pick_app_plugin_file", null],
  ]) {
    const shape = args === null ? `\\("${command}"\\)` : `\\("${command}", ${args.replace(/[{}]/gu, "\\$&")}\\)`;
    assert.match(file, new RegExp(`call<[^>]+>${shape}`, "u"), `${command} is called with ${args ?? "nothing"}`);
  }
  assert.doesNotMatch(file, /\bchange\(/u, "no app plugin action is a retained owner intent");
  const view = source("../src/screens/settings/app-plugins.tsx");
  assert.match(view, /SecretField/u, "the key field is the uncontrolled masked field");
  assert.match(view, /keyRef\.current\.value = ""/u, "the key field is cleared once its value is sent");
});

test("Settings lists App plugins and Search under App and opens each tab's own screen", async () => {
  const settings = await import("../src/screens/settings/index.tsx");
  const app = settings.SETTINGS_GROUPS.find((group) => group.title === "App");
  assert.deepEqual(app.items, [{ id: "sandbox", label: "Sandbox" }, { id: "alerts", label: "Team alerts" }, { id: "app-plugins", label: "App plugins" }, { id: "search", label: "Search" }]);
  const index = source("../src/screens/settings/index.tsx");
  assert.match(index, /tab === "app-plugins" \? <AppPluginsSettings \/>/u);
  assert.match(index, /tab === "search" \? <SearchSettings \/>/u);
  assert.match(index, /tab === "plugins" \? <PluginsAndSkills skill=\{skill\} \/>/u, "Plugins and skills stays as it was");
});
