import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const coord = await import("../src/screens/team/coordination.tsx");
const SETTINGS_RS = readFileSync(new URL("../../../crates/crazyagent/src/coord/settings.rs", import.meta.url), "utf8");

const forbidden = () => assert.fail("nothing reloads while rendering");

/** A team status whose coordination holds the given settings and lane. Inputs: the settings
 * laid over the defaults and the lane. Output: the status. */
function status(settings = {}, lane = undefined, agents = []) {
  return { project_id: "p", agents, coordination: { settings: { ...coord.COORD_DEFAULTS, ...settings }, harnesses: [], ...(lane ? { lane } : {}) } };
}

/** The radio labels of one Segmented row and which are checked. Inputs: the markup and the
 * row label. Output: the labels and the checked ones. */
function radios(markup, label) {
  const start = markup.indexOf(`role="radiogroup" aria-label="${label}"`);
  assert.ok(start >= 0, `the ${label} row shows`);
  const group = markup.slice(start, markup.indexOf("</div>", start));
  const found = [...group.matchAll(/role="radio" aria-checked="(true|false)"[^>]*>([^<]+)</gu)];
  return { labels: found.map((match) => match[2]), checked: found.filter((match) => match[1] === "true").map((match) => match[2]) };
}

const WORDS = ["1 at a time", "2 at a time", "3 at a time", "4 at a time", "No limit"];

test("the heavy lane options are the engine's, in its order and words, with 1 at a time as the default (F28)", () => {
  const named = SETTINGS_RS.match(/named!\(HeavyLane \{([^}]+)\}\);/u);
  assert.ok(named, "the engine names the heavy lane options once");
  assert.deepEqual(coord.HEAVY_LANE_OPTIONS.map((option) => option.value), [...named[1].matchAll(/"([a-z_]+)"/gu)].map((match) => match[1]));
  assert.deepEqual(coord.HEAVY_LANE_OPTIONS.map((option) => option.label), WORDS);
  for (const words of WORDS) assert.ok(SETTINGS_RS.includes(`=> "${words}",`), `the TUI shows ${words}`);
  assert.equal(coord.COORD_DEFAULTS.heavy_lane, "one");
  assert.deepEqual(coord.COORD_DEFAULTS.shared_resources, []);
});

test("heavy lane 1 at a time shows checked on the Heavy lane row", () => {
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, { projectId: "p", status: status({ heavy_lane: "one" }), reload: forbidden }));
  assert.deepEqual(radios(markup, "Heavy lane"), { labels: WORDS, checked: ["1 at a time"] });
});

test("heavy lane 2 at a time shows checked on the Heavy lane row", () => {
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, { projectId: "p", status: status({ heavy_lane: "two" }), reload: forbidden }));
  assert.deepEqual(radios(markup, "Heavy lane"), { labels: WORDS, checked: ["2 at a time"] });
});

test("heavy lane 3 at a time shows checked on the Heavy lane row", () => {
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, { projectId: "p", status: status({ heavy_lane: "three" }), reload: forbidden }));
  assert.deepEqual(radios(markup, "Heavy lane"), { labels: WORDS, checked: ["3 at a time"] });
});

test("heavy lane 4 at a time shows checked on the Heavy lane row", () => {
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, { projectId: "p", status: status({ heavy_lane: "four" }), reload: forbidden }));
  assert.deepEqual(radios(markup, "Heavy lane"), { labels: WORDS, checked: ["4 at a time"] });
});

test("heavy lane No limit shows checked on the Heavy lane row", () => {
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, { projectId: "p", status: status({ heavy_lane: "no_limit" }), reload: forbidden }));
  assert.deepEqual(radios(markup, "Heavy lane"), { labels: WORDS, checked: ["No limit"] });
});

test("an engine from before the lane shows 1 at a time, no shared resources and an empty lane", () => {
  const old = { project_id: "p", agents: [], coordination: { settings: { questions_to: "lead", overlaps: "warn", nudges: "off", counted_tools: [], role_skills: { master: null, lead: null, worker: null }, terminal_display: "classic", task_start: "on" }, harnesses: [] } };
  const view = coord.coordination(old);
  assert.equal(view.settings.heavy_lane, "one");
  assert.deepEqual(view.settings.shared_resources, []);
  assert.deepEqual(view.lane, { running: [], waiting: [] });
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, { projectId: "p", status: old, reload: forbidden }));
  assert.deepEqual(radios(markup, "Heavy lane").checked, ["1 at a time"]);
  assert.match(markup, /0 running, 0 waiting/u);
});

test("the Shared resources row shows the saved resources and the lane counts show on the Heavy lane row", () => {
  const entry = (agent) => ({ agent, label: "make", uses: [], since_ms: 0 });
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneControls, {
    projectId: "p", reload: forbidden,
    status: status({ shared_resources: [{ name: "ollama", slots: 1 }, { name: "postgres", slots: 2 }] }, { running: [entry(1)], waiting: [entry(2), entry(3)] }),
  }));
  assert.match(markup, /aria-label="Shared resources"/u);
  assert.match(markup, /value="ollama:1, postgres:2"/u);
  assert.match(markup, /1 running, 2 waiting/u);
});

