import assert from "node:assert/strict";
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const CHECKS = fileURLToPath(new URL("trial4/", import.meta.url));
export const TUI_CHECKS = fileURLToPath(new URL("../../../../scripts/trial/tui4/", import.meta.url));
const ID = /^[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+$/;
const OP = /^OP-[1-9][0-9]*$/;

/** Whether a value is a non-empty string. Input: the value. Output: a boolean. */
function filled(value) {
  return typeof value === "string" && value.trim().length > 0;
}

/** Checks one check against the trial 4 convention (leader/trial/TRIAL4-CONVENTION.md).
 * Inputs: the check, its file name and the trial 3 step slugs. Throws the file and the
 * broken rule. */
function validate(check, file, slugs) {
  assert(filled(check?.id) && ID.test(check.id) && check.id.length <= 24, `${file}: id must look like N-D1, got ${JSON.stringify(check?.id)}`);
  const where = `${file} ${check.id}`;
  assert(Array.isArray(check.op) && check.op.length > 0 && check.op.every((row) => OP.test(row)), `${where}: op must list OP-<n> rows of owner-problems-v1.md`);
  assert(filled(check.title), `${where}: title must be a non-empty string`);
  assert(filled(check.why), `${where}: why must be a non-empty string`);
  assert(check.after === "end" || slugs.includes(check.after), `${where}: after must name a trial 3 step or "end", not ${check.after}`);
  assert.equal(typeof check.run, "function", `${where}: run must be a function`);
}

/** Loads every check module of a folder in file name order. Inputs: the folder, which may be
 * missing, and the trial 3 step slugs. Output: [{ id, op, title, why, after, run, file }] in
 * file then list order; rejects the first module that breaks the convention or a repeated id. */
export async function loadChecks(folder, slugs) {
  assert(Array.isArray(slugs) && slugs.length > 0, "the trial 3 slugs are given");
  if (!existsSync(folder)) return [];
  const files = readdirSync(folder).filter((name) => name.endsWith(".mjs")).sort();
  const loaded = [];
  const seen = new Map();
  for (const file of files) {
    const { checks } = await import(pathToFileURL(join(folder, file)).href);
    assert(Array.isArray(checks) && checks.length > 0, `${file}: exports no non-empty checks list`);
    for (const check of checks) {
      validate(check, file, slugs);
      assert(!seen.has(check.id), `${check.id} is in ${seen.get(check.id)} and ${file}`);
      seen.set(check.id, file);
      loaded.push({ ...check, file });
    }
  }
  return loaded;
}

/** Orders the trial 4 run: each trial 3 step, then the checks that run after it, then the
 * "end" checks. Inputs: the trial 3 steps and the loaded desktop and TUI checks. Output:
 * [{ slug, step } or { slug, check }], a check's slug being its id in lower case; throws
 * when two checks share an id. */
export function plan(steps, checks) {
  const seen = new Map();
  for (const check of checks) {
    assert(!seen.has(check.id), `${check.id} is in ${seen.get(check.id)} and ${check.file}`);
    seen.set(check.id, check.file);
  }
  const after = (slug) => checks.filter((check) => check.after === slug).map((check) => ({ slug: check.id.toLowerCase(), check }));
  const order = steps.flatMap((step) => [{ slug: step.slug, step }, ...after(step.slug)]).concat(after("end"));
  assert.equal(order.length, steps.length + checks.length, "every check runs once");
  return order;
}
