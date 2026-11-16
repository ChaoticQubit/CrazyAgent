import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-connectors.ts");
const section = await import("../src/screens/project/connectors.tsx");
const projectSettings = await import("../src/screens/project/settings.tsx");
const settingsPage = await import("../src/screens/settings/connectors.tsx");
const router = await import("../src/ui/router.ts");

const SETTINGS = { write: "ask_every_time", wait: "minutes10", native: "off", size_cap: "kib256", keep_log: "days30" };
const HASH = "c0ffee".repeat(10) + "c0ff";
const connector = (name, title, status, patch = {}) => ({
  name, title, version: "1.0.0", description: `${title} through its own MCP server.`, origin: "bundled", status, sign_in: "key", oauth: null,
  runs: { kind: "connector", url: `https://mcp.${name}.example/mcp`, hosts: [`mcp.${name}.example`], oauth_hosts: [], vendor_read_only: true },
  secrets: [{ name: "api_key", binding: { kind: "key" } }], sha256: HASH, ...patch,
});
const VIEW = {
  connectors: [
    connector("linear", "Linear", { kind: "ready" }),
    connector("github", "GitHub", { kind: "ready" }),
    connector("atlassian", "Atlassian", { kind: "needs_key" }, { secrets: [{ name: "api_key", binding: { kind: "none" } }] }),
    connector("board", "Board", { kind: "needs_review" }, { origin: "user" }),
    connector("desk", "Desk", { kind: "ready" }, { sign_in: "not_yet", secrets: [] }),
  ],
  more: false, settings: SETTINGS, version: 2, project: "alpha",
  grants: [
    { connector: "atlassian", enabled: true, who: "both" },
    { connector: "github", enabled: true, who: "master" },
    { connector: "gone", enabled: true, who: "agents" },
    { connector: "linear", enabled: false, who: "agents" },
  ],
};
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const render = (patch = {}) => renderToStaticMarkup(createElement(section.ProjectConnectorsView, {
  view: { kind: "ok", value: VIEW }, writes: { kind: "ok", value: { project: "alpha", connectors: [] } }, busy: null, revoking: null, problem: "",
  onChange: forbidden, onRevoke: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ").trim();
const row = (markup, name) => markup.match(new RegExp(`<li data-connector="${name}"[^>]*>.*?</li>`, "su"))?.[0] ?? null;
const switchOf = (markup, name) => markup.match(new RegExp(`<button id="pc-on-${name}"[^>]*>`, "u"))?.[0] ?? null;
const checked = (markup, label) => markup.match(new RegExp(`<button[^>]*role="radio"[^>]*aria-checked="(true|false)"[^>]*>${label}</button>`, "u"))?.[1] ?? null;
const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const WHO = ["Master and agents with tools on", "Master only", "Agents only"];

test("The project's Connectors section lists each Ready connector and each one still On, with its switch and the word On or Off", () => {
  const markup = render();
  const shown = text(markup);
  assert.ok(shown.startsWith("Connectors"), shown);
  assert.ok(shown.includes("Connectors from Settings, Connectors that this project's master and agents can use. Each is off until you turn it on here."), shown);

  assert.match(switchOf(markup, "linear") ?? "", /aria-checked="false"/u, "a grant row that is off reads Off");
  assert.equal(text(row(markup, "linear") ?? ""), "Linear Off");
  assert.match(switchOf(markup, "github") ?? "", /aria-checked="true"/u);
  const github = row(markup, "github") ?? "";
  assert.ok(text(github).startsWith("GitHub On Who can use it"), text(github));
  assert.deepEqual(WHO.map((label) => checked(github, label)), ["false", "true", "false"], "Who shows the row's choice, in this order");

  const atlassian = row(markup, "atlassian") ?? "";
  assert.match(switchOf(markup, "atlassian") ?? "", /aria-checked="true"/u, "a connector that is On but not Ready can still be turned Off");
  assert.equal(text(atlassian), "Atlassian On Needs a key", "and its Who waits until it is Ready");
  const gone = row(markup, "gone") ?? "";
  assert.match(switchOf(markup, "gone") ?? "", /aria-checked="true"/u);
  assert.equal(text(gone), "gone On Not found", "an enabled grant whose connector is gone is named by the grant and can be turned Off");

  assert.equal(switchOf(markup, "board"), null, "a connector that is neither Ready nor On has no switch");
  assert.equal(switchOf(markup, "desk"), null);
  assert.equal(shown.split("Not ready yet:").length, 2, "the others are named once");
  assert.ok(shown.includes("Not ready yet: Board, Desk. Set them up in Settings, Connectors."), shown);
  for (const hidden of [HASH, "mcp.linear.example", "api_key"]) assert.ok(!markup.includes(hidden), `the section never shows ${hidden}`);
});

test("A project with no connectors at all says where to add one", () => {
  const shown = text(render({ view: { kind: "ok", value: { ...VIEW, connectors: [], grants: [] } } }));
  assert.ok(shown.includes("No connectors yet. Add one in Settings, Connectors."), shown);
  assert.ok(!shown.includes("Not ready yet"), shown);
});

test("While a change is saving its switch is unavailable, and a failed change says why", () => {
  const markup = render({ busy: "github", problem: "The connector was not changed. That project is not set up here any more." });
  assert.match(switchOf(markup, "github") ?? "", /aria-disabled="true"/u);
  assert.ok(text(row(markup, "github") ?? "").includes("Saving..."));
  assert.doesNotMatch(switchOf(markup, "linear") ?? "", /aria-disabled/u, "only the connector being changed waits");
  assert.match(markup, /role="alert"[^>]*>.*The connector was not changed\. That project is not set up here any more\./su);
});

test("The section keeps its title while loading, when the engine is not ready and when the read fails", () => {
  for (const view of [{ kind: "loading" }, { kind: "not_ready" }, { kind: "error", message: "The engine could not be reached." }]) {
    const shown = text(render({ view }));
    assert.ok(shown.startsWith("Connectors"), `${view.kind}: ${shown}`);
  }
  assert.ok(text(render({ view: { kind: "error", message: "The engine could not be reached." } })).includes("Couldn't load the connectors. The engine could not be reached."));
});

const ONCE = { ...VIEW, settings: { ...SETTINGS, write: "ask_once_per_project" } };
const WRITES = { project: "alpha", connectors: ["github", "gone"] };
const revokeOf = (markup, title) => markup.match(new RegExp(`<button[^>]*aria-label="Revoke writes for ${title}"[^>]*>Revoke</button>`, "u"))?.[0] ?? null;

test("While Write actions is Ask once per project and connector, the section lists each connector allowed to write with Revoke, after the connector rows", () => {
  const markup = render({ view: { kind: "ok", value: ONCE }, writes: { kind: "ok", value: WRITES } });
  const shown = text(markup);
  let at = shown.indexOf("Not ready yet: Board, Desk.");
  assert.ok(at >= 0, shown);
  for (const words of ["Allowed writes", "Connectors that may change things in this project without asking again.", "GitHub Revoke", "gone Revoke"]) {
    const found = shown.indexOf(words, at);
    assert.ok(found >= at, `${words} comes next: ${shown.slice(at)}`);
    at = found + words.length;
  }
  assert.ok(revokeOf(markup, "GitHub"), "Revoke names the connector it takes back");
  assert.ok(revokeOf(markup, "gone"), "a connector the page no longer lists is named by its name");
  assert.ok(!shown.includes("No connector may change things here without asking."), shown);
  assert.deepEqual(api.writeRows(ONCE, WRITES), [{ name: "github", title: "GitHub" }, { name: "gone", title: "gone" }]);
  const shadow = { ...VIEW.connectors[0], title: "linear", version: "", origin: "user", runs: null, status: { kind: "refused", problem: { code: "app_plugin_name_taken", line: null, field: "name", message: "taken" } } };
  assert.deepEqual(api.writeRows({ ...ONCE, connectors: [...ONCE.connectors, shadow] }, { project: "alpha", connectors: ["linear"] }), [{ name: "linear", title: "Linear" }],
    "a refused file sharing a name does not rename the listed connector");
  assert.equal(api.writesRevoked("GitHub"), "GitHub asks again before its next write.");
});

test("With none allowed the section says so, and under the other two Write actions there is no Allowed writes at all", () => {
  const empty = text(render({ view: { kind: "ok", value: ONCE } }));
  assert.ok(empty.includes("Allowed writes"), empty);
  assert.ok(empty.includes("No connector may change things here without asking."), empty);
  for (const write of ["ask_every_time", "read_only"]) {
    const shown = text(render({ view: { kind: "ok", value: { ...VIEW, settings: { ...SETTINGS, write } } }, writes: { kind: "ok", value: WRITES } }));
    assert.ok(!shown.includes("Allowed writes"), `${write}: ${shown}`);
    assert.ok(!shown.includes("Revoke"), `${write}: ${shown}`);
  }
});

test("While a Revoke is on its way every Revoke waits and no switch does; Allowed writes keeps its heading while loading, not ready and failed", () => {
  const markup = render({ view: { kind: "ok", value: ONCE }, writes: { kind: "ok", value: WRITES }, revoking: "github" });
  assert.match(revokeOf(markup, "GitHub") ?? "", /disabled=""/u);
  assert.match(revokeOf(markup, "gone") ?? "", /disabled=""/u, "one Revoke at a time");
  assert.doesNotMatch(switchOf(markup, "github") ?? "", /aria-disabled/u, "the connector's switch does not wait");
  for (const writes of [{ kind: "loading" }, { kind: "not_ready" }, { kind: "error", message: "The engine could not be reached." }]) {
    const shown = text(render({ view: { kind: "ok", value: ONCE }, writes }));
    assert.ok(shown.includes("Allowed writes"), `${writes.kind}: ${shown}`);
    assert.ok(!shown.includes("No connector may change things here without asking."), `${writes.kind}: ${shown}`);
  }
  assert.ok(text(render({ view: { kind: "ok", value: ONCE }, writes: { kind: "error", message: "The engine could not be reached." } })).includes("Couldn't load the allowed writes. The engine could not be reached."));
});

test("The project section reads its allowed writes, revokes one for this project and says it asks again", () => {
  const container = source("../src/screens/project/connectors.tsx");
  assert.match(container, /useLoad\(\(\) => loadAllowedWrites\(projectId\), \[projectId\]\)/u);
  assert.match(container, /revokeWrites\(projectId, connector\)/u);
  assert.match(container, /session\.showToast\(writesRevoked\(title\)\)/u);
});

test("A switch change sends the project's last Who for that connector, or both when it has none", () => {
  assert.equal(api.switchWho(VIEW, "linear"), "agents", "an Off row keeps its Who for the next On");
  assert.equal(api.switchWho(VIEW, "github"), "master");
  assert.equal(api.switchWho(VIEW, "board"), "both", "no row: the default");
});

test("Project Settings has a Connectors section right after Plugins and skills, and its route opens it", () => {
  assert.equal(projectSettings.settingsSection("connectors"), "connectors");
  assert.match(source("../src/screens/project/settings.tsx"), /\{ id: "plugins", label: "Plugins and skills" \},\s*\{ id: "connectors", label: "Connectors" \},/u);
});

const PROBLEM = { code: "app_plugin_url_not_https", line: 7, field: "connector.url", message: "The address must start with https://." };
const TAKEN = { code: "app_plugin_name_taken", line: null, field: "name", message: "a plugin with this name already exists; remove it or choose replace." };
const refusedFile = (name, problem) => ({
  name, title: name, version: "", description: "", origin: "user", status: { kind: "refused", problem }, sign_in: "not_yet", oauth: null,
  runs: null, secrets: [], sha256: HASH,
});
const CATALOG = {
  ...VIEW, project: null, grants: [],
  connectors: [...VIEW.connectors, refusedFile("broken", PROBLEM), refusedFile("linear", TAKEN)],
};
const actions = new Proxy({}, { get: () => forbidden });
const REACH = ["Harness A: yes", "Harness B: yes", "Harness C: yes (master)"];
const page = (patch = {}) => renderToStaticMarkup(createElement(settingsPage.ConnectorsSettingsView, {
  view: { kind: "ok", value: CATALOG }, reach: { kind: "ok", value: REACH }, open: null, dialog: { kind: "none" }, write: null, wait: null, native: null, sizeCap: null,
  busy: false, saving: false, failure: "", problem: "", actions, ...patch,
}));
const pageLink = (markup, name, title) => new RegExp(`<a[^>]*href="#/settings/connectors\\?connector=${name}"[^>]*>${title}</a>`, "u").test(markup);
const radios = (markup, name) => [...markup.matchAll(new RegExp(`<input type="radio" name="${name}" value="([a-z0-9_]+)"( checked="")?`, "gu"))].map((match) => [match[1], match[2] !== undefined]);
const saveButton = (markup) => markup.match(/<button[^>]*>Save changes<\/button>/u)?.[0] ?? "";
const TRUST = {
  name: "board", title: "Board", version: "2.1.0", description: "Board through its own MCP server.", kind: "connector",
  runs: { kind: "connector", url: "https://mcp.board.example/mcp", hosts: ["mcp.board.example", "api.board.example"], oauth_hosts: ["auth.board.example"], vendor_read_only: true },
  secrets: ["api_key"], sha256: HASH, replaces: true,
  connector: {
    read_only: { kind: "header", name: "X-Read-Only", value: "true" },
    key: { header: "Authorization", scheme: "Bearer", secrets: ["api_key"] },
    oauth: { hosts: ["auth.board.example"], registration: ["dynamic", "manual"], scopes_read: ["read"], scopes_write: [] },
    read_tools: ["list_items", "get_item"],
  },
};
const trustWith = (patch) => ({ ...TRUST, connector: { ...TRUST.connector, ...patch } });

test("Settings, Connectors lists every connector with its state word and line, where it came from and every host it reaches, and links each to its page", () => {
  const markup = page();
  const shown = text(markup);
  assert.ok(shown.startsWith("Connectors"), shown);
  for (const words of [api.CONNECTORS_PAGE.purpose, "Catalog", api.CONNECTORS_PAGE.catalogAbout, "Add connector"]) assert.ok(shown.includes(words), `the page shows ${words}`);
  for (const [name, title] of [["linear", "Linear"], ["github", "GitHub"], ["atlassian", "Atlassian"], ["board", "Board"], ["desk", "Desk"]]) {
    assert.ok(pageLink(markup, name, title), `${title} links to its own page`);
  }
  const rowOf = (name) => text(markup.match(new RegExp(`<li[^>]*data-connector="${name}"[^>]*>.*?</li>`, "su"))?.[0] ?? "");
  for (const words of ["linear, version 1.0.0. Built in.", "Ready", "Linear through its own MCP server.", "Reaches mcp.linear.example"]) assert.ok(rowOf("linear").includes(words), `Linear shows ${words}: ${rowOf("linear")}`);
  assert.ok(rowOf("atlassian").includes("Needs a key Set a key for each secret it needs."), rowOf("atlassian"));
  assert.ok(rowOf("board").includes("board, version 1.0.0. Added by you."), rowOf("board"));
  assert.ok(rowOf("board").includes("Needs review Review this connector before a project can use it."), rowOf("board"));
  assert.ok(rowOf("desk").includes("Unavailable Signs in with OAuth, which this build does not have."), `a connector that signs in with OAuth says so, not Ready: ${rowOf("desk")}`);
  assert.ok(rowOf("broken").includes("Refused The address must start with https://. (app_plugin_url_not_https, line 7, connector.url)"), rowOf("broken"));
  for (const hidden of [HASH, "lin_api"]) assert.ok(!markup.includes(hidden), `the catalog never shows ${hidden}`);
  assert.ok(text(page({ view: { kind: "ok", value: { ...CATALOG, more: true } } })).includes("Showing the first 7 connectors."));
});

test("A connector's state is checked in a fixed order, Refused, Needs review, Unavailable, Needs a key, Needs sign-in, Ready, and matches the project section's word", () => {
  const states = [
    [connector("a", "A", { kind: "refused", problem: PROBLEM }), { word: "Refused", tone: "danger", detail: "The address must start with https://. (app_plugin_url_not_https, line 7, connector.url)" }],
    [connector("b", "B", { kind: "needs_review" }), { word: "Needs review", tone: "warn", detail: "Review this connector before a project can use it." }],
    [connector("c", "C", { kind: "needs_key" }), { word: "Needs a key", tone: "warn", detail: "Set a key for each secret it needs." }],
    [connector("d", "D", { kind: "ready" }, { sign_in: "not_yet", secrets: [] }), { word: "Unavailable", tone: "warn", detail: "Signs in with OAuth, which this build does not have." }],
    [connector("f", "F", { kind: "needs_review" }, { sign_in: "not_yet", secrets: [] }), { word: "Needs review", tone: "warn", detail: "Review this connector before a project can use it." }],
    [connector("g", "G", { kind: "needs_sign_in" }, { sign_in: "browser", secrets: [] }), { word: "Needs sign-in", tone: "warn", detail: "Sign in on its page before a project can use it." }],
    [connector("h", "H", { kind: "needs_sign_in" }, { sign_in: "not_yet", secrets: [] }), { word: "Unavailable", tone: "warn", detail: "Signs in with OAuth, which this build does not have." }],
    [connector("e", "E", { kind: "ready" }), { word: "Ready", tone: "ok", detail: null }],
  ];
  for (const [shown, state] of states) {
    assert.deepEqual(api.connectorState(shown), state, shown.name);
    assert.equal(api.connectorWord(shown), state.word, "both pages use one word");
  }
  assert.equal(api.originLine(connector("linear", "Linear", { kind: "ready" })), "linear, version 1.0.0. Built in.");
  assert.equal(api.originLine(connector("board", "Board", { kind: "ready" }, { origin: "user" })), "board, version 1.0.0. Added by you.");
  assert.equal(api.reachesLine(["mcp.board.example", "api.board.example"]), "Reaches mcp.board.example, api.board.example");
});

test("A connector's trust screen says, as plain text and in order, everything the engine acts on", () => {
  assert.deepEqual(api.trustLines(TRUST), [
    "Version 2.1.0",
    "Address: https://mcp.board.example/mcp",
    "Reaches mcp.board.example, api.board.example",
    "Read-only header: X-Read-Only: true",
    "Key: the Authorization header with Bearer, from api_key",
    "Signs in on auth.board.example",
    "Registers by dynamic, then manual",
    "Scopes while reading: read",
    "Scopes when writing: none",
    "Tools it says only read: list_items, get_item",
    "Secrets it needs: api_key",
    `SHA-256 ${HASH}`,
  ]);
  const other = api.trustLines({ ...trustWith({ read_only: { kind: "url", url: "https://ro.board.example/mcp" }, key: { header: "X-Api-Key", scheme: "", secrets: ["api_key", "org"] }, oauth: null, read_tools: [] }), secrets: [] });
  assert.ok(other.includes("Read-only address: https://ro.board.example/mcp"));
  assert.ok(other.includes("Key: the X-Api-Key header, from api_key, org"), "an empty scheme is left out");
  assert.ok(other.includes("Tools it says only read: none"));
  assert.ok(other.includes("Secrets it needs: none"));
  assert.ok(!other.some((line) => line.startsWith("Signs in") || line.startsWith("Registers") || line.startsWith("Scopes")), "no OAuth lines when none is declared");
  const none = api.trustLines(trustWith({ read_only: { kind: "none" }, key: null }));
  assert.ok(none.includes("Read-only mode at the vendor: none"));
  assert.ok(none.includes("Key: none"));
});

test("The trust dialog shows the description, every line and the replace notice, with no link, Confirm and Cancel; a review uses the review title", () => {
  const markup = page({ dialog: { kind: "trust", yaml: "schema: 1\n", trust: TRUST, review: false } });
  const shown = text(markup);
  for (const words of ["Trust Board?", "Board through its own MCP server.", ...api.trustLines(TRUST), "Replaces your connector named board.", "Confirm", "Cancel"]) {
    assert.ok(shown.includes(words), `the trust screen shows ${words}`);
  }
  const dialog = markup.slice(markup.indexOf("Trust Board?"));
  assert.doesNotMatch(dialog, /<a /u, "nothing on the trust screen is a link");
  const review = text(page({ dialog: { kind: "trust", yaml: null, trust: { ...TRUST, replaces: false }, review: true } }));
  assert.ok(review.includes("Review Board"), review);
  assert.ok(!review.includes("Replaces"), review);
});

test("A ready preview that is not a connector's reads as refused with the engine's sentence; a connector's passes", () => {
  const notConnector = { kind: "refused", problem: { code: "app_plugin_not_connector", line: null, field: "kind", message: "this is not a connector; add it on the App plugins page." } };
  const embedding = { kind: "ready", trust: { name: "harbor-embed", title: "Harbor", version: "2", description: "", kind: "embedding", runs: { kind: "http", url: "https://api.harbor.example/v1", hosts: ["api.harbor.example"], local_unencrypted: false, dimensions: 256 }, secrets: [], sha256: HASH, replaces: false } };
  assert.deepEqual(api.connectorPreview(embedding), notConnector);
  const { connector: dropped, ...bare } = TRUST;
  assert.ok(dropped);
  assert.deepEqual(api.connectorPreview({ kind: "ready", trust: { ...bare, kind: "connector" } }), notConnector, "a connector answer with no connector block is refused");
  assert.deepEqual(api.connectorPreview({ kind: "ready", trust: TRUST }), { kind: "ready", trust: TRUST });
  assert.deepEqual(api.connectorPreview({ kind: "refused", problem: PROBLEM }), { kind: "refused", problem: PROBLEM });
});

test("Add connector opens a paste box with Pick a .yaml file, Check and Cancel; a refused manifest shows its problem and no Confirm", () => {
  const add = text(page({ dialog: { kind: "add", yaml: "" } }));
  for (const words of ["Add a connector", "Connector manifest (YAML)", api.CONNECTORS_PAGE.manifestHelp, "Pick a .yaml file", "Check", "Cancel"]) assert.ok(add.includes(words), `the paste box shows ${words}`);
  const refused = text(page({ dialog: { kind: "refused", yaml: "schema: 1\n", problem: PROBLEM } }));
  assert.ok(refused.includes("This manifest can't be added"));
  assert.ok(refused.includes("The address must start with https://. (app_plugin_url_not_https, line 7, connector.url)"));
  assert.ok(refused.includes("Edit manifest") && refused.includes("Cancel"));
  assert.ok(!refused.includes("Confirm"));
  const review = text(page({ dialog: { kind: "refused", yaml: null, problem: PROBLEM } }));
  assert.ok(review.includes("This connector can't be trusted"));
  assert.ok(!review.includes("Edit manifest"), "a listed file has no manifest to edit here");
});

test("The key dialog names where the key goes and holds a masked field with no value; Remove asks first and names what goes", () => {
  assert.equal(api.keyLine("Linear", ["mcp.linear.example"]), "Linear sends this key to mcp.linear.example. Only the engine keeps it.");
  const markup = page({ open: "atlassian", dialog: { kind: "key", connector: "atlassian", title: "Atlassian", hosts: ["mcp.atlassian.example"], secret: "api_key" } });
  const shown = text(markup);
  for (const words of ["Key for api_key", "Atlassian sends this key to mcp.atlassian.example. Only the engine keeps it.", "Sent to the engine once and never shown again.", "Save", "Cancel"]) assert.ok(shown.includes(words), `the key dialog shows ${words}`);
  const field = markup.match(/<input[^>]*id="connector-key"[^>]*>/u)?.[0] ?? "";
  assert.match(field, /type="password"/u);
  assert.doesNotMatch(field, /value=/u, "React never holds the key");
  const remove = text(page({ open: "board", dialog: { kind: "remove", connector: "board", title: "Board" } }));
  assert.ok(remove.includes("Remove Board?"), remove);
  assert.ok(remove.includes(api.CONNECTORS_PAGE.removeBody), remove);
});

test("Connector settings: Write actions, Approval wait, Harness-native connectors and Tool result size cap list every option in order, mark a change from the default, and Save changes waits for one", () => {
  assert.deepEqual(api.WRITE_CHOICES, [
    { choice: "ask_every_time", label: "Ask every time" }, { choice: "ask_once_per_project", label: "Ask once per project and connector" }, { choice: "read_only", label: "Read-only" },
  ]);
  assert.deepEqual(api.WAIT_CHOICES, [{ choice: "minutes2", label: "2 min" }, { choice: "minutes10", label: "10 min" }, { choice: "minutes30", label: "30 min" }]);
  assert.deepEqual(api.NATIVE_CHOICES, [{ choice: "off", label: "Off" }, { choice: "on", label: "On" }]);
  assert.deepEqual(api.SIZE_CHOICES, [{ choice: "kib64", label: "64 KiB" }, { choice: "kib256", label: "256 KiB" }, { choice: "mib1", label: "1 MiB" }]);
  assert.deepEqual(api.CONNECTOR_DEFAULTS, SETTINGS);
  const read = page();
  const shown = text(read);
  const writeHelp = "What happens when an agent calls a connector tool that changes something: each call waits for your Allow, your first Allow in a project lets later calls run, or those tools stay hidden.";
  const waitHelp = "How long a call that changes something waits for your answer before it fails.";
  let at = shown.indexOf("Connector settings");
  assert.ok(at >= 0, shown);
  for (const words of ["Write actions", writeHelp, "Approval wait", waitHelp, "Harness-native connectors", api.CONNECTORS_PAGE.nativeHelp, "Tool result size cap", api.CONNECTORS_PAGE.sizeCapHelp,
    "Tool list check and third-party label", api.CONNECTORS_PAGE.lockedReason]) {
    const found = shown.indexOf(words, at);
    assert.ok(found >= at, `the settings show ${words} next: ${shown.slice(at)}`);
    at = found + words.length;
  }
  const locked = read.match(/<div class="set-row"[^>]*data-setting="Tool list check and third-party label"[^>]*>.*?<\/div><\/div>/su)?.[0] ?? "";
  assert.ok(text(locked).includes("Always on"), `the locked line says Always on: ${locked}`);
  assert.doesNotMatch(locked, /<input|<button/u, "the locked line has no control");
  assert.deepEqual(radios(read, "connector-write"), [["ask_every_time", true], ["ask_once_per_project", false], ["read_only", false]]);
  assert.deepEqual(radios(read, "connector-wait"), [["minutes2", false], ["minutes10", true], ["minutes30", false]]);
  assert.deepEqual(radios(read, "connector-native"), [["off", true], ["on", false]]);
  assert.deepEqual(radios(read, "connector-size-cap"), [["kib64", false], ["kib256", true], ["mib1", false]]);
  const marks = (markup) => (markup.match(/class="set-changed"/gu) ?? []).length;
  assert.equal(marks(read), 0, "the defaults are not marked");
  assert.match(saveButton(read), /disabled=""/u, "nothing to save yet");
  const picked = page({ write: "read_only", wait: "minutes2", native: "on", sizeCap: "mib1" });
  assert.deepEqual(radios(picked, "connector-write"), [["ask_every_time", false], ["ask_once_per_project", false], ["read_only", true]]);
  assert.deepEqual(radios(picked, "connector-wait"), [["minutes2", true], ["minutes10", false], ["minutes30", false]]);
  assert.deepEqual(radios(picked, "connector-native"), [["off", false], ["on", true]]);
  assert.deepEqual(radios(picked, "connector-size-cap"), [["kib64", false], ["kib256", false], ["mib1", true]]);
  assert.equal(marks(picked), 4, "each setting away from its default is marked");
  assert.doesNotMatch(saveButton(picked), /disabled=""/u);
  for (const [patch, what] of [[{ write: "ask_once_per_project" }, "Write actions"], [{ wait: "minutes30" }, "Approval wait"]]) {
    const one = page(patch);
    assert.doesNotMatch(saveButton(one), /disabled=""/u, `a ${what} pick alone can be saved`);
    assert.equal(marks(one), 1, `${what} is marked`);
  }
  assert.match(saveButton(page({ write: "ask_every_time", wait: "minutes10", native: "off", sizeCap: "kib256" })), /disabled=""/u, "picks equal to the settings read are no change");
  assert.match(page({ native: "on", problem: api.CONNECTORS_PAGE.stale }), /role="alert"[^>]*>.*The connector settings changed elsewhere\. Check them and try again\./su);
  const saving = page({ write: "read_only", saving: true });
  for (const setting of ["write", "wait", "native", "size_cap"]) assert.match(saving, new RegExp(`<fieldset[^>]*disabled=""[^>]*data-setting="${setting}"`, "u"), `a save in flight locks ${setting}`);
});

test("A settings save sends the settings read with only the four this page shows replaced, and nothing when no pick differs from them", () => {
  const read = { write: "read_only", wait: "minutes30", native: "off", size_cap: "kib256", keep_log: "days90" };
  const none = { write: null, wait: null, native: null, size_cap: null };
  assert.deepEqual(api.settingsToSave(read, { write: "ask_once_per_project", wait: "minutes2", native: "on", size_cap: "mib1" }),
    { write: "ask_once_per_project", wait: "minutes2", native: "on", size_cap: "mib1", keep_log: "days90" });
  for (const [key, value] of [["write", "ask_every_time"], ["wait", "minutes2"], ["native", "on"], ["size_cap", "mib1"]]) {
    assert.deepEqual(api.settingsToSave(read, { ...none, [key]: value }), { ...read, [key]: value }, `a ${key} pick alone is saved`);
  }
  assert.equal(api.settingsToSave(read, none), null, "nothing picked, nothing to save");
  assert.equal(api.settingsToSave(read, { write: "read_only", wait: "minutes30", native: "off", size_cap: "kib256" }), null, "picks equal to the read are no change");
  assert.equal(api.settingsToSave(read, { ...none, native: "on", keep_log: "days7" })?.keep_log, "days90", "the keep setting always goes back as read");
  assert.deepEqual(read, { write: "read_only", wait: "minutes30", native: "off", size_cap: "kib256", keep_log: "days90" }, "the settings read are not changed");
  const container = source("../src/screens/settings/connectors.tsx");
  assert.match(container, /const settings = settingsToSave\(read, \{ write, wait, native, size_cap: sizeCap \}\);\s*if \(settings === null\) return;/u,
    "a save sends nothing only when settingsToSave finds no change");
});

test("A connector's own page shows where it came from, its state, hosts and address, each secret with Set key or Clear key, Review and Remove, and no catalog or settings", () => {
  const linear = page({ open: "linear" });
  const shown = text(linear);
  assert.match(linear, /<a[^>]*href="#\/settings\/connectors"[^>]*>All connectors<\/a>/u);
  for (const words of ["Linear", "linear, version 1.0.0. Built in.", "Linear through its own MCP server.", "Ready", "Reaches mcp.linear.example", "Address: https://mcp.linear.example/mcp", "api_key: Key you entered", "Clear key"]) {
    assert.ok(shown.includes(words), `Linear's page shows ${words}: ${shown}`);
  }
  assert.match(linear, /aria-label="Clear key for api_key of Linear"/u);
  for (const absent of ["Catalog", "Harness-native connectors", "Remove", "Review", "Set key"]) assert.ok(!shown.includes(absent), `Linear's page has no ${absent}`);
  const atlassian = page({ open: "atlassian" });
  assert.ok(text(atlassian).includes("api_key: Not set"));
  assert.match(atlassian, /aria-label="Set key for api_key of Atlassian"/u);
  const board = page({ open: "board" });
  assert.match(board, /aria-label="Review Board"/u);
  assert.match(board, /aria-label="Remove Board"/u);
  const desk = text(page({ open: "desk" }));
  assert.ok(desk.includes("Signs in with OAuth, which this build does not have."));
  assert.ok(!desk.includes("Set key") && !desk.includes("Clear key"), "no key entry for a connector that signs in with OAuth");
  const ghost = page({ open: "ghost" });
  assert.ok(text(ghost).includes("No connector named ghost."));
  assert.match(ghost, /href="#\/settings\/connectors"/u);
  assert.ok(!linear.includes(HASH));
  assert.match(page({ open: "linear", failure: "That connector is gone. The page now shows the current ones." }), /role="alert"[^>]*>.*That connector is gone\./su);
});

test("The Connectors page keeps its title while loading, when the engine is not ready and when the read fails", () => {
  for (const view of [{ kind: "loading" }, { kind: "not_ready" }, { kind: "error", message: "The engine could not be reached." }]) {
    assert.ok(text(page({ view })).startsWith("Connectors"), view.kind);
  }
  assert.ok(text(page({ view: { kind: "error", message: "The engine could not be reached." } })).includes("Couldn't load the connectors. The engine could not be reached."));
});

test("A connector's page has its own address, which keeps only a manifest-shaped name and only on Connectors", () => {
  const open = { name: "settings", tab: "connectors", connector: "linear" };
  assert.equal(router.formatRoute(open), "#/settings/connectors?connector=linear");
  assert.deepEqual(router.parseRoute("#/settings/connectors?connector=linear"), open);
  assert.deepEqual(router.parseRoute("#/settings/connectors?connector=my-board-2"), { ...open, connector: "my-board-2" });
  for (const bad of ["Linear", "1board", "-board", "lin ear", "a".repeat(41), ""]) {
    assert.deepEqual(router.parseRoute(`#/settings/connectors?connector=${encodeURIComponent(bad)}`), { name: "settings", tab: "connectors" }, bad);
  }
  assert.deepEqual(router.parseRoute("#/settings/plugins?connector=linear"), { name: "settings", tab: "plugins" });
  assert.equal(router.formatRoute({ name: "settings", tab: "connectors" }), "#/settings/connectors");
});

test("Settings lists Connectors in Agents right after Plugins and skills and opens the connector the route names", () => {
  const index = source("../src/screens/settings/index.tsx");
  assert.match(index, /\{ id: "plugins", label: "Plugins and skills" \}, \{ id: "connectors", label: "Connectors" \},/u);
  assert.match(index, /tab === "connectors" \? <ConnectorsSettings connector=\{connector\} \/>/u);
  assert.match(source("../src/app.tsx"), /<Settings [^>]*connector=\{route\.connector\}/u);
  assert.match(source("../src/ui/router.ts"), /"plugins", "connectors", "process"/u);
});

test("Every refusal the Connectors protocol answers, past the owner codes, has a connector sentence, the same in the TUI", () => {
  const words = {
    app_plugin_bundled: "Built-in connectors can't be removed or replaced.",
    app_plugin_changed: "The manifest changed after you checked it. Check it again before you trust it.",
    app_plugin_folder_unavailable: "CrazyAgent couldn't open your plugins folder. Try again.",
    app_plugin_folder_unsafe: "Your plugins folder isn't private to you, so CrazyAgent won't read it. Make it yours alone and try again.",
    app_plugin_key_invalid: "That key can't be used: it is empty, too long or has spaces or control characters.",
    app_plugin_key_store_unavailable: "CrazyAgent couldn't save the key. Nothing was saved; try again.",
    app_plugin_name_taken: "A built-in plugin or connector already has that name. Give yours another name.",
    app_plugin_needs_key: "This connector needs a key first. Set one on its page in Settings, Connectors.",
    app_plugin_needs_review: "This connector's file changed or is new. Review it in Settings, Connectors first.",
    app_plugin_not_found: "That connector is gone. The page now shows the current ones.",
    app_plugin_refused: "CrazyAgent refused this manifest. Its reason is shown with it.",
    app_plugin_secret_not_declared: "This connector doesn't declare that secret.",
    app_plugins_unavailable: "Connectors aren't available in this engine yet.",
    connector_project_unknown: "That project is not set up here any more.",
    owner_entropy_unavailable: "This computer couldn't make a random value for the request. Nothing changed; try again.",
    connectors_unavailable: "The engine could not be reached.",
    connector_request_invalid: "The request could not be read.",
    connector_request_bounds: "The request is too large.",
    connector_outcome_unknown: "The engine did not answer in time; check the list again.",
    connector_approval_unknown: "That approval is gone.",
    connector_approval_closed: "This write was already answered or its wait ran out. The page now shows where it stands.",
    connector_sign_in_one_way: "This connector signs in one way only, so there is nothing to choose.",
  };
  for (const [code, sentence] of Object.entries(words)) {
    assert.equal(api.connectorProblem({ code, cause: code, message: "Something went wrong. Try again." }), sentence, code);
  }
  const engineWorded = Object.keys(api.CONNECTOR_REFUSALS);
  const pageWorded = Object.keys(api.CONNECTOR_PAGE_REFUSALS);
  assert.ok(engineWorded.every((code) => !pageWorded.includes(code)), "each code has one sentence");
  assert.deepEqual([...engineWorded, ...pageWorded].sort(), Object.keys(words).sort(), "no code has a sentence of its own elsewhere");
});

test("The Connectors commands, the harness rows' one included, are built, registered and granted, and each call names only its own arguments", () => {
  const lib = source("../src-tauri/src/lib.rs");
  const built = source("../src-tauri/build.rs");
  const granted = JSON.parse(source("../src-tauri/capabilities/main-window.json")).permissions;
  const calls = {
    desktop_connectors: "{ project }",
    desktop_connector_preview: "{ yaml }",
    desktop_connector_review: "{ name }",
    desktop_connector_add: "{ yaml, sha256, replace }",
    desktop_connector_trust: "{ name, sha256 }",
    desktop_connector_remove: "{ name }",
    desktop_connector_set_key: "{ name, secret, key }",
    desktop_connector_clear_key: "{ name, secret }",
    desktop_connector_grant: "{ project, connector, enabled, who }",
    desktop_connector_set_sign_in_with: "{ name, way }",
    desktop_connector_reach: "{}",
  };
  const apiSource = source("../src/api-connectors.ts");
  for (const [command, args] of Object.entries(calls)) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`, "u"), command);
    assert.ok(lib.split(command).length >= 3, `${command} is registered with the window`);
    assert.ok(built.includes(`"${command}",`), `${command} is in the app's built commands`);
    assert.ok(granted.includes(`allow-${command.replaceAll("_", "-")}`), `${command} is granted to the main window`);
    assert.ok(apiSource.includes(`("${command}", ${args})`), `${command} is called with ${args}`);
  }
  assert.equal(apiSource.split(", key }").length, 2, "only the set key call carries a key");
});

test("The key is read from the masked field, the field is cleared, and only then is it sent; closing a dialog clears it too", () => {
  const screen = source("../src/screens/settings/connectors.tsx");
  const read = screen.indexOf("const key = keyRef.current.value;");
  const cleared = screen.indexOf("keyRef.current.value = \"\";", read);
  const sent = screen.indexOf("setConnectorKey(", read);
  assert.ok(read > 0 && cleared > read && sent > cleared, "read, clear, then send");
  assert.match(screen, /function close\(\) \{\s*if \(keyRef\.current !== null\) keyRef\.current\.value = "";/u);
  assert.match(screen, /<SecretField id="connector-key"/u);
  assert.doesNotMatch(screen, /showToast\([^)]*\bkey\b[^)]*\)/u, "no toast names the key");
});

test("Settings, Connectors lists each harness row after the catalog, in the order the bridge gives them, and only on the main page", () => {
  const markup = page();
  const shown = text(markup);
  assert.ok(shown.includes(`Harnesses ${api.CONNECTORS_PAGE.harnessesAbout}`), shown);
  const rows = [...(markup.match(/<ul[^>]*aria-label="Harnesses"[^>]*>(.*?)<\/ul>/su)?.[1] ?? "").matchAll(/<li[^>]*>(.*?)<\/li>/gsu)].map((match) => text(match[1]));
  assert.deepEqual(rows, REACH);
  assert.ok(shown.indexOf("Catalog") < shown.indexOf("Harnesses"), "the harness rows come after the catalog");
  assert.ok(!text(page({ reach: { kind: "loading" } })).includes("Harnesses"), "no harness section before its rows are read");
  assert.ok(!text(page({ open: "linear" })).includes("Harnesses"), "a connector's own page has no harness rows");
});

test("No TypeScript spells a harness row: the words come only from connectors::launch::reach", () => {
  const engine = source("../../../crates/crazyagent/src/connectors/launch.rs");
  const words = [...engine.matchAll(/HarnessKind::\w+ => "([^"]+)"/gu)].map((match) => match[1]);
  assert.ok(words.length >= 3, "the engine words every harness");
  for (const file of ["../src/api-connectors.ts", "../src/screens/settings/connectors.tsx", "../src/screens/settings/connector-detail.tsx"]) {
    for (const word of words) assert.ok(!source(file).includes(word), `${file} never spells ${word}`);
  }
});

test("A refused connector file is listed under its file name with its reason and where to remove it, with no link, version or address, even beside a built-in one of its name", () => {
  const markup = page();
  const userRow = (name) => text(markup.match(new RegExp(`<li[^>]*data-connector="${name}" data-origin="user"[^>]*>.*?</li>`, "su"))?.[0] ?? "");
  assert.ok(userRow("broken").includes("broken. Added by you. Refused The address must start with https://. (app_plugin_url_not_https, line 7, connector.url)"), userRow("broken"));
  assert.ok(userRow("linear").includes("linear. Added by you. Refused linear.yaml is set aside: a built-in plugin or connector now has the name linear. The file stays in your plugins folder, and its keys and switches were cleared. Rename the file and the name in it to use it."), userRow("linear"));
  for (const name of ["broken", "linear"]) {
    assert.ok(userRow(name).includes(api.CONNECTORS_PAGE.refusedFile), `${name} says where its file can be removed: ${userRow(name)}`);
    for (const absent of ["Reaches", "version"]) assert.ok(!userRow(name).includes(absent), `${name} shows no ${absent}`);
  }
  assert.ok(!/href="#\/settings\/connectors\?connector=broken"/u.test(markup), "a refused file has no page of its own to open");
  assert.equal([...markup.matchAll(/href="#\/settings\/connectors\?connector=linear"/gu)].length, 1, "only the built-in Linear links to the page of that name");
  assert.equal(api.originLine(refusedFile("broken", PROBLEM)), "broken. Added by you.");
  const shown = text(page({ open: "broken" }));
  for (const words of ["broken. Added by you.", "Refused", "(app_plugin_url_not_https, line 7, connector.url)", api.CONNECTORS_PAGE.refusedFile]) {
    assert.ok(shown.includes(words), `broken's page shows ${words}: ${shown}`);
  }
  for (const absent of ["Reaches", "Address:", "Remove", "Review", "Set key"]) assert.ok(!shown.includes(absent), `broken's page has no ${absent}`);
});

async function bridged(reply, run) {
  const sent = [];
  const before = globalThis.window;
  globalThis.window = { __TAURI_INTERNALS__: { async invoke(command, args) {
    sent.push([command, args]);
    return reply;
  } } };
  try {
    await run(sent);
  } finally {
    globalThis.window = before;
  }
}

test("Every manifest and vendor string the Connectors pages read from the engine shows its hidden characters as escapes, and names stay as sent", async () => {
  const sneaky = "Line‮ar​\u{e0041}";
  const escaped = "Line\\u202ear\\u200b\\u{e0041}";
  const listed = connector("linear", sneaky, { kind: "ready" }, { description: `Reads ${sneaky}` });
  await bridged({ ...CATALOG, connectors: [listed] }, async () => {
    const result = await api.loadConnectors(null);
    assert.equal(result.kind, "ok");
    assert.equal(result.value.connectors[0].title, escaped);
    assert.equal(result.value.connectors[0].description, `Reads ${escaped}`);
    assert.equal(result.value.connectors[0].name, "linear", "a name is plain text already and goes back as sent");
    const markup = page({ view: result });
    for (const hidden of ["‮", "​", "\u{e0041}"]) assert.ok(!markup.includes(hidden), `the catalog never holds ${JSON.stringify(hidden)}`);
    assert.ok(text(markup).includes(escaped), "the escapes are shown");
  });
  await bridged({ kind: "ready", trust: { ...TRUST, title: sneaky, description: sneaky } }, async () => {
    const result = await api.previewConnector("schema: 1");
    assert.equal(result.kind, "ok");
    assert.equal(result.value.trust.title, escaped);
    assert.equal(result.value.trust.description, escaped);
    assert.equal(result.value.trust.sha256, HASH, "the SHA-256 the add names is the one sent");
  });
  const call = {
    connector: "linear", tool: `get_${sneaky}`, kind: "read", project: "alpha", project_name: `Alpha ${sneaky}`, agent: `agent ${sneaky}`,
    started_ms: 1, ms: 2, bytes_in: 3, bytes_out: 4, outcome: "ok", outcome_text: sneaky,
  };
  const activity = { calls: [call], more: false, connector: null, project: null, connectors: ["linear"], projects: [{ id: "alpha", name: `Alpha ${sneaky}` }], settings: SETTINGS, version: 1 };
  await bridged(activity, async () => {
    const result = await api.loadConnectorActivity(null, null);
    assert.equal(result.kind, "ok");
    const [shown] = result.value.calls;
    assert.deepEqual([shown.tool, shown.project_name, shown.agent, shown.outcome_text], [`get_${escaped}`, `Alpha ${escaped}`, `agent ${escaped}`, escaped]);
    assert.deepEqual([shown.connector, shown.project, result.value.projects[0].id], ["linear", "alpha", "alpha"]);
    assert.equal(result.value.projects[0].name, `Alpha ${escaped}`);
  });
});

test("The trust types name the same fields as the engine's TrustView, ConnectorTrust, KeyTrust, OAuthTrust and ReadOnlyTrust", () => {
  const rust = source("../../../crates/crazyagent/src/app_plugins.rs");
  const ts = source("../src/api-connectors.ts");
  const rustFields = (name) => [...(rust.match(new RegExp(`pub struct ${name} \\{\\n(.*?)\\n\\}`, "su"))?.[1] ?? "").matchAll(/^\s+pub (\w+):/gmu)].map((match) => match[1]).sort();
  const tsFields = (name) => [...(ts.match(new RegExp(`export interface ${name} \\{(.*?)\\}`, "su"))?.[1] ?? "").matchAll(/(\w+):/gu)].map((match) => match[1]).sort();
  for (const [engine, page] of [["TrustView", "ConnectorTrustView"], ["ConnectorTrust", "ConnectorTrust"], ["KeyTrust", "KeyTrust"], ["OAuthTrust", "OAuthTrust"]]) {
    assert.ok(rustFields(engine).length >= 3, `${engine} is read from the engine`);
    assert.deepEqual(tsFields(page), rustFields(engine), `${page} names ${engine}'s fields`);
  }
  const variants = [...(rust.match(/pub enum ReadOnlyTrust \{\n(.*?)\n\}/su)?.[1] ?? "").matchAll(/^\s+(\w+)(?: \{ ([^}]*) \})?,$/gmu)]
    .map((match) => [match[1].toLowerCase(), [...(match[2] ?? "").matchAll(/(\w+):/gu)].map((field) => field[1])]);
  const union = ts.match(/export type ReadOnlyTrust = (.*);$/mu)?.[1] ?? "";
  const shapes = [...union.matchAll(/\{ kind: "(\w+)"([^}]*)\}/gu)].map((match) => [match[1], [...match[2].matchAll(/(\w+):/gu)].map((field) => field[1])]);
  assert.equal(variants.length, 3, "ReadOnlyTrust is read from the engine");
  assert.deepEqual(shapes, variants);
});

