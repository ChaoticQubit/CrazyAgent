import assert from "node:assert/strict";
import test from "node:test";
import { budgetRows, callBudgets, cpuMs, cpuTimeMs, startToReady } from "./speed-absolute.mjs";
import { KEPT_MIN } from "./speed-budget.mjs";

const START_241 = [[3778, 355, 64], [3636, 349, 42], [3802, 862, 56], [3502, 344, 42], [3781, 336, 49],
  [3479, 332, 46], [3948, 726, 50], [3654, 657, 47], [3521, 338, 42], [3342, 332, 45]]
  .map(([engine, webview, reload]) => ({ engine_start_ms: engine, launch_webview_ms: webview, reload_home_ms: reload }));
const IDLE_241 = {
  "idle:home": { desktop_ide_focus: 60, desktop_found_projects: 20, desktop_intake_status: 20, desktop_team_events: 6, desktop_question_counts: 6, desktop_usage_extras: 1 },
  "idle:project": { desktop_found_projects: 20, desktop_intake_status: 20, desktop_ide_focus: 60, desktop_team_status: 20, desktop_uncommitted_instruction_import: 20,
    desktop_question_counts: 26, desktop_questions: 12, desktop_overview_read: 12, desktop_team_events: 6, desktop_changes: 6, desktop_usage_extras: 1 },
};
const IDLE_QUIET = { "idle:home": { desktop_changes: 1 }, "idle:project": { desktop_changes: 1 } };
const IDLE_CPU = { "idle:home": 120, "idle:project": 150 };

/** Engine calls for the p95 rows: quick ones, then `slow` calls of 25 ms. Input: the slow
 * count. Output: 100 `{ cmd, ms }` calls. */
function calls(slow) {
  return [
    ...Array.from({ length: 100 - slow }, () => ({ cmd: "desktop_intake_status", ms: 3 })),
    ...Array.from({ length: slow }, () => ({ cmd: "desktop_found_projects", ms: 25 })),
  ];
}

test("start to ready is engine spawn to its ready line, app launch to its first document and a new document to Home usable; 2.4.1's kept runs (gate 11-16 08:20, b side, whole ms) miss the 2 s budget (v2.5 S1)", () => {
  assert.equal(startToReady({ engine_start_ms: 310, launch_webview_ms: 380, reload_home_ms: 520, memory_app_mb: 9 }), 1210);
  assert.deepEqual(budgetRows(START_241), [{ budget: "start_to_ready_ms", n: 10, median: 4096.5, p90: 4720, limit: 2000, pass: false }]);
  const under = Array.from({ length: KEPT_MIN }, () => ({ engine_start_ms: 1500, launch_webview_ms: 400, reload_home_ms: 99 }));
  assert.equal(budgetRows(under)[0].pass, true);
  assert.equal(budgetRows(under.map((sample) => ({ ...sample, reload_home_ms: 100 })))[0].pass, false, "a reading at the budget is not under it");
  assert.throws(() => startToReady({ engine_start_ms: 310, launch_webview_ms: 380 }), /reload/);
  assert.throws(() => budgetRows(under.slice(1)), /KEPT_MIN|at least/);
});

test("the idle project page's 2.4.1 minute (gate 11-16 08:20, C1 candidate), 203 calls, misses the budget of under 5 calls a minute; 4 meet it, and every budget's reading is kept with its limit (v2.5 S1)", () => {
  const missed = callBudgets({ counts: IDLE_241, calls: calls(0), cpu: IDLE_CPU, idleMs: 60000 });
  assert.deepEqual(missed.findings, [{ budget: "idle_project_calls", scenario: "idle:project", reading: 203, limit: 5, pass: false }]);
  assert.deepEqual(missed.rows.map((row) => [row.budget, row.scenario ?? null, row.reading, row.limit, row.pass]), [
    ["idle_project_calls", "idle:project", 203, 5, false],
    ["call_p95_ms", null, 3, 20, true],
    ["idle_engine_cpu_ms", "idle:home", 120, 600, true],
    ["idle_engine_cpu_ms", "idle:project", 150, 600, true],
  ]);
  assert.deepEqual(callBudgets({ counts: { ...IDLE_241, "idle:project": { desktop_changes: 4 } }, calls: calls(0), cpu: IDLE_CPU, idleMs: 60000 }).findings, []);
  const twoMinutes = callBudgets({ counts: { ...IDLE_241, "idle:project": { desktop_changes: 9 } }, calls: calls(0), cpu: IDLE_CPU, idleMs: 120000 });
  assert.deepEqual(twoMinutes.rows[0], { budget: "idle_project_calls", scenario: "idle:project", reading: 4.5, limit: 5, pass: true });
  assert.throws(() => callBudgets({ counts: { "idle:home": {} }, calls: calls(0), cpu: IDLE_CPU, idleMs: 60000 }), /idle:project/);
  assert.throws(() => callBudgets({ counts: IDLE_241, calls: calls(0), cpu: IDLE_CPU, idleMs: 30000 }), /idle window/);
});

