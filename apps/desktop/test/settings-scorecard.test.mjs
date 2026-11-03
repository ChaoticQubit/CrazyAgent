import "./load-source.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

const scorecard = await import("../src/screens/settings/scorecard.tsx");

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const NOW = Date.UTC(2026, 10, 3, 9, 0);
const START = Date.UTC(2026, 9, 21);
const forbidden = () => { throw new Error("static rendering must not call the engine"); };
const tally = (patch = {}) => ({ messages: 0, answers: 0, interventions: 0, tasks: 0, ...patch });
const settings = (patch = {}) => ({ window: "rolling", baseline: "first_days", daily_question: "off", ...patch });
const card = (patch = {}) => ({
  settings: settings(), version: 3,
  window: { start_ms: START, end_ms: START + 14 * DAY },
  baseline: { start_ms: START, end_ms: START + 3 * DAY },
  measured: { start_ms: START + 3 * DAY, end_ms: START + 14 * DAY },
  real: 3, clean: 2,
  tasks: [
    { ticket: "CA-12", title: "Sort the inbox by sender", closed_ms: NOW - 2 * DAY, change_sets: ["cs-4"], clean: true, flaws: [] },
    { ticket: "CA-9", title: "Fix the export button", closed_ms: NOW - 3 * DAY, change_sets: ["cs-2", "cs-3"], clean: false, flaws: ["discarded", "first_batch_not_passed"] },
  ],
  coordination: { baseline: tally({ messages: 6, answers: 1, interventions: 1, tasks: 2 }), measured: tally({ messages: 2, answers: 1, interventions: 0, tasks: 1 }) },
  acts: [{ kind: "owner_edit", project: "project-harbor", subject: "src/inbox.rs", at_ms: NOW - 3 * HOUR }],
  chosen_days: 4, answered_days: 5,
  days: [{ day: "2026-11-02", yes: true }, { day: "2026-11-01", yes: false }],
  targets: { tasks: 10, clean_of_ten: 8, coordination_cut_percent: 25, chosen_of_ten: 7 },
  met: { tasks: false, clean: false, coordination: true, chosen: false },
  ...patch,
});
const render = (patch = {}) => renderToStaticMarkup(createElement(scorecard.ScorecardView, {
  card: { kind: "ok", value: card() }, projects: new Map([["project-harbor", "Harbor Notes"]]), nowMs: NOW,
  failure: "", busy: false, onSave: forbidden, onRetry: forbidden, ...patch,
}));
const text = (markup) => markup.replace(/<[^>]*>/gu, " ").replace(/&#x27;/gu, "'").replace(/&amp;/gu, "&").replace(/\s+/gu, " ");

test("coordination per task divides the counted owner acts by the real tasks of each span", () => {
  assert.equal(scorecard.perTask(tally({ messages: 6, answers: 1, interventions: 1, tasks: 2 })), 4);
  assert.equal(scorecard.perTask(tally({ messages: 3 })), null);
  assert.equal(scorecard.perTaskText(4), "4");
  assert.equal(scorecard.perTaskText(1.25), "1.3");
  assert.equal(scorecard.perTaskText(null), "No tasks yet");
  const cut = (baseline, measured) => scorecard.coordinationCut({ baseline: tally(baseline), measured: tally(measured) });
  assert.equal(cut({ messages: 8, tasks: 2 }, { messages: 3, tasks: 1 }), 25);
  assert.equal(cut({ messages: 2, tasks: 2 }, { messages: 3, tasks: 1 }), -200);
  assert.equal(cut({ tasks: 2 }, { messages: 3, tasks: 1 }), null);
  assert.equal(cut({ messages: 8 }, { messages: 3, tasks: 1 }), null);
  assert.equal(cut({ messages: 8, tasks: 2 }, { messages: 3 }), null);
  assert.equal(scorecard.cutText(25), "25% less");
  assert.equal(scorecard.cutText(-200), "200% more");
  assert.equal(scorecard.cutText(0), "the same");
});

test("every option of every setting is offered under the engine's own name and saved whole", () => {
  assert.deepEqual(scorecard.WINDOWS.map((option) => option.value), ["rolling", "from_day"]);
  assert.deepEqual(scorecard.BASELINES.map((option) => option.value), ["first_days", "previous_window"]);
  assert.deepEqual(scorecard.DAILY_QUESTIONS, ["off", "on"]);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/scorecard.rs", import.meta.url), "utf8");
  for (const name of ["Rolling", "FromDay(String)", "FirstDays", "PreviousWindow", "Off", "On"]) assert.ok(engine.includes(`    ${name},`), name);
  assert.deepEqual(scorecard.withWindow(settings(), "from_day", "2026-10-21"), settings({ window: { from_day: "2026-10-21" } }));
  assert.deepEqual(scorecard.withWindow(settings({ window: { from_day: "2026-10-21" } }), "rolling", "2026-11-03"), settings());
  const names = engine.slice(engine.indexOf("pub fn name(self)"), engine.indexOf("fn parse(name"));
  assert.deepEqual([...names.matchAll(/=> "([a-z_]+)"/g)].map((match) => match[1]), scorecard.ACT_KINDS);
  assert.equal(scorecard.windowChoice("rolling"), "rolling");
  assert.equal(scorecard.windowChoice({ from_day: "2026-10-21" }), "from_day");
  const shown = text(render({ card: { kind: "ok", value: card({ settings: settings({ window: { from_day: "2026-10-21" }, baseline: "previous_window", daily_question: "on" }) }) } }));
  for (const option of [...scorecard.WINDOWS, ...scorecard.BASELINES]) assert.ok(shown.includes(option.label), option.label);
  assert.match(shown, /Start day/u);
  assert.match(render(), /role="switch"[^>]*aria-checked="false"/u);
  assert.match(render({ card: { kind: "ok", value: card({ settings: settings({ daily_question: "on" }) }) } }), /role="switch"[^>]*aria-checked="true"/u);
  assert.doesNotMatch(text(render()), /Start day/u);
});

test("a start day is a calendar day written the way the engine reads it", () => {
  assert.ok(scorecard.calendarDay("2026-11-03"));
  assert.ok(scorecard.calendarDay("2028-02-29"));
  assert.ok(scorecard.calendarDay("0012-01-01"));
  assert.ok(!scorecard.calendarDay("2026-02-30"));
  assert.ok(!scorecard.calendarDay("2026-13-01"));
  assert.ok(!scorecard.calendarDay("2026-1-03"));
  assert.ok(!scorecard.calendarDay("2026-11-03T00:00"));
  assert.ok(!scorecard.calendarDay(""));
  assert.equal(scorecard.dayText(Date.UTC(2026, 10, 3, 23, 30), 60), "2026-11-04");
  assert.equal(scorecard.dayText(Date.UTC(2026, 10, 3, 0, 30), -60), "2026-11-02");
});

test("the four numbers show with their targets and the records behind each", () => {
  const shown = text(render());
  for (const words of ["Real tasks 3", "Target 10", "Clean 2 of 3", "Target 8 of 10", "Coordination per task 3", "Baseline 4, 25% less", "Target 25% less",
    "Chose CrazyAgent 4 of 5 days", "Target 7 of 10 answered days"]) assert.ok(shown.includes(words), `${words} in: ${shown}`);
  assert.equal(shown.split("Target met").length, 2, "only coordination is met");
  for (const words of ["CA-12", "Sort the inbox by sender", "cs-2, cs-3", "A change set was discarded; Its first check batch didn't pass",
    "Harbor Notes", "Edit", "src/inbox.rs", "3 h ago", "2026-11-02 Yes", "2026-11-01 No", "Showing the newest 2 of 3"]) assert.ok(shown.includes(words), `${words} in: ${shown}`);
  const quiet = text(render({ card: { kind: "ok", value: card({ real: 0, clean: 0, tasks: [], acts: [], days: [], chosen_days: 0, answered_days: 0,
    coordination: { baseline: tally(), measured: tally() }, met: { tasks: false, clean: false, coordination: false, chosen: false } }) } }));
  assert.match(quiet, /No real tasks in this window yet/u);
  assert.match(quiet, /The daily question is off/u);
  assert.doesNotMatch(quiet, /Target met/u);
});

test("a scorecard this engine can't serve reads as not ready, and every refusal as its own sentence", () => {
  assert.match(text(render({ card: { kind: "not_ready" } })), /isn't ready yet/u);
  assert.match(text(render({ card: { kind: "error", code: "scorecard_unavailable", message: "" } })), /isn't ready yet/u);
  assert.match(text(render({ card: { kind: "error", code: "scorecard_request_invalid", message: "" } })), /didn't accept this scorecard request/u);
  assert.match(text(render({ failure: "These settings changed in another window." })), /changed in another window/u);
  const engine = readFileSync(new URL("../../../crates/crazyagent/src/engine/owner/transport/scorecard.rs", import.meta.url), "utf8");
  const listed = engine.slice(engine.indexOf("pub const SCORECARD_CODES"), engine.indexOf("];", engine.indexOf("pub const SCORECARD_CODES")));
  const own = [...listed.matchAll(/"(scorecard_[a-z_]+)"/g)].map((match) => match[1]);
  assert.equal(own.length, 6);
  for (const code of own) assert.ok(scorecard.SCORECARD_REFUSALS.includes(code), `${code} has its own sentence`);
  assert.equal(scorecard.scorecardRefusal({ code: "scorecard_settings_invalid" }), "That start day isn't a calendar day. Pick a day like 2026-11-03.");
});
