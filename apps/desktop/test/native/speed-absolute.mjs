import assert from "node:assert/strict";
import { KEPT_MIN, output, round, summary } from "./speed-budget.mjs";

/** The absolute budgets every release meets (v2.5 plan Q4), judged on one build's own
 * readings, never against a base: start to ready ms, the 95th percentile of engine call ms,
 * idle project page calls a minute and engine CPU ms over an idle minute. A reading passes only
 * under its budget. The CPU budget, 1% of one core, holds until the S2 baseline reads it. */
export const BUDGETS = { start_to_ready_ms: 2000, call_p95_ms: 20, idle_project_calls: 5, idle_engine_cpu_ms: 600 };
/** The idle minutes of the C1 walk whose engine CPU the CPU budget judges. */
const IDLE_SCENARIOS = ["idle:home", "idle:project"];
const MINUTE_MS = 60000;
/** Most engine calls one budget check takes, as speed-calls bounds its call log. */
const BUDGET_CALLS_MAX = 20000;

/** The value at a whole percent of sorted numbers, by nearest rank. Inputs: the sorted values
 * and the percent. Output: the value. */
function nearestRank(sorted, percent) {
  return sorted[Math.ceil((percent * sorted.length) / 100) - 1];
}

/** One run's start to ready (v2.5 S1): engine spawn to its ready line, app launch to its page's
 * first document, and a new document to Home usable, each timed exactly, so the harness's
 * seeding between engine and app and WebDriver's late attach count in none. Input: one sample.
 * Output: the ms. */
export function startToReady(sample) {
  const spans = [sample.engine_start_ms, sample.launch_webview_ms, sample.reload_home_ms];
  assert.ok(spans.every((ms) => Number.isFinite(ms) && ms >= 0), `an engine start, a webview start and a reload: ${spans.join(", ")}`);
  return round(spans[0] + spans[1] + spans[2]);
}

/** Judges one build's kept runs against the start to ready budget, apart from the DEC-169
 * verdict, so a budget a later build is planned to meet never fails a compare today. Input: at
 * least `KEPT_MIN` samples. Output: the row: count, median and p90, the limit, and whether the
 * median is under it. */
export function budgetRows(samples) {
  assert.ok(Array.isArray(samples) && samples.length >= KEPT_MIN, `a budget needs at least KEPT_MIN (${KEPT_MIN}) kept runs`);
  const { n, median, p90 } = summary(samples.map(startToReady));
  const limit = BUDGETS.start_to_ready_ms;
  return [{ budget: "start_to_ready_ms", n, median, p90, limit, pass: median < limit }];
}

/** Judges one build's C1 walk against the call and idle budgets (v2.5 S1): the idle project
 * page under `BUDGETS.idle_project_calls` calls a minute, 95% of its engine calls under
 * `BUDGETS.call_p95_ms`, and the engine's CPU over each idle minute under
 * `BUDGETS.idle_engine_cpu_ms`. Inputs: `{ scenario: { cmd: count } }`, every engine call's
 * `{ cmd, ms }`, the engine CPU ms per idle scenario and the idle window. Output: every reading
 * with its limit, each command's p95 with its count, and the rows it misses. */
export function callBudgets({ counts, calls, cpu, idleMs }) {
  assert.ok(Number.isInteger(idleMs) && idleMs >= MINUTE_MS, `an idle window of a minute or more: ${idleMs}`);
  assert.ok(Object.hasOwn(counts, "idle:project"), "the walk ran idle:project");
  assert.ok(Array.isArray(calls) && calls.length >= 1 && calls.length <= BUDGET_CALLS_MAX, `1 to ${BUDGET_CALLS_MAX} engine calls`);
  const byCmd = new Map();
  for (const call of calls) {
    assert.ok(typeof call.cmd === "string" && Number.isFinite(call.ms) && call.ms >= 0, `an engine call with its ms: ${JSON.stringify(call)}`);
    if (!byCmd.has(call.cmd)) byCmd.set(call.cmd, []);
    byCmd.get(call.cmd).push(call.ms);
  }
  const perMinute = (value) => round((value * MINUTE_MS) / idleMs);
  const idleCalls = perMinute(Object.values(counts["idle:project"]).reduce((total, count) => total + count, 0));
  const p95 = nearestRank(calls.map((call) => call.ms).sort((left, right) => left - right), 95);
  const rows = [
    { budget: "idle_project_calls", scenario: "idle:project", reading: idleCalls, limit: BUDGETS.idle_project_calls, pass: idleCalls < BUDGETS.idle_project_calls },
    { budget: "call_p95_ms", reading: p95, n: calls.length, limit: BUDGETS.call_p95_ms, pass: p95 < BUDGETS.call_p95_ms },
    ...IDLE_SCENARIOS.map((scenario) => {
      assert.ok(Number.isFinite(cpu[scenario]) && cpu[scenario] >= 0, `the engine's CPU ms over ${scenario}: ${cpu[scenario]}`);
      const reading = perMinute(cpu[scenario]);
      return { budget: "idle_engine_cpu_ms", scenario, reading, limit: BUDGETS.idle_engine_cpu_ms, pass: reading < BUDGETS.idle_engine_cpu_ms };
    }),
  ];
  const p95ByCmd = Object.fromEntries([...byCmd.keys()].sort().map((cmd) => {
    const times = byCmd.get(cmd).sort((left, right) => left - right);
    return [cmd, { n: times.length, p95: nearestRank(times, 95) }];
  }));
  return { rows, p95_by_cmd: p95ByCmd, findings: rows.filter((row) => !row.pass) };
}

/** Reads ps's accumulated CPU time (`time`, user plus system) as ms. Input: the field, as
 * `[d-][hh:]mm:ss.ss`. Output: the ms. */
export function cpuTimeMs(text) {
  const found = /^(?:(\d+)-)?(?:(\d+):)?(\d+):(\d{2}(?:\.\d+)?)$/.exec(text);
  assert.ok(found, `a CPU time: ${text}`);
  const [days, hours, minutes] = [found[1], found[2], found[3]].map((part) => Number(part ?? 0));
  return Math.round((((days * 24 + hours) * 60 + minutes) * 60 + Number(found[4])) * 1000);
}

/** Reads one running process's accumulated CPU time through ps; a process that ended fails the
 * read. Input: the pid. Output: the ms. */
export function cpuMs(pid) {
  assert.ok(Number.isInteger(pid) && pid > 1, `a pid: ${pid}`);
  return cpuTimeMs(output("/bin/ps", ["-o", "time=", "-p", String(pid)]).trim());
}
