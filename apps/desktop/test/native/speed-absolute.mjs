/** The absolute budgets every release meets (v2.5 plan Q4): start to ready ms, engine call
 * p95 ms, idle project page calls a minute and engine CPU ms over an idle minute. */
export const BUDGETS = { start_to_ready_ms: 2000, call_p95_ms: 20, idle_project_calls: 5, idle_engine_cpu_ms: 600 };

/** Stands in for one run's start to ready until it moves here. Output: 0. */
export function startToReady() {
  return 0;
}

/** Stands in for the start to ready budget rows until they move here. Output: no rows. */
export function budgetRows() {
  return [];
}

/** Stands in for the C1 walk's call and idle budgets until they move here. Output: nothing
 * read and nothing missed. */
export function callBudgets() {
  return { rows: [], p95_by_cmd: {}, findings: [] };
}

/** Stands in for reading ps's CPU time until it moves here. Output: 0. */
export function cpuTimeMs() {
  return 0;
}

/** Stands in for one process's CPU time until it moves here. Output: 0. */
export function cpuMs() {
  return 0;
}