test("95% of engine calls must be under 20 ms: six slow calls in a hundred miss it, five do not, and each command's p95 is kept with its count; an over-budget example, as 2.4.1's timed reads are about 6 ms (v2.5 S1)", () => {
  const missed = callBudgets({ counts: IDLE_QUIET, calls: calls(6), cpu: IDLE_CPU, idleMs: 60000 });
  assert.deepEqual(missed.findings, [{ budget: "call_p95_ms", reading: 25, n: 100, limit: 20, pass: false }]);
  assert.deepEqual(missed.p95_by_cmd, { desktop_found_projects: { n: 6, p95: 25 }, desktop_intake_status: { n: 94, p95: 3 } });
  assert.deepEqual(callBudgets({ counts: IDLE_QUIET, calls: calls(5), cpu: IDLE_CPU, idleMs: 60000 }).findings, []);
  const atLimit = calls(0).map((call) => ({ ...call, ms: 20 }));
  assert.equal(callBudgets({ counts: IDLE_QUIET, calls: atLimit, cpu: IDLE_CPU, idleMs: 60000 }).findings[0].budget, "call_p95_ms");
  assert.throws(() => callBudgets({ counts: IDLE_QUIET, calls: [], cpu: IDLE_CPU, idleMs: 60000 }), /engine call/);
  assert.throws(() => callBudgets({ counts: IDLE_QUIET, calls: [{ cmd: "x", ms: -1 }], cpu: IDLE_CPU, idleMs: 60000 }), /engine call/);
});

test("the engine's CPU over each idle minute must be under 600 ms, 1% of one core; an over-budget example, as no 2.4.1 reading exists (v2.5 S1)", () => {
  const missed = callBudgets({ counts: IDLE_QUIET, calls: calls(0), cpu: { "idle:home": 599, "idle:project": 600 }, idleMs: 60000 });
  assert.deepEqual(missed.findings, [{ budget: "idle_engine_cpu_ms", scenario: "idle:project", reading: 600, limit: 600, pass: false }]);
  const twoMinutes = callBudgets({ counts: IDLE_QUIET, calls: calls(0), cpu: { "idle:home": 1100, "idle:project": 1198 }, idleMs: 120000 });
  assert.deepEqual(twoMinutes.findings, []);
  assert.throws(() => callBudgets({ counts: IDLE_QUIET, calls: calls(0), cpu: { "idle:home": -10, "idle:project": 0 }, idleMs: 60000 }), /CPU/);
  assert.throws(() => callBudgets({ counts: IDLE_QUIET, calls: calls(0), cpu: { "idle:project": 0 }, idleMs: 60000 }), /CPU/);
});

test("ps's accumulated CPU time reads as ms, with minutes past 59, an hours field and a days field", () => {
  assert.equal(cpuTimeMs("0:01.50"), 1500);
  assert.equal(cpuTimeMs("187:44.32"), 11264320);
  assert.equal(cpuTimeMs("1:02:03.50"), 3723500);
  assert.equal(cpuTimeMs("2-01:00:00.00"), 176400000);
  assert.throws(() => cpuTimeMs("1.5"), /CPU time/);
  assert.throws(() => cpuTimeMs(""), /CPU time/);
  assert.throws(() => cpuMs(0), /a pid/);
});
