import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-connectors.ts");
const section = await import("../src/screens/project/connectors.tsx");
const projectSettings = await import("../src/screens/project/settings.tsx");

const SETTINGS = { write: "ask_every_time", wait: "minutes10", native: "off", size_cap: "kib256", keep_log: "days30" };
const HASH = "c0ffee".repeat(10) + "c0ff";
const connector = (name, title, status, patch = {}) => ({
  name, title, version: "1.0.0", description: `${title} through its own MCP server.`, origin: "bundled", status, sign_in: "key",
  runs: { kind: "connector", url: `https://mcp.${name}.example/mcp`, hosts: [`mcp.${name}.example`], oauth_hosts: [], vendor_read_only: true },
  secrets: [{ name: "api_key", binding: { kind: "key" } }], sha256: HASH, ...patch,
});
const VIEW = {
  connectors: [
    connector("linear", "Linear", { kind: "ready" }),
    connector("github", "GitHub", { kind: "ready" }),
    connector("atlassian", "Atlassian", { kind: "needs_key" }, { secrets: [{ name: "api_key", binding: { kind: "none" } }] }),
    connector("board", "Board", { kind: "needs_review" }, { origin: "user" }),
    connector("desk", "Desk", { kind: "needs_key" }, { sign_in: "not_yet", secrets: [] }),
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
  view: { kind: "ok", value: VIEW }, busy: null, problem: "", onChange: forbidden, onRetry: forbidden, ...patch,
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

test("A switch change sends the project's last Who for that connector, or both when it has none", () => {
  assert.equal(api.switchWho(VIEW, "linear"), "agents", "an Off row keeps its Who for the next On");
  assert.equal(api.switchWho(VIEW, "github"), "master");
  assert.equal(api.switchWho(VIEW, "board"), "both", "no row: the default");
});

test("Project Settings has a Connectors section right after Plugins and skills, and its route opens it", () => {
  assert.equal(projectSettings.settingsSection("connectors"), "connectors");
  assert.match(source("../src/screens/project/settings.tsx"), /\{ id: "plugins", label: "Plugins and skills" \},\s*\{ id: "connectors", label: "Connectors" \},/u);
});
