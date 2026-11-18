import "./load-source.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

globalThis.window = {
  __TAURI_INTERNALS__: {
    async invoke(command) {
      assert.equal(command, "desktop_watch");
      return null;
    },
    transformCallback: () => 1,
  },
};

const { COALESCE_MS, changed, readOnChanges } = await import("../src/engine-changes.ts");

const settle = async () => {
  for (let turn = 0; turn < 8; turn += 1) await new Promise((resolve) => setImmediate(resolve));
};

test("v2.5 S4: a watched read runs once the page watches, again on its topic and on unknown, one at a time, and stops when its apply says so", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const answers = [];
  const applied = [];
  const stop = readOnChanges(["team"], () => new Promise((resolve) => answers.push(resolve)), (value) => {
    applied.push(value);
    return value !== "stop";
  });
  await settle();
  assert.equal(answers.length, 1, "the first read starts once the page watches");
  changed({ topics: ["team"] });
  t.mock.timers.tick(COALESCE_MS);
  changed({ topics: ["team"] });
  t.mock.timers.tick(COALESCE_MS);
  await settle();
  assert.equal(answers.length, 1, "a change heard during a read never starts a second one beside it");
  answers[0]("first");
  await settle();
  assert.deepEqual(applied, ["first"]);
  assert.equal(answers.length, 2, "a change heard during the read runs it once more after");
  answers[1]("second");
  await settle();
  assert.equal(answers.length, 2, "two changes heard during one read run it once more, not twice");
  changed({ topics: ["ide"] });
  await settle();
  assert.equal(answers.length, 2, "a change to another topic reads nothing");
  t.mock.timers.tick(COALESCE_MS);
  changed({ topics: ["unknown"] });
  await settle();
  assert.equal(answers.length, 3, "a change of unknown subject reads again");
  answers[2]("stop");
  await settle();
  assert.deepEqual(applied, ["first", "second", "stop"]);
  t.mock.timers.tick(COALESCE_MS);
  changed({ topics: ["team"] });
  await settle();
  assert.equal(answers.length, 3, "a read whose apply said stop is never run again");
  stop();
  t.mock.timers.tick(COALESCE_MS);
});

test("v2.5 S4: a stopped watched read applies no reply still in flight and reads no more", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const answers = [];
  const applied = [];
  const stop = readOnChanges(["landing"], () => new Promise((resolve) => answers.push(resolve)), (value) => {
    applied.push(value);
    return true;
  });
  await settle();
  assert.equal(answers.length, 1);
  stop();
  answers[0]("late");
  await settle();
  assert.deepEqual(applied, [], "a reply that comes after the stop is dropped");
  changed({ topics: ["landing", "unknown"] });
  await settle();
  assert.equal(answers.length, 1, "a stopped read hears no change");
  t.mock.timers.tick(COALESCE_MS);
  assert.throws(() => readOnChanges([], async () => 0, () => true), /needs a topic/);
});

test("v2.5 S4: the team, the question dock, the Questions tab, the Overview tabs and the waiting change sets read on the engine's watch, with no poll left", () => {
  const source = (path) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");
  const body = (text, start) => {
    const at = text.indexOf(start);
    assert.ok(at >= 0, start);
    return text.slice(at, text.indexOf("\n}\n", at));
  };
  const readers = [
    ["screens/project/team.tsx", "export function useTeam(", "team"],
    ["screens/project/question-popup.tsx", "export function QuestionPopup(", "team"],
    ["screens/project/questions.tsx", "export function QuestionsTab(", "team"],
    ["screens/project/overview-tabs.tsx", "export function ProjectOverviewTabs(", "projects"],
    ["screens/changes/waiting.tsx", "export function WaitingChanges(", "landing"],
  ];
  for (const [path, start, topic] of readers) {
    const reader = body(source(path), start);
    assert.match(reader, new RegExp(`readOnChanges\\(\\["${topic}"\\],`), `${path} reads on ${topic}`);
    assert.doesNotMatch(reader, /setInterval|setTimeout/, `${path} keeps no timer`);
  }
  assert.doesNotMatch(source("screens/project/team.tsx"), /TEAM_POLL_MS/);
  assert.doesNotMatch(source("screens/project/questions.tsx"), /LIST_POLL_MS/);
  assert.doesNotMatch(source("screens/project/question-popup.tsx"), /LIST_POLL_MS/);
  assert.doesNotMatch(source("screens/project/overview-tabs.tsx"), /pollEvery|OVERVIEW_POLL_MS|OVERVIEW_WAIT_MAX_MS/);
  assert.doesNotMatch(source("api-overview.ts"), /OVERVIEW_POLL_MS|OVERVIEW_WAIT_MAX_MS/);
  assert.doesNotMatch(source("screens/changes/waiting.tsx"), /WAITING_POLL_MS|useChangeList/);
  const project = source("screens/project/index.tsx");
  assert.match(project, /useLoad\(\(\) => loadUncommittedImport\(projectId\), \[projectId, data\.team\.at\]\)/, "the checkout read follows the team read, so the watch");
  assert.match(project, /usePendingQuestions\(projectId, data\.team\.at\)/, "the waiting count follows the team read, so the watch");
});
