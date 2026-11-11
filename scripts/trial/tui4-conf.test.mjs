import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { loadChecks } from "../../apps/desktop/test/native/trial4-checks.mjs";
import { TRIAL3_STEPS } from "../../apps/desktop/test/native/trial3-steps.mjs";

const TUI4 = fileURLToPath(new URL("tui4/", import.meta.url));
const SLUGS = TRIAL3_STEPS.map((step) => step.slug);

test("every tui4 check module passes the shared trial 4 convention loader", async (t) => {
  const checks = await loadChecks(TUI4, SLUGS);
  t.diagnostic(`${checks.length} TUI checks in ${TUI4}`);
  assert.deepEqual(
    checks.map((loaded) => loaded.id),
    ["N-T1", "N-T2", "N-T3"],
  );
  for (const loaded of checks) {
    assert.equal(typeof loaded.run, "function", `${loaded.id}: run is a function`);
    assert.equal(loaded.run.constructor.name, "AsyncFunction", `${loaded.id}: run is async`);
    assert.ok(loaded.op.every((op) => /^OP-[1-9][0-9]*$/.test(op)), `${loaded.id}: OP rows`);
  }
});
