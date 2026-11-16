import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const api = await import("../src/api-connectors.ts");
const screen = await import("../src/screens/settings/connector-activity.tsx");

const NOV_15_1204_UTC = 1_794_744_240_000;
const MINUTE = 60_000;
const WRITES = "This tool can change things at the vendor; writes arrive with the approval step.";
const SETTINGS = { write: "ask_every_time", wait: "minutes10", native: "off", size_cap: "kib256", keep_log: "days30" };
const call = (patch = {}) => ({
  connector: "board", tool: "list_issues", kind: "read", project: "alpha", project_name: "Plant shop", agent: "Agent 1",
  started_ms: NOV_15_1204_UTC, ms: 340, bytes_in: 120, bytes_out: 2048, outcome: "ok", outcome_text: "OK", ...patch,
});
const CALLS = [
  call({ arguments: { note: "SECRET-ARG-TEXT" }, result: "RESULT-TEXT-123" }),
  call({ tool: "create_issue", kind: "write", agent: "Master", started_ms: NOV_15_1204_UTC - MINUTE, ms: 0, bytes_in: 64, bytes_out: 0, outcome: "connector_writes_not_yet", outcome_text: WRITES }),
  call({ connector: "desk", tool: "", kind: "list", project: "beta", project_name: "beta", agent: "Agent 2", started_ms: NOV_15_1204_UTC - 6 * MINUTE, ms: 1210, bytes_in: 0, bytes_out: 5400 }),
];
const VIEW = {
  calls: CALLS, more: false, connector: null, project: null, connectors: ["board", "desk"],
  projects: [{ id: "beta", name: "beta" }, { id: "alpha", name: "Plant shop" }], settings: SETTINGS, version: 3,
};
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const render = (patch = {}) => renderToStaticMarkup(createElement(screen.ConnectorActivityView, {
  activity: { kind: "ok", value: VIEW }, keep: null, connector: null, project: null, saving: false, problem: "",
  onPickKeep: forbidden, onFilter: forbidden, onSave: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&quot;/gu, "\"").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");
const saveDisabled = (markup) => /<button type="submit"[^>]*disabled=""/u.test(markup);
const source = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const TUI_PAGE = "../../../crates/crazyagent/src/tui/screens/settings/connector_activity.rs";

test("Connector activity shows each call with every field, the keep setting beside the list, and never a call's arguments or result", () => {
  const markup = render();
  const shown = text(markup);
  for (const words of [screen.ACTIVITY_TITLE, screen.ACTIVITY_PURPOSE, "Activity log keeps", "7 days", "30 days", "90 days", "Calls 3"]) {
    assert.ok(shown.includes(words), `the page shows ${words}`);
  }
  for (const [at, head, detail] of [
    [NOV_15_1204_UTC, "board list_issues Read OK", "Project: Plant shop Agent: Agent 1 120 B in, 2.0 KiB out 340 ms"],
    [NOV_15_1204_UTC - MINUTE, `board create_issue Write ${WRITES}`, "Project: Plant shop Agent: Master 64 B in, 0 B out 0 ms"],
    [NOV_15_1204_UTC - 6 * MINUTE, "desk Tool list OK", "Project: beta Agent: Agent 2 0 B in, 5.3 KiB out 1210 ms"],
  ]) {
    assert.ok(shown.includes(`${api.stampWords(at)} ${head}`), `a call reads ${head}`);
    assert.ok(shown.includes(detail), `a call reads ${detail}`);
  }
  assert.match(markup, /value="days30" checked=""/u, "the stored keep time is the one picked");
  assert.ok(saveDisabled(markup), "Save changes waits for a change");
  for (const hidden of ["SECRET-ARG-TEXT", "RESULT-TEXT-123", "arguments"]) assert.ok(!markup.includes(hidden), `the page never shows ${hidden}`);
});

test("Connector activity: a keep pick enables Save changes, a save in flight locks it, and a problem shows as an alert", () => {
  const picked = render({ keep: "days7" });
  assert.match(picked, /value="days7" checked=""/u);
  assert.ok(!saveDisabled(picked));
  assert.ok(text(picked).includes("changed"));
  assert.ok(saveDisabled(render({ keep: "days7", saving: true })));
  assert.ok(text(render({ problem: "The connector settings were not saved. The engine could not be reached." })).includes("The connector settings were not saved."));
});

test("Connector activity: the filters list All, each connector and each project by name, and keep a filter the log no longer offers", () => {
  const markup = render({ connector: "board", project: "alpha" });
  assert.match(markup, /<option value="board" selected="">board<\/option>/u);
  assert.match(markup, /<option value="alpha" selected="">Plant shop<\/option>/u);
  assert.match(markup, /<option value="">All<\/option>/u);
  assert.match(markup, /<option value="desk">desk<\/option>/u);
  const gone = render({ connector: "old-board", project: "gone" });
  assert.match(gone, /<option value="old-board" selected="">old-board<\/option>/u);
  assert.match(gone, /<option value="gone" selected="">gone<\/option>/u);
});

test("Connector activity: an empty log, a filter with no match, more than the view holds, loading and a failed read each say so", () => {
  const empty = { ...VIEW, calls: [], connectors: [], projects: [] };
  assert.ok(text(render({ activity: { kind: "ok", value: empty } })).includes("No connector calls yet."));
  assert.ok(text(render({ activity: { kind: "ok", value: { ...empty, connector: "board" } }, connector: "board" })).includes("No calls match these filters."));
  assert.ok(text(render({ activity: { kind: "ok", value: { ...VIEW, more: true } } })).includes("Showing the newest calls. Filter by connector or project to see others."));
  assert.ok(text(render({ activity: { kind: "loading" } })).includes(screen.ACTIVITY_TITLE));
  const failed = text(render({ activity: { kind: "error", message: "The engine could not be reached." }, onRetry: () => {} }));
  assert.ok(failed.includes("Couldn't load the connector activity. The engine could not be reached."));
});

test("Connector activity: times, sizes and kinds read the same as the TUI's", () => {
  const at = new Date(NOV_15_1204_UTC);
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const two = (value) => String(value).padStart(2, "0");
  assert.equal(api.stampWords(NOV_15_1204_UTC), `${months[at.getMonth()]} ${at.getDate()} ${two(at.getHours())}:${two(at.getMinutes())}`);
  assert.equal(api.sizeWords(120, 2048), "120 B in, 2.0 KiB out");
  assert.equal(api.sizeWords(0, 1023), "0 B in, 1023 B out");
  assert.equal(api.sizeWords(1536, 5400), "1.5 KiB in, 5.3 KiB out");
  assert.equal(api.sizeWords(1_048_576, 1_101_005), "1.0 MiB in, 1.1 MiB out");
  assert.equal(api.sizeWords(-4, 1_048_575), "0 B in, 1024.0 KiB out");
  assert.deepEqual(api.KIND_WORDS, { read: "Read", write: "Write", list: "Tool list", unknown: "Unknown tool" });
  assert.deepEqual(api.KEEP_CHOICES.map((option) => option.choice), ["days7", "days30", "days90"]);
  const tui = source(TUI_PAGE);
  const words = [screen.ACTIVITY_TITLE, screen.ACTIVITY_PURPOSE, "Activity log keeps", "Connector", "Project", "All", "Calls",
    "No connector calls yet.", "No calls match these filters.", "Showing the newest calls. Filter by connector or project to see others.",
    "Connector settings saved.", "The connector settings changed elsewhere. Check them and try again.", "The connector settings were not saved.",
    "Couldn't load the connector activity.", ...Object.values(api.KIND_WORDS), ...api.KEEP_CHOICES.map((option) => option.label)];
  for (const word of words) assert.ok(tui.includes(`"${word}"`), `the TUI page uses the words ${word}`);
});

test("Connector activity: a refusal reads as the engine's own sentence", () => {
  const engine = source("../../../crates/crazyagent/src/connectors.rs");
  for (const [code, sentence] of Object.entries(api.CONNECTOR_REFUSALS)) {
    const plain = engine.match(new RegExp(`"${code}",\\s*"([^"]+)"`, "u"));
    assert.ok(plain, `the engine has a sentence for ${code}`);
    assert.equal(sentence, plain[1][0].toUpperCase() + plain[1].slice(1), code);
    assert.equal(api.connectorProblem({ code }), sentence);
  }
  assert.ok(Object.keys(api.CONNECTOR_REFUSALS).includes("connectors_unavailable"));
  assert.ok(Object.keys(api.CONNECTOR_REFUSALS).includes("connector_request_invalid"));
});

test("Settings lists Connector activity under App after App plugins and opens its own screen through its two commands, built and granted to the main window", () => {
  const index = source("../src/screens/settings/index.tsx");
  assert.match(index, /\{ id: "app-plugins", label: "App plugins" \}, \{ id: "connector-activity", label: "Connector activity" \}/u);
  assert.match(index, /tab === "connector-activity" \? <ConnectorActivity \/>/u);
  assert.match(source("../src/ui/router.ts"), /"app-plugins", "connector-activity", "search"/u);
  const apiSource = source("../src/api-connectors.ts");
  assert.match(apiSource, /[cC]all<ActivityView>\("desktop_connector_activity", \{ connector, project \}\)/u);
  assert.match(apiSource, /[cC]all<ConnectorSettingsSave>\("desktop_connector_settings_set", \{ settings, version \}\)/u);
  const lib = source("../src-tauri/src/lib.rs");
  const built = source("../src-tauri/build.rs");
  const granted = JSON.parse(source("../src-tauri/capabilities/main-window.json")).permissions;
  for (const command of ["desktop_connector_activity", "desktop_connector_settings_set"]) {
    assert.match(lib, new RegExp(`async fn ${command}\\(`, "u"), command);
    assert.ok(lib.split(command).length >= 3, `${command} is registered with the window`);
    assert.ok(built.includes(`"${command}",`), `${command} is in the app's built commands`);
    assert.ok(granted.includes(`allow-${command.replaceAll("_", "-")}`), `${command} is granted to the main window`);
  }
});
