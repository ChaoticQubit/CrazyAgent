import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { setTimeout as pause } from "node:timers/promises";
import { POLL_MS } from "./e2e-harness.mjs";

const WAIT_MS = 90000;
const WAIT_MAX_MS = 300000;
const STEPS_MAX = 64;

/** Starts a flow's run record with no golden engine, bridge or browser, for a flow that drives
 * the real app: its name, its evidence folder, no step and no measure yet, and no record an
 * earlier run left. Inputs: the empty run, the flow's name and the evidence folder's absolute
 * path. Output: none. */
export function startRecord(run, flow, evidence) {
  assert.match(flow, /^[a-z]+(-[a-z]+)*$/);
  assert.ok(flow.length <= 64 && typeof evidence === "string" && isAbsolute(evidence), `${flow} ${evidence}`);
  assert.deepEqual(Object.keys(run), [], "a golden run starts empty");
  Object.assign(run, { flow, steps: [], measures: {}, started: Date.now(), finished: false, evidence });
  run.mark = run.started;
  mkdirSync(evidence, { recursive: true, mode: 0o700 });
  rmSync(recordPath(run), { force: true });
}

/** Ends one step of the flow, timed from the end of the one before.
 * Inputs: the run and the step's name, a fixed text that holds no path and no name the owner
 * typed. Output: none. */
export function step(run, name) {
  assert.ok(typeof name === "string" && name.length > 0 && !run.finished);
  assert.ok(run.steps.length < STEPS_MAX, `a flow has at most ${STEPS_MAX} steps`);
  const now = Date.now();
  run.steps.push({ step: name, ms: now - run.mark });
  run.mark = now;
}

/** Keeps one measure of the flow for its run record, such as a start's time or a folder's
 * growth. Inputs: the run, started by `startRecord`, the measure's name, lowercase words joined
 * by `_` and set once, and its value, a whole number of at least 0. Output: none. */
export function measure(run, name, value) {
  assert.match(name, /^[a-z]+(_[a-z]+)*$/);
  assert.ok(Number.isSafeInteger(value) && value >= 0, `${name}: ${value}`);
  assert.ok(!run.finished && run.measures !== undefined && !Object.hasOwn(run.measures, name), name);
  assert.ok(Object.keys(run.measures).length < STEPS_MAX);
  run.measures[name] = value;
}

/** Ends the flow as passed: writes its run record, then reads it back from disk.
 * Input: the run.
 * Output: the record as the evidence folder holds it, or null when it holds none. */
export function finishGolden(run) {
  assert.ok(!run.finished);
  run.finished = true;
  writeRecord(run, "passed");
  return existsSync(recordPath(run)) ? JSON.parse(readFileSync(recordPath(run), "utf8")) : null;
}

/** Reads until `done` accepts what `read` gave, every POLL_MS, for at most `ms`: the one place a
 * golden flow sleeps (Q2). Inputs: what is awaited, for the failure, the read, the check and
 * the limit (at most five minutes). Output: the value the check accepted. */
export async function waitUntil(what, read, done, ms = WAIT_MS) {
  assert.ok(ms > 0 && ms <= WAIT_MAX_MS, `${what}: ${ms} ms`);
  const deadline = Date.now() + ms;
  for (;;) {
    const found = await read();
    if (done(found)) return found;
    assert.ok(Date.now() < deadline, `${what} never came in ${ms} ms: ${JSON.stringify(found)}`);
    await pause(POLL_MS);
  }
}

/** Where a flow's run record goes in the evidence folder.
 * Input: the run.
 * Output: the path. */
export function recordPath(run) {
  return join(run.evidence, `${run.flow}-desktop.json`);
}

/** Writes the run record: the flow, its outcome, its whole time, each step's time and the
 * measures when the flow kept any, with no path and no name.
 * Inputs: the run and "passed" or "failed". Output: none. */
export function writeRecord(run, outcome) {
  assert.ok(outcome === "passed" || outcome === "failed");
  const record = { flow: run.flow, client: "desktop", outcome, total_ms: Date.now() - run.started, steps: run.steps };
  if (run.measures && Object.keys(run.measures).length > 0) record.measures = run.measures;
  writeFileSync(recordPath(run), JSON.stringify(record, null, 1), { mode: 0o600 });
}