test("Removing the connector whose page is open replaces that page in the history, so Back never opens a page for a connector that is gone", () => {
  const screen = source("../src/screens/settings/connectors.tsx");
  assert.match(screen, /connector === name\) navigate\(\{ name: "settings", tab: "connectors" \}, true\);/u);
});

test("A project's Connectors section has one row per name: the owner's refused file a built-in connector shadows adds no row and no not-ready name", () => {
  for (const enabled of [true, false]) {
    const view = { ...VIEW, connectors: [...VIEW.connectors, refusedFile("linear", TAKEN)], grants: [{ connector: "linear", enabled, who: "both" }] };
    const { rows, others } = api.projectRows(view);
    assert.deepEqual(rows.filter((row) => row.name === "linear").map((row) => row.title), ["Linear"], `grant ${enabled}`);
    assert.ok(!others.includes("linear"), `grant ${enabled}: ${others.join(", ")}`);
    const markup = render({ view: { kind: "ok", value: view } });
    assert.equal(markup.split("id=\"pc-on-linear\"").length, 2, `grant ${enabled}: one switch for linear`);
  }
  const broken = { ...VIEW, connectors: [...VIEW.connectors, refusedFile("broken", PROBLEM)], grants: [{ connector: "broken", enabled: true, who: "both" }] };
  assert.deepEqual(api.projectRows(broken).rows.filter((row) => row.name === "broken").map((row) => row.word), ["Refused"], "a refused file of its own name keeps its switch, so it can be turned off");
});