test("typed shared resources follow the engine's rule: name or name:slots, 1 to 8 slots, at most 16, none twice", () => {
  assert.deepEqual(coord.sharedResourcesInput(""), { kind: "ok", resources: [] });
  assert.deepEqual(coord.sharedResourcesInput("ollama:1, postgres:2\nredis"), { kind: "ok", resources: [{ name: "ollama", slots: 1 }, { name: "postgres", slots: 2 }, { name: "redis", slots: 1 }] });
  assert.deepEqual(coord.sharedResourcesInput("model.v2_db-1:8").kind, "ok");
  for (const bad of ["Ollama", "ollama:0", "ollama:9", "ollama:two", "ollama:1:2", "-db", "a".repeat(33), "ollama ollama:2"]) {
    assert.equal(coord.sharedResourcesInput(bad).kind, "invalid", bad);
  }
  const many = Array.from({ length: 17 }, (_, at) => `r${at}`).join(",");
  assert.equal(coord.sharedResourcesInput(many).kind, "invalid", "17 names are too many");
  assert.equal(coord.sharedResourcesInput(many.split(",").slice(0, 16).join(",")).kind, "ok", "16 names are kept");
  assert.equal(coord.sharedResourcesText([{ name: "ollama", slots: 1 }, { name: "pg", slots: 3 }]), "ollama:1, pg:3");
});

test("the desktop resource bounds are the engine's", () => {
  assert.match(SETTINGS_RS, new RegExp(`pub const SHARED_RESOURCES_MAX: usize = ${coord.SHARED_RESOURCES_MAX};`, "u"));
  assert.match(SETTINGS_RS, new RegExp(`pub const RESOURCE_SLOTS_MAX: u8 = ${coord.RESOURCE_SLOTS_MAX};`, "u"));
  assert.match(SETTINGS_RS, /pub const RESOURCE_NAME_BYTES_MAX: usize = 32;/u);
});

test("a heavy lane save sends the engine's newest settings with only the lane or the resources changed", async () => {
  const stored = { p: { ...coord.COORD_DEFAULTS, nudges: "first_edit" } };
  const sent = [];
  globalThis.window = { __TAURI_INTERNALS__: { async invoke(command, args) {
    if (command === "desktop_team_status") return { project_id: args.id, coordination: { settings: { ...stored[args.id] }, harnesses: [] } };
    assert.equal(command, "desktop_team_coordination");
    sent.push(args.settings);
    stored[args.id] = { ...args.settings };
    return { outcome: "applied", pending: false, rejection: null };
  } } };
  for (const option of coord.HEAVY_LANE_OPTIONS) {
    assert.equal((await coord.saveCoordSettings("p", { heavy_lane: option.value })).kind, "ok", option.value);
  }
  assert.deepEqual(sent.map((settings) => settings.heavy_lane), ["one", "two", "three", "four", "no_limit"]);
  await coord.saveCoordSettings("p", { shared_resources: [{ name: "ollama", slots: 1 }] });
  assert.deepEqual(sent.at(-1), { ...coord.COORD_DEFAULTS, nudges: "first_edit", heavy_lane: "no_limit", shared_resources: [{ name: "ollama", slots: 1 }] });
});

test("the Team view lists running then waiting heavy commands with the agent's name, label, resources and time, never more", () => {
  const now = 10 * 60_000;
  const lane = {
    running: [{ agent: 4, label: "cargo", uses: ["ollama"], since_ms: now - 3 * 60_000 }],
    waiting: [{ agent: 9, label: "full suite", uses: [], since_ms: now - 20_000 }],
  };
  const agents = [{ id: 4, label: "Builder" }];
  const markup = renderToStaticMarkup(createElement(coord.HeavyLaneQueue, { status: status({ heavy_lane: "two" }, lane, agents), now }));
  assert.match(markup, /aria-label="Heavy lane"/u);
  assert.match(markup, /2 at a time: 1 running, 1 waiting/u);
  const rows = [...markup.matchAll(/<li[^>]*>([\s\S]*?)<\/li>/gu)].map((match) => match[1].replace(/<[^>]+>/gu, "").replace(/\s+/gu, " ").trim());
  assert.deepEqual(rows, ["Builder cargo uses ollama running for 3 minutes", "Agent 9 full suite waiting for under a minute"]);
  assert.equal(renderToStaticMarkup(createElement(coord.HeavyLaneQueue, { status: status(), now })), "", "an empty lane shows nothing");
});

test("the Team view mounts the heavy lane queue and Agent settings list both rows under Coordination", () => {
  const agents = readFileSync(new URL("../src/screens/project/agents.tsx", import.meta.url), "utf8");
  assert.match(agents, /<HeavyLaneQueue status=\{team\.result\.value\} now=\{data\.now\} \/>/u);
  const controls = readFileSync(new URL("../src/screens/team/controls.tsx", import.meta.url), "utf8");
  assert.match(controls, /<HeavyLaneControls projectId=\{projectId\} status=\{status\} reload=\{reload\} \/>/u);
  assert.match(controls, /"Terminal display", "Heavy lane", "Shared resources"/u);
});
