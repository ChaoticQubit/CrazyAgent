import assert from "node:assert/strict";
import { test } from "node:test";
import { teamWait } from "./trial3-team.mjs";

const TEAM_MS = 7200000;
const FLOOR_MS = 30000;

/** Runs the three team steps one after another, each using all the time it is given, as when
 * the team never forms. Inputs: the mode and the failed steps. Output: the total wait in ms. */
function failedTeamPhase(mode, failed) {
  let now = 1000;
  let teamBy = null;
  for (let step = 0; step < 3; step += 1) {
    const next = teamWait({ mode, failed, teamBy, now, teamMs: TEAM_MS, floorMs: FLOOR_MS });
    teamBy = next.teamBy;
    now += next.wait;
  }
  return now - 1000;
}

test("a live team step does not run when the prompt or the project failed", () => {
  for (const failed of [["send-prompt"], ["add-project"], ["add-project", "send-prompt"]]) {
    assert.throws(() => teamWait({ mode: "live", failed, teamBy: null, now: 0, teamMs: TEAM_MS, floorMs: FLOOR_MS }),
      { message: `not run: ${failed.join(" and ")} failed` }, `live after ${failed.join(", ")}`);
  }
  assert.throws(() => teamWait({ mode: "fixture", failed: ["add-project"], teamBy: null, now: 0, teamMs: TEAM_MS, floorMs: FLOOR_MS }),
    { message: "not run: add-project failed" });
});

test("a fixture team step still runs after the expected send-prompt failure, since the fixture seeds its team", () => {
  const first = teamWait({ mode: "fixture", failed: ["send-prompt", "hidden-folders"], teamBy: null, now: 5, teamMs: 90000, floorMs: FLOOR_MS });
  assert.deepEqual(first, { wait: 90000, teamBy: 90005 });
});

test("the three team steps share one deadline, so a team that never forms costs the trial one team wait, not three", () => {
  const total = failedTeamPhase("live", ["hidden-folders"]);
  assert.ok(total <= TEAM_MS + 2 * FLOOR_MS, `team phase waited ${total} ms`);
  const later = teamWait({ mode: "live", failed: [], teamBy: 50000, now: 20000, teamMs: TEAM_MS, floorMs: FLOOR_MS });
  assert.deepEqual(later, { wait: FLOOR_MS, teamBy: 50000 }, "a step reached near the deadline still gets the floor to read once");
});

test("a team wait refuses inputs that break its own rules", () => {
  const ok = { mode: "live", failed: [], teamBy: null, now: 0, teamMs: TEAM_MS, floorMs: FLOOR_MS };
  assert.throws(() => teamWait({ ...ok, mode: "tui" }), { message: "the mode is live or fixture" });
  assert.throws(() => teamWait({ ...ok, floorMs: TEAM_MS + 1 }), { message: "the floor is positive and within the team time" });
  assert.throws(() => teamWait({ ...ok, teamBy: TEAM_MS + 1 }), { message: "the shared deadline never moves later" });
});