const both = (using) => connector("board", "Board", { kind: using === "key" ? "needs_key" : "needs_sign_in" }, {
  origin: "user", sign_in: "both", secrets: [{ name: "api_key", binding: { kind: "none" } }],
  oauth: { signed_in: false, own_app_allowed: true, own_app: null, redirect: "http://127.0.0.1/callback", using },
});

test("A connector that signs in with a key or the browser shows Sign in with as the engine reads it, its keys only under Key, and sends the choice as its name and way, in the TUI's words", async () => {
  for (const using of ["key", "browser"]) {
    const markup = page({ view: { kind: "ok", value: { ...CATALOG, connectors: [both(using)] } }, open: "board" });
    assert.ok(markup.includes("data-setting=\"sign_in_with\""), `${using}: the choice is on the page`);
    assert.deepEqual(radios(markup, "connector-sign-in-with"), [["key", using === "key"], ["browser", using === "browser"]], using);
    assert.equal(markup.includes("Set key for api_key of Board"), using === "key", `${using}: keys show only under Key`);
    assert.ok(text(markup).includes(api.CONNECTORS_PAGE.signInWithHelp), `${using}: the help says the other way stays saved`);
  }
  assert.ok(!page({ open: "linear" }).includes("data-setting=\"sign_in_with\""), "a connector with one way has nothing to choose");
  assert.deepEqual(api.SIGN_IN_WITH_CHOICES, [{ choice: "key", label: "Key" }, { choice: "browser", label: "Browser" }]);
  const tui = source("../../../crates/crazyagent/src/tui/screens/settings/connectors/words.rs");
  for (const said of [api.CONNECTORS_PAGE.signInWith, api.CONNECTORS_PAGE.signInWithHelp]) {
    assert.ok(typeof said === "string" && tui.includes(`"${said}"`), `the TUI says ${said}`);
  }
  assert.equal(typeof api.setConnectorSignInWith, "function", "the page can send the choice");
  await bridged(CATALOG, async (sent) => {
    const result = await api.setConnectorSignInWith("board", "browser");
    assert.equal(result.kind, "ok");
    assert.deepEqual(sent, [["desktop_connector_set_sign_in_with", { name: "board", way: "browser" }]]);
  });
});

