import assert from "node:assert/strict";
import * as kit from "./trial-kit.mjs";
import { CHECKS, loadChecks, plan } from "./trial4-checks.mjs";
import { TRIAL3_STEPS } from "./trial3-steps.mjs";

assert(["live", "fixture"].includes(kit.MODE), "TRIAL3_MODE is live or fixture");
const checks = await loadChecks(CHECKS, TRIAL3_STEPS.map((step) => step.slug));
const checkKit = { ...kit, screen: kit.proof };

/** Runs one trial 4 check as a step of its own, so a failure is recorded and the next test
 * still runs. Input: the check. Output: its detail; throws when it failed or returned none. */
async function verify(check) {
  const detail = await check.run(checkKit);
  assert(typeof detail === "string" && detail.length > 0, `${check.id} returns its detail as text`);
  return detail;
}

describe(`CrazyAgent trial 4: trial 3 plus ${checks.length} owner problem checks (${kit.MODE})`, () => {
  for (const { slug, step, check } of plan(TRIAL3_STEPS, checks)) {
    if (step) {
      it(step.it, () => kit.step(slug, step.title, step.why, step.action));
    } else {
      const title = `${check.id} (${check.op.join(", ")}) ${check.title}`;
      it(title, () => kit.step(slug, title, check.why, () => verify(check)));
    }
  }
});