test("The Connectors page types name the same fields and words as the engine's ConnectorView, OAuthView, OwnAppView, SignIn and SignInWith", () => {
  const rust = source("../../../crates/crazyagent/src/connectors.rs");
  const ts = source("../src/api-connectors.ts");
  const rustFields = (name) => [...(rust.match(new RegExp(`pub struct ${name} \\{\\n(.*?)\\n\\}`, "su"))?.[1] ?? "").matchAll(/^\s+pub (\w+):/gmu)].map((match) => match[1]).sort();
  const tsFields = (name) => [...(ts.match(new RegExp(`export interface ${name} \\{(.*?)\\}`, "su"))?.[1] ?? "").matchAll(/(\w+):/gu)].map((match) => match[1]).sort();
  for (const name of ["ConnectorView", "OAuthView", "OwnAppView"]) {
    assert.ok(rustFields(name).length >= 2, `${name} is read from the engine`);
    assert.deepEqual(tsFields(name), rustFields(name), `${name} names the engine's fields`);
  }
  const snake = (word) => word.replace(/[A-Z]/gu, (letter, at) => `${at === 0 ? "" : "_"}${letter.toLowerCase()}`);
  const variants = (name) => [...(rust.match(new RegExp(`pub enum ${name} \\{\\n(.*?)\\n\\}`, "su"))?.[1] ?? "").matchAll(/^\s+(\w+),$/gmu)].map((match) => snake(match[1]));
  const union = (name) => [...(ts.match(new RegExp(`export type ${name} = (.*);$`, "mu"))?.[1] ?? "").matchAll(/"(\w+)"/gu)].map((match) => match[1]);
  for (const name of ["SignIn", "SignInWith"]) {
    assert.ok(variants(name).length >= 2, `${name} is read from the engine`);
    assert.deepEqual(union(name), variants(name), `${name} has the engine's words`);
  }
});
